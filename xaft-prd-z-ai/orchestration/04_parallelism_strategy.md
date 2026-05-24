# Parallelism Strategy

> How xauft exploits parallelism at multiple levels: parallel tool calls,
> `SubagentPool`, `BestOfNProvider`, `ConsensusProvider`, concurrency limits,
> result merging, and cost implications.

---

## 1. Overview

xauft employs parallelism at **four distinct levels**, each with different
trade-offs between latency, cost, and correctness:

```
┌─────────────────────────────────────────────────────────────┐
│                    Parallelism Levels                        │
│                                                             │
│  Level 4: ConsensusProvider   (multi-model voting)          │
│  ─────────────────────────────────────────────────────      │
│  Level 3: BestOfNProvider     (parallel sampling + judge)   │
│  ─────────────────────────────────────────────────────      │
│  Level 2: SubagentPool        (concurrent sub-agents)       │
│  ─────────────────────────────────────────────────────      │
│  Level 1: Parallel Tool Calls (within a single agent)       │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

Each level is independently configurable and composable.

---

## 2. Level 1: Parallel Tool Calls

### 2.1 Mechanism

When an LLM response includes multiple tool calls, xauft can execute them
**in parallel** rather than sequentially. This is controlled by the
`parallel_tool_calls` flag.

```
  Agent makes 3 tool calls in one response:
  ┌──────────────────────────────────────┐
  │  Tool Call 1: read_file("a.rs")      │
  │  Tool Call 2: read_file("b.rs")      │
  │  Tool Call 3: search("TODO")         │
  └──────────────┬───────────────────────┘
                 │
     ┌───────────┼───────────┐
     │           │           │
     ▼           ▼           ▼
  ┌──────┐  ┌──────┐  ┌──────┐
  │ T1   │  │ T2   │  │ T3   │   ← parallel execution
  │read  │  │read  │  │search│
  │a.rs  │  │b.rs  │  │TODO  │
  └──┬───┘  └──┬───┘  └──┬───┘
     │         │         │
     └─────────┼─────────┘
               │
               ▼
  ┌──────────────────────────────────────┐
  │  Results collected → next LLM call   │
  └──────────────────────────────────────┘
```

### 2.2 Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelToolCallConfig {
    /// Whether to execute tool calls in parallel.
    pub enabled: bool,
    /// Maximum number of concurrent tool calls.
    pub max_concurrent: usize,
    /// Tool names that are safe to parallelize (read-only / side-effect-free).
    pub safe_tools: Vec<String>,
    /// Tool names that must be serialized (have side effects).
    pub serial_tools: Vec<String>,
    /// Timeout for individual tool calls in parallel mode.
    pub per_call_timeout: Duration,
}
```

### 2.3 Safety Classification

Not all tools can be safely parallelized. xauft classifies tools into
categories:

| Category       | Parallelizable? | Examples                              |
|----------------|:---------------:|---------------------------------------|
| Read-only      | ✅ Yes          | `read_file`, `search`, `glob`, `grep` |
| Idempotent     | ✅ Yes          | `list_dir`, `git_status`              |
| Write (file)   | ⚠️ Conditional  | `write_file` (if different files)     |
| Write (shared) | ❌ No           | `write_file` (same file), `shell`     |
| Stateful       | ❌ No           | `shell`, `npm_install`, `git_commit`  |
| External API   | ⚠️ Conditional  | `web_search` (yes), `deploy` (no)     |

### 2.4 Implementation

