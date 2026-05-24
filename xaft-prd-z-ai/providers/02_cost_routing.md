# Cost Routing and Provider Composition

> How xauft routes requests through `CostedProvider`, `FallbackProvider`,
> `BestOfNProvider`, and `ConsensusProvider`. Predicate-based routing,
> rule priority, streaming modes, sampling + judging, multi-model voting,
> response judging, and budget enforcement.

---

## 1. Overview

xauft composes multiple `LlmProvider` implementations into a **provider
stack** that handles routing, fallback, quality improvement, and cost
management. Each layer in the stack is itself an `LlmProvider`, enabling
transparent composition.

```
┌─────────────────────────────────────────────────────────────┐
│                     Provider Stack                           │
│                                                             │
│  ┌─────────────────┐                                        │
│  │  CostedProvider  │  ← routes based on cost predicates    │
│  └────────┬────────┘                                        │
│           │                                                 │
│  ┌────────▼────────┐                                        │
│  │ FallbackProvider │  ← retries on failure                 │
│  └────────┬────────┘                                        │
│           │                                                 │
│  ┌────────▼───────────────────────────────────────────┐     │
│  │  BestOfNProvider / ConsensusProvider (optional)    │     │
│  └────────┬───────────────────────────────────────────┘     │
│           │                                                 │
│  ┌────────▼───────────────────────────────────────────┐     │
│  │          Concrete Providers                        │     │
│  │  ┌──────┐  ┌──────────┐  ┌───────┐  ┌────────┐   │     │
│  │  │OpenAI│  │Anthropic │  │Ollama │  │ Gemini │   │     │
│  │  └──────┘  └──────────┘  └───────┘  └────────┘   │     │
│  └────────────────────────────────────────────────────┘     │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. CostedProvider

### 2.1 Concept

`CostedProvider` routes each request to the cheapest provider that satisfies
the request's requirements. It uses **predicate-based routing rules** with
configurable priorities.

```
  Incoming Request
       │
       ▼
  ┌──────────────────┐
  │  Evaluate Rules  │
  │  (by priority)   │
  └────────┬─────────┘
           │
     ┌─────┼─────┐
     │     │     │
     ▼     ▼     ▼
  Rule 1 Rule 2 Rule 3
  P=100  P=50   P=10
     │     │     │
     │     │     └──▶ cheap model (gpt-4o-mini)
     │     └────────▶ mid model (claude-3.5-haiku)
     └──────────────▶ flagship model (gpt-4o)
```

### 2.2 Routing Rules

```rust
pub struct CostedProvider {
    /// Available providers.
    providers: Vec<ProviderRoute>,
    /// Routing rules ordered by priority.
    rules: Vec<RoutingRule>,
    /// Default provider when no rule matches.
    default_provider: String,
}