```rust
pub struct ToolCallExecutor {
    config: ParallelToolCallConfig,
    semaphore: Arc<Semaphore>,
}

impl ToolCallExecutor {
    /// Execute a batch of tool calls, parallelizing where safe.
    pub async fn execute_batch(
        &self,
        calls: Vec<ToolCall>,
        tools: &ToolRegistry,
    ) -> Vec<ToolResult> {
        if !self.config.enabled || calls.len() == 1 {
            // Sequential execution
            return self.execute_sequential(calls, tools).await;
        }

        // Classify calls into parallelizable and serial groups
        let (parallel, serial) = self.classify_calls(calls);

        // Detect write conflicts within parallel group
        let (safe_parallel, conflicted) = self.detect_write_conflicts(parallel);

        // Execute safe parallel calls + conflicted serial calls
        let mut results = Vec::new();

        // Phase 1: Execute parallelizable calls concurrently
        let parallel_results = self.execute_parallel(safe_parallel, tools).await;
        results.extend(parallel_results);

        // Phase 2: Execute serial calls sequentially (including conflicted)
        let serial_results = self.execute_sequential(
            serial.into_iter().chain(conflicted).collect(),
            tools,
        ).await;
        results.extend(serial_results);

        // Sort results by original call order
        results.sort_by_key(|r| r.call_index);
        results
    }

    fn classify_calls(&self, calls: Vec<ToolCall>) -> (Vec<ToolCall>, Vec<ToolCall>) {
        calls.into_iter().partition(|call| {
            self.config.safe_tools.contains(&call.name)
                && !self.config.serial_tools.contains(&call.name)
        })
    }

    fn detect_write_conflicts(
        &self,
        calls: Vec<ToolCall>,
    ) -> (Vec<ToolCall>, Vec<ToolCall>) {
        let mut file_targets: HashMap<PathBuf, usize> = HashMap::new();
        let mut conflicted_indices = HashSet::new();

        for (i, call) in calls.iter().enumerate() {
            if call.name == "write_file" || call.name == "edit_file" {
                if let Some(path) = call.input.get("path").and_then(|v| v.as_str()) {
                    let path = PathBuf::from(path);
                    if let Some(prev_idx) = file_targets.insert(path, i) {
                        // Both the current and previous call conflict
                        conflicted_indices.insert(prev_idx);
                        conflicted_indices.insert(i);
                    }
                }
            }
        }

        calls.into_iter().enumerate().partition(|(i, _)| {
            !conflicted_indices.contains(i)
        }).0.into_iter().map(|(_, c)| c).zip(
            calls.into_iter().enumerate().filter(|(i, _)| {
                conflicted_indices.contains(i)
            }).map(|(_, c)| c).collect::<Vec<_>>()
        )
        // Simplified: return (safe, conflicted)
    }

    async fn execute_parallel(
        &self,
        calls: Vec<ToolCall>,
        tools: &ToolRegistry,
    ) -> Vec<ToolResult> {
        let mut join_set = JoinSet::new();

        for (i, call) in calls.into_iter().enumerate() {
            let tools = tools.clone();
            let semaphore = self.semaphore.clone();
            let timeout = self.config.per_call_timeout;
            join_set.spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();
                let result = tokio::time::timeout(
                    timeout,
                    tools.execute(&call.name, call.input.clone()),
                ).await;
                ToolResult {
                    call_index: i,
                    call_id: call.id,
                    output: match result {
                        Ok(Ok(output)) => Ok(output),
                        Ok(Err(e)) => Err(ToolError::ExecutionFailed(e.to_string())),
                        Err(_) => Err(ToolError::Timeout(timeout)),
                    },
                }
            });
        }

        let mut results = Vec::new();
        while let Some(res) = join_set.join_next().await {
            results.push(res.unwrap());
        }
        results
    }
}
```

---

## 3. Level 2: SubagentPool — Concurrent Sub-Agent Execution

### 3.1 Architecture

The `SubagentPool` manages a pool of pre-initialized agents that can execute
independent tasks concurrently.

```
                       ┌─────────────────────────┐
                       │     SubagentPool         │
                       │                         │
  Task T1 ────────────▶│  ┌─────┐  ┌─────┐     │──────────▶ Result R1
                       │  │ A1  │  │ A2  │     │
  Task T2 ────────────▶│  │(busy)│  │(idle)│    │──────────▶ Result R2
                       │  └─────┘  └─────┘     │
  Task T3 ────────────▶│  ┌─────┐  ┌─────┐     │──────────▶ Result R3
  (waits for slot)     │  │ A3  │  │ A4  │     │
                       │  │(busy)│  │(busy)│    │
                       │  └─────┘  └─────┘     │
                       │                         │
                       │  Semaphore: 4 permits   │
                       │  Available: 0           │
                       └─────────────────────────┘
```

### 3.2 Concurrency Control

```rust
pub struct ConcurrencyConfig {
    /// Maximum concurrent agents in the pool.
    pub max_agents: usize,
    /// Maximum concurrent tasks (may be > max_agents with queuing).
    pub max_tasks: usize,
    /// Task queue capacity (tasks waiting for an agent).
    pub queue_capacity: usize,
    /// Timeout for acquiring an agent from the pool.
    pub acquire_timeout: Duration,
    /// Whether to create agents on-demand beyond pre-warmed count.
    pub elastic: bool,
    /// Maximum elastic agents (beyond pre-warmed).
    pub max_elastic: usize,
    /// Idle timeout for elastic agents.
    pub elastic_idle_timeout: Duration,
}
```

### 3.3 Result Merging

When multiple sub-agents produce results, they must be merged into a coherent
whole. The merge strategy depends on the task type:

```rust
pub enum MergeStrategy {
    /// Simply collect all results in order. No merging.
    Collect,

    /// Merge file modifications, detecting conflicts.
    FileMerge { conflict_resolution: ConflictResolution },

    /// Combine search results, deduplicating.
    SearchMerge { dedup_by: DeduplicationKey },

    /// Combine code review issues, sorted by severity.
    ReviewMerge { sort_by: IssueSortKey },

    /// Let an LLM synthesise the results.
    LlmSynthesis { model: String },
}

#[derive(Debug, Clone)]
pub enum ConflictResolution {
    /// Fail if there are conflicts.
    Fail,
    /// Last writer wins.
    LastWriteWins,
    /// Use a 3-way merge algorithm.
    ThreeWayMerge,
    /// Let an LLM resolve the conflict.
    LlmResolution { model: String },
}

impl MergeStrategy {
    pub async fn merge(&self, results: Vec<StepResult>) -> Result<StepResult, MergeError> {
        match self {
            Self::Collect => Ok(StepResult::Multi(results)),

            Self::FileMerge { conflict_resolution } => {
                let mut merged_changes: HashMap<PathBuf, FileChange> = HashMap::new();
                let mut conflicts = Vec::new();

                for result in &results {
                    if let StepResult::FileChanges(changes) = result {
                        for change in changes {
                            match merged_changes.entry(change.path.clone()) {
                                Entry::Vacant(e) => { e.insert(change.clone()); }
                                Entry::Occupied(e) => {
                                    conflicts.push(FileConflict {
                                        path: change.path.clone(),
                                        existing: e.get().clone(),
                                        incoming: change.clone(),
                                    });
                                }
                            }
                        }
                    }
                }

                if conflicts.is_empty() {
                    Ok(StepResult::FileChanges(merged_changes.into_values().collect()))
                } else {
                    match conflict_resolution {
                        ConflictResolution::Fail => {
                            Err(MergeError::Conflicts(conflicts))
                        }
                        ConflictResolution::LastWriteWins => {
                            for conflict in conflicts {
                                merged_changes.insert(
                                    conflict.path,
                                    conflict.incoming,
                                );
                            }
                            Ok(StepResult::FileChanges(
                                merged_changes.into_values().collect()
                            ))
                        }
                        ConflictResolution::ThreeWayMerge => {
                            // Apply diff3 algorithm
                            self.three_way_merge(merged_changes, conflicts).await
                        }
                        ConflictResolution::LlmResolution { model } => {
                            self.llm_resolve(merged_changes, conflicts, model).await
                        }
                    }
                }
            }

            Self::SearchMerge { dedup_by } => {
                // Deduplicate search results
                let mut seen = HashSet::new();
                let mut deduped = Vec::new();
                for result in results {
                    if let StepResult::SearchResults(items) = result {
                        for item in items {
                            let key = match dedup_by {
                                DeduplicationKey::FilePath => {
                                    item.file_path.clone()
                                }
                                DeduplicationKey::Content => {
                                    format!("{:x}", md5::compute(&item.content))
                                }
                            };
                            if seen.insert(key) {
                                deduped.push(item);
                            }
                        }
                    }
                }
                Ok(StepResult::SearchResults(deduped))
            }

            Self::ReviewMerge { sort_by } => {
                let mut all_issues = Vec::new();
                for result in results {
                    if let StepResult::ReviewIssues(issues) = result {
                        all_issues.extend(issues);
                    }
                }
                all_issues.sort_by(|a, b| match sort_by {
                    IssueSortKey::Severity => b.severity.cmp(&a.severity),
                    IssueSortKey::File => a.file.cmp(&b.file),
                    IssueSortKey::Line => a.line.cmp(&b.line),
                });
                Ok(StepResult::ReviewIssues(all_issues))
            }

            Self::LlmSynthesis { model } => {
                // Use LLM to merge results (expensive but high quality)
                let combined = serde_json::to_value(&results)?;
                let synthesised = self.synthesise_with_llm(model, &combined).await?;
                Ok(StepResult::Synthesised(synthesised))
            }
        }
    }
}
```

---

## 4. Level 3: BestOfNProvider — Parallel Sampling

### 4.1 Concept

`BestOfNProvider` generates N completions in parallel, then uses a **judge**
to select the best one. This improves output quality at the cost of N×
token usage.

```
                    ┌─────────────────┐
                    │   User Prompt   │
                    └────────┬────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
              ▼              ▼              ▼
        ┌──────────┐  ┌──────────┐  ┌──────────┐
        │ Sample 1 │  │ Sample 2 │  │ Sample 3 │
        │ (N=3)    │  │ (N=3)    │  │ (N=3)    │
        └────┬─────┘  └────┬─────┘  └────┬─────┘
             │              │              │
             └──────────────┼──────────────┘
                            │
                            ▼
                    ┌───────────────┐
                    │     Judge     │
                    │  (LLM-based)  │
                    └───────┬───────┘
                            │
                    ┌───────▼───────┐
                    │  Best Result  │
                    │  (Sample 2)   │
                    └───────────────┘
```