#[derive(Debug, Clone)]
pub struct ProviderRoute {
    /// Provider instance.
    provider: Arc<dyn LlmProvider>,
    /// Route name.
    name: String,
    /// Cost tier.
    tier: CostTier,
    /// Whether this provider is currently healthy.
    healthy: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CostTier {
    /// Cheapest available (e.g., Ollama, gpt-4o-mini).
    Budget,
    /// Mid-range (e.g., claude-3.5-haiku, gemini-flash).
    Standard,
    /// Most capable (e.g., gpt-4o, claude-3.5-sonnet).
    Flagship,
}

#[derive(Debug, Clone)]
pub struct RoutingRule {
    /// Unique rule name.
    name: String,
    /// Priority (higher = evaluated first).
    priority: i32,
    /// Predicate that must match for this rule to apply.
    predicate: Box<dyn RoutingPredicate + Send + Sync>,
    /// Target provider name.
    target: String,
    /// Whether this rule stops further evaluation.
    terminal: bool,
}

#[async_trait]
pub trait RoutingPredicate: Send + Sync {
    async fn evaluate(&self, request: &LlmRequest) -> bool;
}
```

### 2.3 Built-In Predicates

```rust
/// Route based on system prompt keywords.
pub struct SystemPromptKeywordPredicate {
    keywords: Vec<String>,
    /// If true, match if ANY keyword is present.
    any: bool,
}

#[async_trait]
impl RoutingPredicate for SystemPromptKeywordPredicate {
    async fn evaluate(&self, request: &LlmRequest) -> bool {
        let system = request.system_prompt.as_deref().unwrap_or("");
        if self.any {
            self.keywords.iter().any(|k| system.contains(k))
        } else {
            self.keywords.iter().all(|k| system.contains(k))
        }
    }
}

/// Route based on required features.
pub struct RequiresFeaturePredicate {
    feature: ProviderFeature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFeature {
    ToolCalling,
    Vision,
    StructuredOutput,
    Thinking,
    Streaming,
    LargeContext { min_tokens: usize },
}

#[async_trait]
impl RoutingPredicate for RequiresFeaturePredicate {
    async fn evaluate(&self, request: &LlmRequest) -> bool {
        match self.feature {
            ProviderFeature::ToolCalling => !request.tools.is_empty(),
            ProviderFeature::Vision => request.messages.iter().any(|m| m.has_images()),
            ProviderFeature::StructuredOutput => request.response_format.is_some(),
            ProviderFeature::Thinking => request.metadata.get("thinking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ProviderFeature::Streaming => false, // handled separately
            ProviderFeature::LargeContext { min_tokens } => {
                request.estimate_tokens() > min_tokens
            }
        }
    }
}

/// Route based on token budget remaining.
pub struct BudgetThresholdPredicate {
    /// Minimum remaining budget (in USD cents) to use flagship.
    min_budget_cents: u64,
    budget: Arc<TokenBudget>,
}

#[async_trait]
impl RoutingPredicate for BudgetThresholdPredicate {
    async fn evaluate(&self, _request: &LlmRequest) -> bool {
        let remaining = self.budget.remaining();
        remaining > self.min_budget_cents
    }
}

/// Route based on agent role.
pub struct AgentRolePredicate {
    roles: Vec<AgentRole>,
}

#[async_trait]
impl RoutingPredicate for AgentRolePredicate {
    async fn evaluate(&self, request: &LlmRequest) -> bool {
        request.metadata.get("agent_role")
            .and_then(|v| v.as_str())
            .map(|r| self.roles.iter().any(|role| role.as_str() == r))
            .unwrap_or(false)
    }
}
```

### 2.4 System-Prompt Keyword Routing

xauft uses **system prompt keywords** as a lightweight routing mechanism.
The user's system prompt (or the agent's auto-generated one) includes keywords
that indicate the quality tier needed:

| Keyword            | Target Tier  | Example                                     |
|--------------------|:------------:|---------------------------------------------|
| `@flagship`        | Flagship     | Complex architecture decisions               |
| `@standard`        | Standard     | Code review, test writing                   |
| `@budget`          | Budget       | Simple search, file reads, formatting       |
| `@thinking`        | Flagship     | Problems requiring chain-of-thought         |
| `@vision`          | Vision-capable | Image analysis, UI screenshots            |
| `@local`           | Ollama       | Privacy-sensitive code, offline mode        |

```rust
impl CostedProvider {
    /// Build the default routing rules.
    pub fn default_rules() -> Vec<RoutingRule> {
        vec![
            // @flagship keyword → flagship provider
            RoutingRule {
                name: "flagship_keyword".into(),
                priority: 100,
                predicate: Box::new(SystemPromptKeywordPredicate {
                    keywords: vec!["@flagship".into(), "@thinking".into()],
                    any: true,
                }),
                target: "flagship".into(),
                terminal: true,
            },

            // @local keyword → Ollama
            RoutingRule {
                name: "local_keyword".into(),
                priority: 90,
                predicate: Box::new(SystemPromptKeywordPredicate {
                    keywords: vec!["@local".into()],
                    any: true,
                }),
                target: "ollama".into(),
                terminal: true,
            },

            // Vision required → vision-capable provider
            RoutingRule {
                name: "vision_required".into(),
                priority: 80,
                predicate: Box::new(RequiresFeaturePredicate {
                    feature: ProviderFeature::Vision,
                }),
                target: "flagship".into(),
                terminal: true,
            },

            // Tool calling required → any tool-capable provider
            RoutingRule {
                name: "tools_required".into(),
                priority: 70,
                predicate: Box::new(RequiresFeaturePredicate {
                    feature: ProviderFeature::ToolCalling,
                }),
                target: "standard".into(),
                terminal: false,  // allow fallback to budget
            },

            // @budget keyword → cheapest provider
            RoutingRule {
                name: "budget_keyword".into(),
                priority: 60,
                predicate: Box::new(SystemPromptKeywordPredicate {
                    keywords: vec!["@budget".into()],
                    any: true,
                }),
                target: "budget".into(),
                terminal: true,
            },

            // Default: standard tier
            RoutingRule {
                name: "default".into(),
                priority: 0,
                predicate: Box::new(AlwaysPredicate),
                target: "standard".into(),
                terminal: true,
            },
        ]
    }
}
```

### 2.5 Routing Execution

```rust
#[async_trait]
impl LlmProvider for CostedProvider {
    fn id(&self) -> &str { "costed" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        // 1. Evaluate rules in priority order
        let target = self.resolve_route(&request).await;

        // 2. Find the target provider
        let route = self.providers.iter()
            .find(|r| r.name == target)
            .ok_or(ProviderError::RouteNotFound(target))?;

        // 3. Check health
        if !route.healthy.load(Ordering::Relaxed) {
            // Fall back to next best provider
            let fallback = self.find_healthy_alternative(&route.tier)?;
            return fallback.provider.complete(request).await;
        }

        // 4. Execute
        let start = Instant::now();
        let result = route.provider.complete(request.clone()).await;

        match result {
            Ok(response) => {
                // Emit routing decision event
                self.emit_routing_event(
                    &request,
                    &target,
                    &response.usage,
                    start.elapsed(),
                    true,
                );
                Ok(response)
            }
            Err(e) => {
                // Mark provider as unhealthy temporarily
                route.healthy.store(false, Ordering::Relaxed);
                // Try alternative
                let fallback = self.find_healthy_alternative(&route.tier)?;
                let response = fallback.provider.complete(request).await?;
                self.emit_routing_event(
                    &request,
                    &target,
                    &response.usage,
                    start.elapsed(),
                    false,
                );
                Ok(response)
            }
        }
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Costed }
}

impl CostedProvider {
    async fn resolve_route(&self, request: &LlmRequest) -> String {
        let mut rules = self.rules.clone();
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        for rule in &rules {
            if rule.predicate.evaluate(request).await {
                return rule.target.clone();
            }
        }

        self.default_provider.clone()
    }
}
```

### 2.6 ProviderRoutingDecision Event

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRoutingDecision {
    /// Request that was routed.
    pub request_model: String,
    /// Target provider that was selected.
    pub target_provider: String,
    /// Target cost tier.
    pub target_tier: CostTier,
    /// Rule that matched (if any).
    pub matched_rule: Option<String>,
    /// Whether the request succeeded on the target.
    pub succeeded: bool,
    /// Token usage.
    pub usage: TokenUsage,
    /// Estimated cost in USD.
    pub estimated_cost_usd: f64,
    /// Latency.
    pub latency: Duration,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}
```

---

## 3. FallbackProvider

### 3.1 Concept

`FallbackProvider` tries a chain of providers in order. If the primary fails,
it retries with the next provider in the chain.

```
  Request
     │
     ▼
  ┌─────────┐     fail     ┌──────────┐     fail     ┌───────┐
  │ Primary │─────────────▶│Fallback 1│─────────────▶│Fallb 2│
  │ (OpenAI)│              │(Anthropic)│              │(Ollama)│
  └─────────┘              └──────────┘              └───────┘
       │                        │                        │
       │ success                │ success                │ success
       ▼                        ▼                        ▼
  Response                 Response                 Response
```

### 3.2 Streaming Modes

FallbackProvider has three modes for handling streaming across the fallback
chain:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamingFallbackMode {
    /// Buffer the entire response from the primary before committing.
    /// If the primary fails, seamlessly retry with fallback.
    /// Latency cost: adds full response time before streaming starts.
    BufferAndCommit,

    /// Start streaming immediately. If the primary fails mid-stream,
    /// abort and retry with fallback. The client may see a gap.
    /// Lowest latency but may produce visible errors.
    FailFast,

    /// Disable streaming entirely for the fallback chain.
    /// All requests use non-streaming complete().
    Disable,
}
```

### 3.3 Implementation

```rust
pub struct FallbackProvider {
    /// Provider chain (ordered by priority).
    chain: Vec<Arc<dyn LlmProvider>>,
    /// Streaming fallback mode.
    streaming_mode: StreamingFallbackMode,
    /// Maximum retries per provider before moving to next.
    max_retries_per_provider: usize,
    /// Retry delay strategy.
    retry_delay: RetryDelay,
    /// Errors that trigger fallback (others are propagated).
    fallback_on_errors: Vec<ErrorCategory>,
}

#[derive(Debug, Clone)]
pub enum RetryDelay {
    /// Fixed delay between retries.
    Fixed(Duration),
    /// Exponential backoff.
    ExponentialBackoff {
        base: Duration,
        max: Duration,
        multiplier: f64,
    },
    /// No delay.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Rate limit (429).
    RateLimit,
    /// Server error (5xx).
    ServerError,
    /// Timeout.
    Timeout,
    /// Context window exceeded.
    ContextOverflow,
    /// Content filter triggered.
    ContentFilter,
    /// Authentication error.
    AuthError,
    /// Network error.
    NetworkError,
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    fn id(&self) -> &str { "fallback" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let mut last_error = None;

        for (i, provider) in self.chain.iter().enumerate() {
            let mut attempt = 0;

            loop {
                match provider.complete(request.clone()).await {
                    Ok(response) => {
                        tracing::info!(
                            provider = %provider.id(),
                            attempt = attempt,
                            chain_position = i,
                            "Request succeeded"
                        );
                        return Ok(response);
                    }
                    Err(e) => {
                        let category = self.categorize_error(&e);

                        if !self.fallback_on_errors.contains(&category) {
                            // Don't fallback for this error type
                            return Err(e);
                        }

                        attempt += 1;

                        if attempt < self.max_retries_per_provider {
                            // Retry same provider
                            let delay = self.retry_delay.delay_for(attempt);
                            tokio::time::sleep(delay).await;
                            continue;
                        }

                        // Move to next provider
                        tracing::warn!(
                            provider = %provider.id(),
                            error = %e,
                            "Provider failed, trying fallback"
                        );
                        last_error = Some(e);
                        break;
                    }
                }
            }
        }

        Err(last_error.unwrap_or(ProviderError::AllProvidersFailed))
    }