### 4.2 Implementation

```rust
pub struct BestOfNProvider<P: LlmProvider> {
    inner: P,
    /// Number of parallel samples.
    n: usize,
    /// Judge for selecting the best sample.
    judge: Arc<dyn ResponseJudge>,
    /// Configuration.
    config: BestOfNConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BestOfNConfig {
    /// Number of samples (N).
    pub n: usize,
    /// Temperature for sampling (higher = more diversity).
    pub temperature: f64,
    /// Maximum tokens per sample.
    pub max_tokens_per_sample: usize,
    /// Judge model.
    pub judge_model: String,
    /// Judge temperature (usually 0 for consistency).
    pub judge_temperature: f64,
    /// Whether to include all samples in the event for debugging.
    pub keep_all_samples: bool,
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for BestOfNProvider<P> {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        // 1. Generate N samples in parallel
        let mut sample_requests = Vec::with_capacity(self.n);
        for i in 0..self.n {
            let mut req = request.clone();
            // Add variation via temperature
            req.temperature = Some(self.config.temperature);
            req.max_tokens = self.config.max_tokens_per_sample;
            sample_requests.push(req);
        }

        let samples: Vec<LlmResponse> = futures::future::join_all(
            sample_requests.into_iter().map(|req| self.inner.complete(req))
        ).await.into_iter()
            .filter_map(|r| r.ok())
            .collect();

        if samples.is_empty() {
            return Err(ProviderError::AllSamplesFailed);
        }

        if samples.len() == 1 {
            return Ok(samples.into_iter().next().unwrap());
        }

        // 2. Judge selects the best
        let best_index = self.judge.select_best(
            &request.messages,
            &samples,
            &self.config.judge_model,
        ).await?;

        let best = samples.into_iter().nth(best_index)
            .ok_or(ProviderError::InvalidJudgeIndex(best_index))?;

        Ok(best)
    }
}
```

### 4.3 ResponseJudge Implementations

```rust
/// Trait for judging which of N samples is best.
#[async_trait]
pub trait ResponseJudge: Send + Sync {
    /// Select the index of the best sample.
    async fn select_best(
        &self,
        prompt: &[Message],
        samples: &[LlmResponse],
        model: &str,
    ) -> Result<usize, JudgeError>;
}

/// Judge that uses an LLM to evaluate and rank samples.
pub struct LlmJudge<P: LlmProvider> {
    provider: P,
}

#[async_trait]
impl<P: LlmProvider> ResponseJudge for LlmJudge<P> {
    async fn select_best(
        &self,
        prompt: &[Message],
        samples: &[LlmResponse],
        model: &str,
    ) -> Result<usize, JudgeError> {
        let samples_text = samples.iter().enumerate()
            .map(|(i, s)| format!("--- Sample {} ---\n{}", i, s.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let judge_prompt = format!(
            "You are a judge evaluating {n} code generation samples.\n\
             Original prompt: {prompt}\n\n\
             {samples}\n\n\
             Which sample (0-indexed) is the BEST? Consider:\n\
             1. Correctness: Does it solve the stated problem?\n\
             2. Code quality: Clean, idiomatic, well-structured?\n\
             3. Completeness: Does it handle edge cases?\n\
             4. Safety: No security issues or dangerous operations?\n\n\
             Respond with ONLY the sample index number.",
            n = samples.len(),
            prompt = prompt.last().map(|m| m.content()).unwrap_or("N/A"),
            samples = samples_text,
        );

        let response = self.provider.complete(LlmRequest {
            model: model.to_string(),
            messages: vec![Message::user(&judge_prompt)],
            temperature: Some(0.0),
            max_tokens: 10,
            ..Default::default()
        }).await?;

        let index: usize = response.content.trim().parse()
            .map_err(|_| JudgeError::InvalidResponse(response.content))?;

        if index >= samples.len() {
            return Err(JudgeError::IndexOutOfRange {
                index,
                max: samples.len(),
            });
        }

        Ok(index)
    }
}

/// Judge that uses heuristic scoring (fast, no LLM cost).
pub struct HeuristicJudge {
    weights: JudgeWeights,
}

#[derive(Debug, Clone)]
pub struct JudgeWeights {
    pub length_penalty: f64,       // penalize overly long outputs
    pub code_block_bonus: f64,     // bonus for well-formatted code
    pub error_mention_penalty: f64, // penalize if sample mentions errors
    pub completeness_bonus: f64,   // bonus for covering all requirements
}

#[async_trait]
impl ResponseJudge for HeuristicJudge {
    async fn select_best(
        &self,
        prompt: &[Message],
        samples: &[LlmResponse],
        _model: &str,
    ) -> Result<usize, JudgeError> {
        let scores: Vec<f64> = samples.iter().map(|s| {
            let mut score = 0.0;

            // Length penalty: prefer concise outputs
            let len = s.content.len() as f64;
            score -= self.weights.length_penalty * (len / 1000.0);

            // Code block bonus
            let code_blocks = s.content.matches("```").count() / 2;
            score += self.weights.code_block_bonus * code_blocks as f64;

            // Error mention penalty
            if s.content.contains("error") || s.content.contains("Error") {
                score -= self.weights.error_mention_penalty;
            }

            // Finish reason bonus (completed > length-limited)
            if s.finish_reason == Some(FinishReason::Stop) {
                score += 0.5;
            }

            score
        }).collect();

        let best_index = scores.iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        Ok(best_index)
    }
}
```

---

## 5. Level 4: ConsensusProvider — Multi-Model Voting

### 5.1 Concept

`ConsensusProvider` queries multiple LLM providers in parallel and uses a
**voting mechanism** to reach consensus. This provides robustness against
individual model errors or biases.

```
                    ┌─────────────────┐
                    │   User Prompt   │
                    └────────┬────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
              ▼              ▼              ▼
        ┌──────────┐  ┌──────────┐  ┌──────────┐
        │ OpenAI   │  │Anthropic │  │  Gemini  │
        │ GPT-4o   │  │Claude 3.5│  │  Pro 1.5 │
        └────┬─────┘  └────┬─────┘  └────┬─────┘
             │              │              │
             └──────────────┼──────────────┘
                            │
                            ▼
                    ┌───────────────┐
                    │   Consensus   │
                    │   Voter       │
                    │               │
                    │ 2/3 agree    │──────▶ Consensus result
                    │  on answer A  │
                    └───────────────┘
```

### 5.2 Implementation

```rust
pub struct ConsensusProvider {
    /// Multiple providers to query.
    providers: Vec<Arc<dyn LlmProvider>>,
    /// Consensus strategy.
    strategy: ConsensusStrategy,
    /// Configuration.
    config: ConsensusConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusConfig {
    /// Minimum number of agreeing providers (quorum).
    pub quorum: usize,
    /// Timeout for each provider.
    pub per_provider_timeout: Duration,
    /// Whether to fall back to majority vote if no quorum.
    pub majority_fallback: bool,
    /// Whether to use an LLM judge for semantic consensus.
    pub semantic_consensus: bool,
    /// Judge model for semantic consensus.
    pub judge_model: String,
}

#[derive(Debug, Clone)]
pub enum ConsensusStrategy {
    /// Exact string match between provider outputs.
    ExactMatch,
    /// Normalized match (whitespace, case insensitive).
    NormalizedMatch,
    /// Semantic similarity above a threshold.
    SemanticSimilarity { threshold: f64 },
    /// LLM judge determines if outputs agree.
    LlmJudged,
    /// Structured output field matching (specific fields must agree).
    StructuredMatch { fields: Vec<String> },
}

#[async_trait]
impl LlmProvider for ConsensusProvider {
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        // 1. Query all providers in parallel
        let results: Vec<(usize, Result<LlmResponse, ProviderError>)> =
            futures::future::join_all(
                self.providers.iter().enumerate().map(|(i, provider)| {
                    let req = request.clone();
                    let timeout = self.config.per_provider_timeout;
                    async move {
                        let result = tokio::time::timeout(
                            timeout,
                            provider.complete(req),
                        ).await;
                        (i, result.map_err(|_| ProviderError::Timeout(timeout)).and_then(|r| r))
                    }
                })
            ).await;

        // 2. Collect successful responses
        let responses: Vec<(usize, LlmResponse)> = results.into_iter()
            .filter_map(|(i, r)| r.ok().map(|resp| (i, resp)))
            .collect();

        if responses.is_empty() {
            return Err(ProviderError::AllProvidersFailed);
        }

        // 3. Apply consensus strategy
        match &self.strategy {
            ConsensusStrategy::ExactMatch => {
                self.exact_match_consensus(responses)
            }
            ConsensusStrategy::NormalizedMatch => {
                self.normalized_consensus(responses)
            }
            ConsensusStrategy::SemanticSimilarity { threshold } => {
                self.semantic_consensus(responses, *threshold).await
            }
            ConsensusStrategy::LlmJudged => {
                self.llm_judged_consensus(responses).await
            }
            ConsensusStrategy::StructuredMatch { fields } => {
                self.structured_consensus(responses, fields)
            }
        }
    }
}