    async fn complete_stream(
        &self,
        request: LlmRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        match self.streaming_mode {
            StreamingFallbackMode::BufferAndCommit => {
                // Buffer the complete response, then stream it
                let response = self.complete(request).await?;
                let chunks = self.response_to_chunks(response);
                Ok(Box::pin(futures::stream::iter(chunks)))
            }
            StreamingFallbackMode::FailFast => {
                // Try streaming from primary, fallback on first error
                for provider in &self.chain {
                    match provider.complete_stream(request.clone()).await {
                        Ok(stream) => return Ok(stream),
                        Err(e) => {
                            if self.fallback_on_errors.contains(&self.categorize_error(&e)) {
                                continue;
                            }
                            return Err(e);
                        }
                    }
                }
                Err(ProviderError::AllProvidersFailed)
            }
            StreamingFallbackMode::Disable => {
                // Non-streaming only
                let response = self.complete(request).await?;
                let chunks = self.response_to_chunks(response);
                Ok(Box::pin(futures::stream::iter(chunks)))
            }
        }
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Fallback }
}
```

---

## 4. BestOfNProvider

### 4.1 Architecture

```
  Request ──▶ ┌───────────┐
              │ Sample xN │
              └─────┬─────┘
                    │
        ┌───────────┼───────────┐
        ▼           ▼           ▼
  ┌──────────┐┌──────────┐┌──────────┐
  │ Sample 1 ││ Sample 2 ││ Sample 3 │
  │ temp=0.7 ││ temp=0.7 ││ temp=0.7 │
  └────┬─────┘└────┬─────┘└────┬─────┘
       │           │           │
       └───────────┼───────────┘
                   ▼
            ┌──────────┐
            │  Judge   │
            │(LLM or   │
            │heuristic)│
            └────┬─────┘
                 │
                 ▼
          Best Sample
```

### 4.2 Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BestOfNConfig {
    /// Number of samples to generate.
    pub n: usize,
    /// Temperature for sampling.
    pub temperature: f64,
    /// Maximum tokens per sample.
    pub max_tokens_per_sample: usize,
    /// Judge type.
    pub judge: JudgeType,
    /// Judge model (for LLM judge).
    pub judge_model: String,
    /// Whether to emit all samples for debugging.
    pub keep_all_samples: bool,
    /// Timeout for sample generation.
    pub sample_timeout: Duration,
    /// Maximum cost multiplier (if budget is tight, reduce N).
    pub max_cost_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JudgeType {
    /// LLM-based judge (most accurate, adds cost).
    Llm,
    /// Heuristic judge (fast, no additional cost).
    Heuristic(JudgeWeights),
    /// Custom judge implementation.
    Custom(String),
}
```

### 4.3 Implementation

```rust
pub struct BestOfNProvider<P: LlmProvider> {
    inner: P,
    judge: Arc<dyn ResponseJudge>,
    config: BestOfNConfig,
}

#[async_trait]
impl<P: LlmProvider + Clone> LlmProvider for BestOfNProvider<P> {
    fn id(&self) -> &str { "best_of_n" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let effective_n = self.effective_n(&request);

        // Generate N samples in parallel
        let mut sample_requests = Vec::with_capacity(effective_n);
        for _ in 0..effective_n {
            let mut req = request.clone();
            req.temperature = Some(self.config.temperature);
            req.max_tokens = Some(self.config.max_tokens_per_sample);
            sample_requests.push(req);
        }

        let samples: Vec<LlmResponse> = futures::future::join_all(
            sample_requests.into_iter().map(|req| {
                let provider = self.inner.clone();
                async move {
                    tokio::time::timeout(
                        self.config.sample_timeout,
                        provider.complete(req),
                    ).await
                }
            })
        ).await.into_iter()
            .filter_map(|r| r.ok())   // timeout ok
            .filter_map(|r| r.ok())   // provider ok
            .collect();

        if samples.is_empty() {
            return Err(ProviderError::AllSamplesFailed);
        }

        if samples.len() == 1 {
            return Ok(samples.into_iter().next().unwrap());
        }

        // Judge selects the best
        let best_index = self.judge.select_best(
            &request.messages,
            &samples,
            &self.config.judge_model,
        ).await.map_err(|e| ProviderError::JudgeError(e.to_string()))?;

        // Aggregate token usage
        let total_usage = samples.iter()
            .fold(TokenUsage::default(), |acc, s| TokenUsage {
                prompt_tokens: acc.prompt_tokens + s.usage.prompt_tokens,
                completion_tokens: acc.completion_tokens + s.usage.completion_tokens,
                total_tokens: acc.total_tokens + s.usage.total_tokens,
            });

        let mut best = samples.into_iter().nth(best_index)
            .ok_or(ProviderError::InvalidJudgeIndex(best_index))?;
        best.usage = total_usage;  // Report total cost including all samples

        Ok(best)
    }

    fn estimate_cost(&self, request: &LlmRequest) -> CostEstimate {
        let base = self.inner.estimate_cost(request);
        CostEstimate {
            input_tokens: base.input_tokens * self.config.n as u64,
            output_tokens: base.output_tokens * self.config.n as u64,
            cost_usd: base.cost_usd * self.config.n as f64,
        }
    }

    fn provider_type(&self) -> ProviderType { ProviderType::BestOfN }
}

impl<P: LlmProvider> BestOfNProvider<P> {
    /// Adjust N based on budget constraints.
    fn effective_n(&self, request: &LlmRequest) -> usize {
        let base_cost = self.inner.estimate_cost(request);
        let total_cost = base_cost.cost_usd * self.config.n as f64;
        if total_cost > self.config.max_cost_multiplier * base_cost.cost_usd {
            // Reduce N to stay within budget
            let max_n = (self.config.max_cost_multiplier) as usize;
            max_n.max(1)
        } else {
            self.config.n
        }
    }
}
```

---

## 5. ConsensusProvider

### 5.1 Multi-Model Voting

```rust
pub struct ConsensusProvider {
    providers: Vec<Arc<dyn LlmProvider>>,
    strategy: ConsensusStrategy,
    config: ConsensusConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusConfig {
    /// Minimum number of agreeing providers (quorum).
    pub quorum: usize,
    /// Timeout per provider.
    pub per_provider_timeout: Duration,
    /// Whether to fall back to majority vote.
    pub majority_fallback: bool,
    /// Semantic consensus configuration.
    pub semantic: SemanticConsensusConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConsensusConfig {
    /// Similarity threshold (0.0–1.0).
    pub threshold: f64,
    /// Judge model for semantic comparison.
    pub judge_model: String,
    /// Whether to use embedding-based similarity first.
    pub use_embeddings: bool,
}

#[async_trait]
impl LlmProvider for ConsensusProvider {
    fn id(&self) -> &str { "consensus" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        // Query all providers in parallel
        let results: Vec<(usize, Result<LlmResponse, ProviderError>)> =
            futures::future::join_all(
                self.providers.iter().enumerate().map(|(i, provider)| {
                    let req = request.clone();
                    let timeout = self.config.per_provider_timeout;
                    async move {
                        let result = tokio::time::timeout(timeout, provider.complete(req)).await;
                        (i, result.map_err(|_| ProviderError::Timeout(timeout)).and_then(|r| r))
                    }
                })
            ).await;

        let responses: Vec<(usize, LlmResponse)> = results.into_iter()
            .filter_map(|(i, r)| r.ok().map(|resp| (i, resp)))
            .collect();

        if responses.is_empty() {
            return Err(ProviderError::AllProvidersFailed);
        }

        // Apply consensus strategy
        let consensus_result = match &self.strategy {
            ConsensusStrategy::ExactMatch => {
                self.exact_match_consensus(&responses)?
            }
            ConsensusStrategy::NormalizedMatch => {
                self.normalized_consensus(&responses)?
            }
            ConsensusStrategy::SemanticSimilarity { threshold } => {
                self.semantic_consensus(&responses, *threshold).await?
            }
            ConsensusStrategy::LlmJudged => {
                self.llm_judged_consensus(&responses).await?
            }
            ConsensusStrategy::StructuredMatch { fields } => {
                self.structured_consensus(&responses, fields)?
            }
        };

        Ok(consensus_result)
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Consensus }
}
```

### 5.2 Quorum Enforcement

```rust
#[derive(Debug, Clone)]
pub struct ConsensusResult {
    /// The consensus response.
    pub response: LlmResponse,
    /// Whether strict quorum was reached.
    pub quorum_reached: bool,
    /// Number of agreeing providers.
    pub agreement_count: usize,
    /// Total providers queried.
    pub total_providers: usize,
    /// Per-provider responses for audit.
    pub all_responses: Vec<(String, String)>,
}

impl ConsensusProvider {
    fn check_quorum(&self, agreement_count: usize) -> Result<(), ProviderError> {
        if agreement_count >= self.config.quorum {
            Ok(())
        } else if self.config.majority_fallback && agreement_count > self.providers.len() / 2 {
            tracing::warn!(
                agreement = agreement_count,
                quorum = self.config.quorum,
                "Quorum not reached but majority achieved; using majority result"
            );
            Ok(())
        } else {
            Err(ProviderError::NoQuorum {
                quorum: self.config.quorum,
                agreement: agreement_count,
            })
        }
    }
}
```

### 5.3 Structured Field Matching

For structured output, consensus is checked on specific JSON fields:

```rust
impl ConsensusProvider {
    fn structured_consensus(
        &self,
        responses: &[(usize, LlmResponse)],
        fields: &[String],
    ) -> Result<LlmResponse, ProviderError> {
        // Parse all responses as JSON
        let parsed: Vec<(usize, serde_json::Value)> = responses.iter()
            .filter_map(|(i, r)| {
                serde_json::from_str::<serde_json::Value>(&r.content)
                    .ok()
                    .map(|v| (*i, v))
            })
            .collect();

        if parsed.is_empty() {
            // Fall back to exact match
            return self.exact_match_consensus(responses);
        }

        // Check each field for agreement
        let mut field_agreements: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, value) in &parsed {
            for field in fields {
                if let Some(field_val) = value.get(field) {
                    let key = field_val.to_string();
                    field_agreements.entry(format!("{}:{}", field, key))
                        .or_default()
                        .push(*i);
                }
            }
        }

        // Find the response with the most field agreements
        let mut best_score = 0;
        let mut best_index = 0;

        for (idx, _) in &parsed {
            let score = field_agreements.values()
                .filter(|indices| indices.contains(idx))
                .count();
            if score > best_score {
                best_score = score;
                best_index = *idx;
            }
        }

        let agreement_count = field_agreements.values()
            .filter(|indices| indices.len() >= self.config.quorum)
            .count();

        self.check_quorum(agreement_count)?;

        Ok(responses[best_index].1.clone())
    }
}
```

---

## 6. Budget Enforcement

### 6.1 UserBudgetGuardrail

```rust
pub struct UserBudgetGuardrail<P: LlmProvider> {
    inner: P,
    budget: Arc<TokenBudget>,
    config: BudgetGuardrailConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetGuardrailConfig {
    /// Maximum spend in USD.
    pub max_spend_usd: f64,
    /// Warning threshold (percentage of max_spend).
    pub warn_threshold_percent: u8,
    /// Hard stop threshold (percentage of max_spend).
    pub stop_threshold_percent: u8,
    /// Action on budget exceeded.
    pub on_exceeded: BudgetExceededAction,
    /// Action on budget warning.
    pub on_warning: BudgetWarningAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BudgetExceededAction {
    /// Return an error immediately.
    Error,
    /// Switch to the cheapest available provider.
    DowngradeToBudget,
    /// Stop all tasks gracefully.
    GracefulShutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BudgetWarningAction {
    /// Log a warning.
    Log,
    /// Emit an event to the SSE stream.
    EmitEvent,
    /// Prompt the user for approval.
    PromptUser,
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for UserBudgetGuardrail<P> {
    fn id(&self) -> &str { "budget_guardrail" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        // Pre-flight cost estimate
        let estimate = self.inner.estimate_cost(&request);
        let current_spend = self.budget.current_spend_usd();
        let projected_spend = current_spend + estimate.cost_usd;
        let max_spend = self.config.max_spend_usd;

        // Check hard stop
        if projected_spend > max_spend * (self.config.stop_threshold_percent as f64 / 100.0) {
            match self.config.on_exceeded {
                BudgetExceededAction::Error => {
                    return Err(ProviderError::BudgetExceeded {
                        current_spend,
                        max_spend,
                        projected: projected_spend,
                    });
                }
                BudgetExceededAction::DowngradeToBudget => {
                    // Modify request to use cheapest model
                    let mut cheap_request = request.clone();
                    cheap_request.model = "gpt-4o-mini".into();
                    return self.inner.complete(cheap_request).await;
                }
                BudgetExceededAction::GracefulShutdown => {
                    // Signal shutdown
                    return Err(ProviderError::BudgetShutdown {
                        current_spend,
                        max_spend,
                    });
                }
            }
        }

        // Check warning threshold
        if projected_spend > max_spend * (self.config.warn_threshold_percent as f64 / 100.0) {
            match self.config.on_warning {
                BudgetWarningAction::Log => {
                    tracing::warn!(
                        current = %current_spend,
                        max = %max_spend,
                        "Approaching budget limit"
                    );
                }
                BudgetWarningAction::EmitEvent => {
                    self.budget.emit_warning(current_spend, max_spend);
                }
                BudgetWarningAction::PromptUser => {
                    // In CLI mode, prompt user to continue
                    let confirmed = self.prompt_user(current_spend, max_spend).await;
                    if !confirmed {
                        return Err(ProviderError::BudgetUserCancelled {
                            current_spend,
                            max_spend,
                        });
                    }
                }
            }
        }

        // Execute request
        let response = self.inner.complete(request).await?;

        // Record actual spend
        self.budget.record_spend(estimate.cost_usd);

        Ok(response)
    }

    fn provider_type(&self) -> ProviderType { ProviderType::BudgetGuardrail }
}
```

### 6.2 Token Budget Implementation

```rust
pub struct TokenBudget {
    /// Maximum spend in USD.
    max_spend_usd: AtomicU64,  // stored as cents (x100)
    /// Current spend in USD.
    current_spend_usd: AtomicU64,  // stored as cents (x100)
    /// Token counts by provider.
    spend_by_provider: DashMap<String, AtomicU64>,
    /// Event sender for budget notifications.
    event_tx: mpsc::Sender<BudgetEvent>,
}

impl TokenBudget {
    pub fn new(max_spend_usd: f64) -> Self {
        Self {
            max_spend_usd: AtomicU64::new((max_spend_usd * 100.0) as u64),
            current_spend_usd: AtomicU64::new(0),
            spend_by_provider: DashMap::new(),
            event_tx: /* ... */,
        }
    }

    pub fn current_spend_usd(&self) -> f64 {
        self.current_spend_usd.load(Ordering::Relaxed) as f64 / 100.0
    }

    pub fn remaining_usd(&self) -> f64 {
        let max = self.max_spend_usd.load(Ordering::Relaxed) as f64 / 100.0;
        max - self.current_spend_usd()
    }

    pub fn record_spend(&self, cost_usd: f64) {
        let cost_cents = (cost_usd * 100.0) as u64;
        self.current_spend_usd.fetch_add(cost_cents, Ordering::Relaxed);
    }

    pub fn record_provider_spend(&self, provider: &str, cost_usd: f64) {
        let cost_cents = (cost_usd * 100.0) as u64;
        self.spend_by_provider
            .entry(provider.to_string())
            .or_insert(AtomicU64::new(0))
            .fetch_add(cost_cents, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum BudgetEvent {
    #[serde(rename = "budget.warning")]
    Warning { current: f64, max: f64, percent: u8 },

    #[serde(rename = "budget.exceeded")]
    Exceeded { current: f64, max: f64 },

    #[serde(rename = "budget.spend_recorded")]
    SpendRecorded { provider: String, amount: f64, total: f64 },
}
```

---

## 7. Configuration Reference

```toml
[xaft.providers.cost_routing]
default_tier = "standard"       # "budget" | "standard" | "flagship"

[[xaft.providers.cost_routing.rules]]
name = "flagship_keyword"
priority = 100
predicate = "system_prompt_keyword"
keywords = ["@flagship", "@thinking"]
target = "flagship"
terminal = true

[[xaft.providers.cost_routing.rules]]
name = "local_keyword"
priority = 90
predicate = "system_prompt_keyword"
keywords = ["@local"]
target = "ollama"
terminal = true

[[xaft.providers.cost_routing.rules]]
name = "vision_required"
priority = 80
predicate = "requires_feature"
feature = "vision"
target = "flagship"
terminal = true

[[xaft.providers.cost_routing.rules]]
name = "default"
priority = 0
predicate = "always"
target = "standard"
terminal = true

[xaft.providers.fallback]
chain = ["openai", "anthropic", "ollama"]
streaming_mode = "buffer_and_commit"    # "buffer_and_commit" | "fail_fast" | "disable"
max_retries_per_provider = 2
fallback_on_errors = ["rate_limit", "server_error", "timeout", "context_overflow"]
retry_base_delay_secs = 1
retry_max_delay_secs = 30
retry_multiplier = 2.0

[xaft.providers.best_of_n]
enabled = false
n = 3
temperature = 0.7
max_tokens_per_sample = 4096
judge = "heuristic"            # "llm" | "heuristic" | "custom"
judge_model = "gpt-4o-mini"
keep_all_samples = false
sample_timeout_secs = 120
max_cost_multiplier = 5.0

[xaft.providers.consensus]
enabled = false
providers = ["openai", "anthropic", "gemini"]
quorum = 2
strategy = "exact_match"       # "exact_match" | "normalized" | "semantic" | "llm_judged" | "structured"
per_provider_timeout_secs = 60
majority_fallback = true

[xaft.budget]
max_spend_usd = 10.0
warn_threshold_percent = 75
stop_threshold_percent = 95
on_exceeded = "error"          # "error" | "downgrade" | "shutdown"
on_warning = "log"             # "log" | "event" | "prompt"
```

---

## 8. Cost Tracking Dashboard

xauft emits structured cost events that can be visualized:

```
╔══════════════════════════════════════════════════════════════╗
║  xauft Cost Dashboard                                        ║
╠══════════════════════════════════════════════════════════════╣
║                                                              ║
║  Budget: $10.00  │  Spent: $3.47 (34.7%)  │  Remaining: $6.53 ║
║  ████████████░░░░░░░░░░░░░░░░░░░░                           ║
║                                                              ║
║  By Provider:                                                ║
║    OpenAI    $2.10  ██████████████████████                   ║
║    Anthropic $1.22  █████████████                             ║
║    Ollama    $0.00  ░                                         ║
║    Gemini    $0.15  █                                         ║
║                                                              ║
║  By Tier:                                                    ║
║    Flagship  $1.80  █████████████████                         ║
║    Standard  $1.52  ██████████████                            ║
║    Budget    $0.15  █                                         ║
║                                                              ║
║  By Agent Role:                                              ║
║    Coder     $1.40  █████████████                             ║
║    QA        $0.95  █████████                                 ║
║    Planner   $0.62  ██████                                   ║
║    Fixer     $0.50  █████                                    ║
║                                                              ║
╚══════════════════════════════════════════════════════════════╝
```