impl ConsensusProvider {
    fn exact_match_consensus(
        &self,
        responses: Vec<(usize, LlmResponse)>,
    ) -> Result<LlmResponse, ProviderError> {
        // Count occurrences of each unique response
        let mut counts: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, resp) in &responses {
            counts
                .entry(resp.content.clone())
                .or_default()
                .push(*i);
        }

        // Find the most common response
        let (most_common, indices) = counts.into_iter()
            .max_by_key(|(_, v)| v.len())
            .ok_or(ProviderError::NoConsensus)?;

        if indices.len() >= self.config.quorum {
            // Return the first response with the consensus content
            let first_idx = indices[0];
            Ok(responses.into_iter()
                .nth(first_idx)
                .map(|(_, r)| r)
                .unwrap())
        } else if self.config.majority_fallback {
            // Fall back to majority vote
            Ok(responses.into_iter()
                .nth(indices[0])
                .map(|(_, r)| r)
                .unwrap())
        } else {
            Err(ProviderError::NoQuorum {
                quorum: self.config.quorum,
                agreement: indices.len(),
            })
        }
    }
}
```

### 5.3 Consensus Event

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ConsensusEvent {
    /// Providers that were queried.
    pub providers_queried: Vec<String>,
    /// Responses received (indexed by provider).
    pub responses: HashMap<String, String>,
    /// Whether consensus was reached.
    pub consensus_reached: bool,
    /// The consensus value (if reached).
    pub consensus_value: Option<String>,
    /// Agreement level (e.g., "2/3").
    pub agreement_level: String,
    /// Strategy used.
    pub strategy: String,
    /// Duration.
    pub duration: Duration,
}
```

---

## 6. Parallelism Decision Engine

### 6.1 How xauft Decides What to Parallelize

xauft uses a **parallelism decision engine** that evaluates each potential
parallelization opportunity:

```rust
pub struct ParallelismDecisionEngine {
    config: ParallelismConfig,
    token_budget: Arc<TokenBudget>,
    latency_target: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelismConfig {
    /// Enable parallel tool calls.
    pub parallel_tool_calls: bool,
    /// Enable sub-agent parallelism.
    pub parallel_subagents: bool,
    /// Enable BestOfN sampling.
    pub best_of_n_enabled: bool,
    /// Enable consensus voting.
    pub consensus_enabled: bool,
    /// Maximum total concurrent LLM calls across all levels.
    pub max_total_concurrency: usize,
    /// Cost multiplier threshold (don't parallelize if cost > threshold).
    pub max_cost_multiplier: f64,
}

impl ParallelismDecisionEngine {
    /// Decide whether to parallelize tool calls.
    pub fn should_parallelize_tools(&self, calls: &[ToolCall]) -> bool {
        if !self.config.parallel_tool_calls || calls.len() <= 1 {
            return false;
        }
        // All calls must be safe to parallelize
        calls.iter().all(|c| self.is_safe_to_parallelize(&c.name))
    }

    /// Decide the N for BestOfN sampling.
    pub fn best_of_n(&self, task: &Task) -> usize {
        if !self.config.best_of_n_enabled {
            return 1;
        }
        match task.complexity() {
            Complexity::Simple => 1,
            Complexity::Decomposable => 1,
            Complexity::Sequential => 2,  // moderate benefit
            Complexity::Exploratory => 3, // high benefit from diversity
        }
    }

    /// Decide whether to use consensus for this request.
    pub fn should_use_consensus(&self, request: &LlmRequest) -> bool {
        if !self.config.consensus_enabled {
            return false;
        }
        // Use consensus for high-stakes decisions
        request.metadata.get("stakes")
            .and_then(|v| v.as_str())
            .map(|s| s == "high" || s == "critical")
            .unwrap_or(false)
    }

    /// Check if the token budget allows parallelism.
    pub fn budget_allows(&self, multiplier: usize) -> bool {
        let remaining = self.token_budget.remaining();
        let single_cost = self.token_budget.estimated_single_call_cost();
        remaining >= (single_cost * multiplier as u64)
    }

    fn is_safe_to_parallelize(&self, tool_name: &str) -> bool {
        matches!(
            tool_name,
            "read_file" | "search" | "glob" | "grep" |
            "list_dir" | "git_status" | "git_log" |
            "web_search" | "code_review"
        )
    }
}
```

### 6.2 Decision Matrix

| Scenario                        | Tool Calls | Subagents | BestOfN | Consensus |
|---------------------------------|:----------:|:---------:|:-------:|:---------:|
| Simple file read                | ✅ parallel| —         | —       | —         |
| Multi-file edit                 | ❌ serial  | —         | —       | —         |
| Large refactoring               | —          | ✅ 3 pool | —       | —         |
| Critical security fix           | —          | —         | ✅ N=3  | ✅ 3 models|
| Exploratory design              | —          | —         | ✅ N=3  | —         |
| Code review (multiple files)    | ✅ parallel| ✅ 3 pool | —       | —         |
| Production deployment script    | ❌ serial  | —         | ✅ N=3  | ✅ 3 models|
| Simple question answering       | —          | —         | —       | —         |

---

## 7. Concurrency Limits

### 7.1 Global Semaphore

xauft uses a **global semaphore** to limit the total number of concurrent
LLM API calls across all parallelism levels:

```rust
pub struct GlobalConcurrencyLimiter {
    /// Semaphore for LLM API calls.
    llm_semaphore: Arc<Semaphore>,
    /// Semaphore for tool execution.
    tool_semaphore: Arc<Semaphore>,
    /// Semaphore for sub-agent execution.
    agent_semaphore: Arc<Semaphore>,
    /// Current counts for monitoring.
    counters: Arc<ConcurrencyCounters>,
}

#[derive(Debug, Default)]
pub struct ConcurrencyCounters {
    active_llm_calls: AtomicUsize,
    active_tool_calls: AtomicUsize,
    active_agents: AtomicUsize,
    total_llm_calls: AtomicU64,
    total_tool_calls: AtomicU64,
}

impl GlobalConcurrencyLimiter {
    pub fn new(config: &ConcurrencyLimits) -> Self {
        Self {
            llm_semaphore: Arc::new(Semaphore::new(config.max_concurrent_llm_calls)),
            tool_semaphore: Arc::new(Semaphore::new(config.max_concurrent_tool_calls)),
            agent_semaphore: Arc::new(Semaphore::new(config.max_concurrent_agents)),
            counters: Arc::new(ConcurrencyCounters::default()),
        }
    }

    pub async fn acquire_llm(&self) -> ConcurrencyPermit {
        let permit = self.llm_semaphore.acquire().await.unwrap();
        self.counters.active_llm_calls.fetch_add(1, Ordering::Relaxed);
        self.counters.total_llm_calls.fetch_add(1, Ordering::Relaxed);
        ConcurrencyPermit {
            permit: Some(permit),
            counter: &self.counters.active_llm_calls,
        }
    }

    pub async fn acquire_tool(&self) -> ConcurrencyPermit {
        let permit = self.tool_semaphore.acquire().await.unwrap();
        self.counters.active_tool_calls.fetch_add(1, Ordering::Relaxed);
        self.counters.total_tool_calls.fetch_add(1, Ordering::Relaxed);
        ConcurrencyPermit {
            permit: Some(permit),
            counter: &self.counters.active_tool_calls,
        }
    }
}

pub struct ConcurrencyPermit<'a> {
    permit: Option<SemaphorePermit<'a>>,
    counter: &'a AtomicUsize,
}

impl<'a> Drop for ConcurrencyPermit<'a> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
        self.permit.take();
    }
}
```

### 7.2 Concurrency Limits Configuration

```toml
[xaft.concurrency]
max_concurrent_llm_calls = 10     # global LLM API call limit
max_concurrent_tool_calls = 20    # global tool execution limit
max_concurrent_agents = 5         # max parallel sub-agents
```

---

## 8. Cost Implications

### 8.1 Cost Model

Each parallelism level has different cost implications:

| Level             | Token Multiplier | Latency Impact    | Quality Impact        |
|-------------------|:----------------:|:-----------------:|:---------------------:|
| Parallel tools    | 1× (same tokens) | ↓↓ (much faster)  | Neutral               |
| SubagentPool      | N× (N agents)    | ↓ (faster)        | ↑ (specialization)    |
| BestOfN (N=3)     | N+0.1× (N+judge) | — (same, parallel)| ↑↑ (best selection)  |
| Consensus (3 mod) | 3+0.1×           | — (same, parallel)| ↑↑ (agreement)       |

### 8.2 Budget Enforcement

```rust
pub struct TokenBudget {
    /// Total budget in tokens.
    total: AtomicU64,
    /// Tokens consumed so far.
    consumed: AtomicU64,
    /// Warning threshold (percentage).
    warn_at_percent: u8,
    /// Hard stop threshold (percentage).
    stop_at_percent: u8,
}

impl TokenBudget {
    /// Check if a request is within budget.
    pub fn allows(&self, estimated_tokens: u64) -> bool {
        let consumed = self.consumed.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        consumed + estimated_tokens <= total
    }

    /// Record token consumption.
    pub fn record(&self, tokens: u64) -> BudgetStatus {
        let consumed = self.consumed.fetch_add(tokens, Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let percent = ((consumed + tokens) * 100 / total) as u8;

        match percent {
            p if p >= self.stop_at_percent => BudgetStatus::Exceeded,
            p if p >= self.warn_at_percent => BudgetStatus::Warning {
                percent: p,
                remaining: total.saturating_sub(consumed + tokens),
            },
            _ => BudgetStatus::Ok,
        }
    }
}

#[derive(Debug, Clone)]
pub enum BudgetStatus {
    Ok,
    Warning { percent: u8, remaining: u64 },
    Exceeded,
}
```

### 8.3 Cost-Aware Parallelism

xauft automatically reduces parallelism when the token budget is low:

```rust
impl ParallelismDecisionEngine {
    pub fn effective_n(&self, base_n: usize) -> usize {
        let budget_status = self.token_budget.status();
        match budget_status {
            BudgetStatus::Ok => base_n,
            BudgetStatus::Warning { percent, .. } if percent > 80 => {
                (base_n * 2 / 3).max(1)
            }
            BudgetStatus::Warning { percent, .. } if percent > 50 => {
                (base_n + 1) / 2
            }
            BudgetStatus::Exceeded => 1, // no parallelism
            _ => base_n,
        }
    }
}
```

---

## 9. Architecture Diagram: Full Parallelism Stack

```
┌─────────────────────────────────────────────────────────────────┐
│                        xauft Session                             │
│                                                                 │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                   GlobalConcurrencyLimiter                  │ │
│  │   LLM Semaphore (10)  │  Tool Semaphore (20)  │  Agent(5) │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                 │
│  ┌──────────────────────┐  ┌──────────────────────────────────┐│
│  │   TaskRunner         │  │  Provider Stack                  ││
│  │                      │  │                                  ││
│  │  ┌─────────────┐    │  │  ┌────────────────────────────┐  ││
│  │  │ Planner     │    │  │  │ ConsensusProvider          │  ││
│  │  └──────┬──────┘    │  │  │  ┌────────┐┌────────┐     │  ││
│  │         │           │  │  │  │OpenAI  ││Anthropic│     │  ││
│  │  ┌──────▼──────┐    │  │  │  └────────┘└────────┘     │  ││
│  │  │ Step Exec   │    │  │  └────────────────────────────┘  ││
│  │  │  │          │    │  │  ┌────────────────────────────┐  ││
│  │  │  ├─ToolExec │    │  │  │ BestOfNProvider            │  ││
│  │  │  │  ├─par1  │    │  │  │  Sample1│Sample2│Sample3   │  ││
│  │  │  │  ├─par2  │    │  │  │  ───────Judge───────       │  ││
│  │  │  │  └─par3  │    │  │  └────────────────────────────┘  ││
│  │  │  │          │    │  │  ┌────────────────────────────┐  ││
│  │  │  └─SubAgent │    │  │  │ FallbackProvider            │  ││
│  │  │     Pool    │    │  │  │  Primary → Fallback1 → F2   │  ││
│  │  │  ┌───┐┌───┐ │    │  │  └────────────────────────────┘  ││
│  │  │  │ A ││ B │ │    │  │  ┌────────────────────────────┐  ││
│  │  │  └───┘└───┘ │    │  │  │ CostedProvider              │  ││
│  │  └─────────────┘    │  │  │  Route by cost predicate     │  ││
│  │                      │  │  └────────────────────────────┘  ││
│  └──────────────────────┘  └──────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

---

## 10. Configuration Reference

```toml
[xaft.parallelism]

# Level 1: Parallel tool calls
[xaft.parallelism.tool_calls]
enabled = true
max_concurrent = 5
safe_tools = ["read_file", "search", "glob", "grep", "list_dir"]
serial_tools = ["write_file", "shell", "git_commit"]
per_call_timeout_secs = 30

# Level 2: Sub-agent pool
[xaft.parallelism.subagents]
enabled = true
max_agents = 5
max_tasks = 10
queue_capacity = 20
acquire_timeout_secs = 60
elastic = true
max_elastic = 3
elastic_idle_timeout_secs = 300

# Level 3: BestOfN
[xauft.parallelism.best_of_n]
enabled = false
default_n = 3
temperature = 0.7
judge_model = "gpt-4o-mini"
keep_all_samples = false

# Level 4: Consensus
[xaft.parallelism.consensus]
enabled = false
quorum = 2
per_provider_timeout_secs = 60
majority_fallback = true
strategy = "exact_match"     # "exact_match" | "normalized" | "semantic" | "llm_judged"

# Concurrency limits
[xaft.concurrency]
max_concurrent_llm_calls = 10
max_concurrent_tool_calls = 20
max_concurrent_agents = 5

# Budget
[xaft.budget]
total_tokens = 1_000_000
warn_at_percent = 75
stop_at_percent = 95
```
