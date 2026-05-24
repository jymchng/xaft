# XAFT Planning System — PRD

> Document ID: XAFT-PLAN-001
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

The planning system is the cognitive backbone of `xaft`. Before any file is touched or command is run, `xaft` must produce a structured plan that describes *what* it will do, *why*, and in *what order*. This document specifies the three planner backends provided by the `agtrs` framework, the `Intent` specification that feeds them, the `ReplanTool` for mid-execution revision, and the contract between planning and the `TaskRunner`.

---

## 2. Architecture

```
                         ┌──────────────────────┐
                         │       Intent         │
                         │  (goal, constraints, │
                         │   preferences,       │
                         │   acceptance_criteria│
                         │   )                  │
                         └─────────┬────────────┘
                                   │
                    ┌──────────────┼──────────────────┐
                    │              │                  │
                    ▼              ▼                  ▼
          ┌─────────────┐ ┌───────────────┐ ┌──────────────────┐
          │ OneShotPlan │ │ IterativeRef  │ │ TreeOfThought    │
          │    ner      │ │ inementPlan   │ │    Planner       │
          │ (3-tier)    │ │   ner         │ │ (multi-candidate)│
          └──────┬──────┘ └───────┬───────┘ └────────┬─────────┘
                 │                │                   │
                 └────────────────┼───────────────────┘
                                  │
                                  ▼
                        ┌──────────────────┐
                        │     Plan         │
                        │  (Vec<Step>)     │
                        └────────┬─────────┘
                                 │
                    ┌────────────┼────────────┐
                    │            │            │
                    ▼            ▼            ▼
              ┌──────────┐ ┌──────────┐ ┌──────────┐
              │TaskRunner│ │ Replan   │ │ PlanStore│
              │          │ │  Tool    │ │ (persist)│
              └──────────┘ └──────────┘ └──────────┘
```

---

## 3. Intent Specification

The `Intent` is the immutable input to every planner. It captures the user's desire in a structured form.

### 3.1 Data Model

```rust
/// The structured expression of what the user wants to accomplish.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    /// Natural-language goal statement.
    /// Example: "Add JWT authentication to the /api endpoints"
    pub goal: String,

    /// Hard constraints that must not be violated.
    /// Example: "Must not modify the public API signature of UserService"
    pub constraints: Vec<Constraint>,

    /// Soft preferences that the planner should try to honour.
    /// Example: "Prefer using the existing `jsonwebtoken` crate"
    pub preferences: Vec<Preference>,

    /// Measurable criteria that determine plan success.
    /// Example: "`cargo test` passes; `cargo clippy` has 0 warnings"
    pub acceptance_criteria: Vec<AcceptanceCriterion>,

    /// Optional context from prior planning sessions.
    pub prior_context: Option<PriorContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    pub description: String,
    pub severity: ConstraintSeverity, // Hard | Advisory
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub description: String,
    pub weight: f32, // 0.0–1.0, used for plan ranking
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    pub description: String,
    pub validation: ValidationMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationMethod {
    ShellCommand { command: String, expected_exit_code: i32 },
    FileExists { path: PathBuf },
    FileContains { path: PathBuf, pattern: String },
    ManualReview,
}
```

### 3.2 Intent Construction Pipeline

```
 User Input (CLI / TUI / API)
         │
         ▼
 ┌──────────────────┐
 │ IntentBuilder    │
 │ - parse goal     │
 │ - infer con-     │
 │   straints from  │
 │   .xaft.toml     │
 │ - extract pref-  │
 │   erences from   │
 │   conversation   │
 │ - load accept-   │
 │   ance criteria  │
 │   from project   │
 │   config         │
 └────────┬─────────┘
          │
          ▼
    ┌────────────┐
    │   Intent   │  ← validated, frozen, hashed
    └────────────┘
```

```rust
impl IntentBuilder {
    pub fn build(self) -> Result<Intent, IntentError> {
        let constraints = self.infer_constraints_from_config()?;
        let preferences = self.merge_preferences(self.cli_prefs, self.config_prefs)?;
        let acceptance  = self.load_acceptance_criteria()?;

        let intent = Intent {
            goal: self.goal,
            constraints,
            preferences,
            acceptance_criteria: acceptance,
            prior_context: self.prior_context,
        };

        // Validate: at least one acceptance criterion must exist
        if intent.acceptance_criteria.is_empty() {
            return Err(IntentError::NoAcceptanceCriteria);
        }

        Ok(intent)
    }
}
```

---

## 4. OneShotPlanner (3-Tier Strategy)

The `OneShotPlanner` is the default planner for well-scoped tasks. It uses a **3-tier fallback** strategy to extract a structured plan from the LLM.

### 4.1 3-Tier Extraction Pipeline

```
 ┌─────────────────────────────────────────────────────┐
 │                  OneShotPlanner                      │
 │                                                      │
 │   Tier 1: Tool-Call Extraction                       │
 │   ┌───────────────────────────────────────────┐      │
 │   │ LLM returns structured tool_call with      │      │
 │   │ name="submit_plan", args={steps:[...]}     │      │
 │   │ → Directly deserialized into Plan          │      │
 │   └───────────────────┬───────────────────────┘      │
 │                       │ on parse failure              │
 │                       ▼                               │
 │   Tier 2: Structured LLM Output                      │
 │   ┌───────────────────────────────────────────┐      │
 │   │ LLM returns JSON in markdown code fence    │      │
 │   │ ```json {"steps":[...]} ```                │      │
 │   │ → Extracted via regex, deserialized        │      │
 │   └───────────────────┬───────────────────────┘      │
 │                       │ on parse failure              │
 │                       ▼                               │
 │   Tier 3: Free-Text Extraction                       │
 │   ┌───────────────────────────────────────────┐      │
 │   │ LLM returns natural-language plan          │      │
 │   │ → Regex/heuristic extraction of numbered   │      │
 │   │   steps → best-effort Plan construction    │      │
 │   └───────────────────────────────────────────┘      │
 └─────────────────────────────────────────────────────┘
```

### 4.2 Implementation

```rust
pub struct OneShotPlanner {
    llm_client: LlmClient,
    prompt_template: PromptTemplate,
    tier_config: TierConfig,
}

#[derive(Debug, Clone)]
pub struct TierConfig {
    /// Maximum token budget for the planning LLM call.
    pub max_tokens: u32,
    /// Temperature for plan generation (lower = more deterministic).
    pub temperature: f32,
    /// Whether to attempt Tier 2 before falling back to Tier 3.
    pub enable_tier2: bool,
    /// Whether to attempt Tier 3 as last resort.
    pub enable_tier3: bool,
}

impl OneShotPlanner {
    pub async fn plan(&self, intent: &Intent) -> Result<Plan, PlannerError> {
        let prompt = self.prompt_template.render(intent)?;
        let response = self.llm_client.chat(prompt).await?;

        // Tier 1: Tool-call extraction
        if let Some(plan) = self.extract_via_tool_call(&response)? {
            tracing::info!("Plan extracted via Tier 1 (tool-call)");
            return Ok(plan);
        }

        // Tier 2: Structured JSON in code fence
        if self.tier_config.enable_tier2 {
            if let Some(plan) = self.extract_via_structured_json(&response)? {
                tracing::info!("Plan extracted via Tier 2 (structured JSON)");
                return Ok(plan);
            }
        }

        // Tier 3: Free-text heuristic extraction
        if self.tier_config.enable_tier3 {
            if let Some(plan) = self.extract_via_free_text(&response)? {
                tracing::info!("Plan extracted via Tier 3 (free-text)");
                return Ok(plan);
            }
        }

        Err(PlannerError::ExtractionFailed {
            raw_response: response.content,
        })
    }

    fn extract_via_tool_call(&self, response: &LlmResponse) -> Result<Option<Plan>, PlannerError> {
        for tool_call in &response.tool_calls {
            if tool_call.name == "submit_plan" {
                let plan: Plan = serde_json::from_str(&tool_call.arguments)
                    .map_err(PlannerError::ToolCallDeserialization)?;
                plan.validate()?;
                return Ok(Some(plan));
            }
        }
        Ok(None)
    }

    fn extract_via_structured_json(&self, response: &LlmResponse) -> Result<Option<Plan>, PlannerError> {
        // Look for ```json ... ``` code fences
        let re = regex::Regex::new(r"```json\s*\n([\s\S]*?)\n```")?;
        for cap in re.captures_iter(&response.content) {
            if let Ok(plan) = serde_json::from_str::<Plan>(&cap[1]) {
                plan.validate()?;
                return Ok(Some(plan));
            }
        }
        Ok(None)
    }

    fn extract_via_free_text(&self, response: &LlmResponse) -> Result<Option<Plan>, PlannerError> {
        // Heuristic: look for numbered/bulleted lists, extract as steps
        let step_re = regex::Regex::new(
            r"(?m)^\s*(?:\d+[\.\)]|[-*])\s+(.+)$"
        )?;
        let steps: Vec<Step> = step_re
            .captures_iter(&response.content)
            .filter_map(|cap| {
                let desc = cap[1].trim().to_string();
                if desc.len() < 5 { return None; } // skip false positives
                Some(Step {
                    id: StepId::new(),
                    description: desc,
                    tool: ToolHint::Unspecified,
                    dependencies: vec![],
                    rollback: None,
                })
            })
            .collect();

        if steps.is_empty() {
            return Ok(None);
        }
        Ok(Some(Plan { id: PlanId::new(), steps, intent_hash: intent.hash() }))
    }
}
```

---

## 5. IterativeRefinementPlanner

For complex tasks requiring higher plan quality, the `IterativeRefinementPlanner` uses a **draft → critique → revise** loop.

### 5.1 Loop Architecture

```
         ┌──────────────────────────────────────────────────┐
         │          IterativeRefinementPlanner               │
         │                                                   │
         │   ┌─────┐     ┌──────────┐     ┌──────────┐     │
         │   │Draft│────▶│ Critique │────▶│  Revise  │     │
         │   └─────┘     └────┬─────┘     └────┬─────┘     │
         │                     │                │            │
         │                     │ score < θ?     │            │
         │                     │                │            │
         │                     ▼                │            │
         │              ┌─────────────┐         │            │
         │              │ Accept Plan │◀────────┘            │
         │              │ (or abort)  │                      │
         │              └─────────────┘                      │
         │                                                   │
         │   max_iterations: usize (default: 3)              │
         │   quality_threshold: f32 (default: 0.80)          │
         └──────────────────────────────────────────────────┘
```

### 5.2 Critique Scoring

The critic evaluates the plan on multiple axes:

| Axis                  | Weight | Description                                       |
|-----------------------|--------|---------------------------------------------------|
| Completeness          | 0.25   | Does the plan cover all aspects of the intent?    |
| Constraint Adherence  | 0.25   | Are all hard constraints respected?               |
| Step Ordering         | 0.20   | Are dependencies correctly ordered?               |
| Rollback Feasibility  | 0.15   | Can each step be rolled back?                     |
| Preference Alignment  | 0.15   | Are soft preferences honoured?                    |

### 5.3 Implementation

```rust
pub struct IterativeRefinementPlanner {
    drafter: OneShotPlanner,
    critic:  LlmClient,
    reviser: LlmClient,
    config:  RefinementConfig,
}

#[derive(Debug, Clone)]
pub struct RefinementConfig {
    pub max_iterations: usize,
    pub quality_threshold: f32,
    pub critique_axes: Vec<CritiqueAxis>,
}

pub struct CritiqueResult {
    pub scores: HashMap<CritiqueAxis, f32>,
    pub overall: f32,
    pub feedback: Vec<String>,
}

impl IterativeRefinementPlanner {
    pub async fn plan(&self, intent: &Intent) -> Result<Plan, PlannerError> {
        let mut current_plan = self.drafter.plan(intent).await?;
        let mut iteration = 0;

        loop {
            // ── Critique ──
            let critique = self.critique(&current_plan, intent).await?;
            tracing::info!(
                iteration,
                score = critique.overall,
                "Critique completed"
            );

            if critique.overall >= self.config.quality_threshold {
                tracing::info!("Plan accepted at iteration {iteration}");
                return Ok(current_plan);
            }

            iteration += 1;
            if iteration > self.config.max_iterations {
                tracing::warn!(
                    "Max iterations reached; returning best-effort plan (score={})",
                    critique.overall
                );
                return Ok(current_plan);
            }

            // ── Revise ──
            current_plan = self.revise(&current_plan, &critique, intent).await?;
        }
    }

    async fn critique(&self, plan: &Plan, intent: &Intent) -> Result<CritiqueResult, PlannerError> {
        let prompt = format!(
            "You are a plan critic. Evaluate this plan against the intent.\n\n\
             ## Intent\n{intent:#?}\n\n\
             ## Plan\n{plan:#?}\n\n\
             Score each axis from 0.0 to 1.0 and provide actionable feedback."
        );
        let response = self.critic.chat(prompt).await?;
        let result: CritiqueResult = serde_json::from_str(&response.content)?;
        Ok(result)
    }

    async fn revise(
        &self,
        plan: &Plan,
        critique: &CritiqueResult,
        intent: &Intent,
    ) -> Result<Plan, PlannerError> {
        let prompt = format!(
            "Revise the following plan based on critique feedback.\n\n\
             ## Original Plan\n{plan:#?}\n\n\
             ## Critique\n{critique:#?}\n\n\
             ## Intent\n{intent:#?}"
        );
        let response = self.reviser.chat(prompt).await?;
        // Use Tier-2 extraction for structured revision
        self.drafter.extract_via_structured_json(&response)?
            .ok_or(PlannerError::RevisionFailed)
    }
}
```

---

## 6. TreeOfThoughtPlanner (Multi-Candidate)

For open-ended or ambiguous tasks, the `TreeOfThoughtPlanner` generates multiple candidate plans, evaluates them, and selects the best.

### 6.1 Tree Structure

```
                         Intent
                           │
                ┌──────────┼──────────┐
                │          │          │
                ▼          ▼          ▼
           Candidate   Candidate  Candidate
              A           B          C
                │          │          │
           ┌────┴────┐     │     ┌────┴────┐
           │         │     │     │         │
           ▼         ▼     ▼     ▼         ▼
         A.1       A.2   B.1  C.1       C.2
           │         │
           ▼         ▼
        eval(A.1) eval(A.2)
           │         │
           └────┬────┘
                ▼
           score(A) = max(avg(A.1), avg(A.2))

           ... similarly for B, C ...

                ┌──────────┐
                │  Select  │
                │  Best    │
                │  (beam   │
                │  search) │
                └──────────┘
```

### 6.2 Configuration

```rust
#[derive(Debug, Clone)]
pub struct TotConfig {
    /// Number of initial candidate plans to generate.
    pub num_candidates: usize,          // default: 3
    /// Maximum depth of the thought tree.
    pub max_depth: usize,               // default: 2
    /// Beam width for beam-search selection.
    pub beam_width: usize,              // default: 2
    /// LLM calls per evaluation.
    pub eval_samples: usize,            // default: 1
    /// Scoring function selection.
    pub scorer: TotScorer,              // default: LLM
}

pub enum TotScorer {
    /// Use the LLM to score each candidate.
    Llm,
    /// Use a heuristic: step count, constraint coverage, etc.
    Heuristic,
    /// Combine LLM and heuristic scores.
    Hybrid { llm_weight: f32, heuristic_weight: f32 },
}
```

### 6.3 Implementation Sketch

```rust
pub struct TreeOfThoughtPlanner {
    generator: LlmClient,
    evaluator: LlmClient,
    config: TotConfig,
}

#[derive(Debug)]
struct ThoughtNode {
    plan: Plan,
    score: Option<f32>,
    children: Vec<ThoughtNode>,
}

impl TreeOfThoughtPlanner {
    pub async fn plan(&self, intent: &Intent) -> Result<Plan, PlannerError> {
        // Phase 1: Generate initial candidates
        let candidates = self.generate_candidates(intent).await?;
        let mut roots: Vec<ThoughtNode> = candidates
            .into_iter()
            .map(|p| ThoughtNode { plan: p, score: None, children: vec![] })
            .collect();

        // Phase 2: Expand and evaluate (beam search)
        for depth in 0..self.config.max_depth {
            roots = self.beam_expand(roots, intent, depth).await?;
        }

        // Phase 3: Select best plan
        let best = self.select_best(roots)?;
        Ok(best.plan)
    }

    async fn generate_candidates(&self, intent: &Intent) -> Result<Vec<Plan>, PlannerError> {
        let mut candidates = Vec::with_capacity(self.config.num_candidates);
        for i in 0..self.config.num_candidates {
            let prompt = format!(
                "Generate plan candidate #{i} for the following intent.\n\
                 Be creative and consider a different approach than typical.\n\n\
                 {intent:#?}"
            );
            let response = self.generator.chat(prompt).await?;
            if let Some(plan) = self.extract_plan(&response)? {
                candidates.push(plan);
            }
        }
        Ok(candidates)
    }

    async fn beam_expand(
        &self,
        nodes: Vec<ThoughtNode>,
        intent: &Intent,
        depth: usize,
    ) -> Result<Vec<ThoughtNode>, PlannerError> {
        // Evaluate all leaf nodes
        let mut scored_nodes = Vec::new();
        for node in nodes {
            let score = self.evaluate(&node.plan, intent).await?;
            let mut scored_node = ThoughtNode { score: Some(score), ..node };

            // Expand top performers
            if depth < self.config.max_depth {
                let refinements = self.refine_plan(&scored_node.plan, intent).await?;
                for refined in refinements {
                    scored_node.children.push(ThoughtNode {
                        plan: refined,
                        score: None,
                        children: vec![],
                    });
                }
            }
            scored_nodes.push(scored_node);
        }

        // Beam search: keep top beam_width
        scored_nodes.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal)
        });
        scored_nodes.truncate(self.config.beam_width);

        // If children exist, recurse into them
        let mut result = Vec::new();
        for node in scored_nodes {
            if node.children.is_empty() {
                result.push(node);
            } else {
                result.extend(node.children);
            }
        }
        Ok(result)
    }

    fn select_best(&self, nodes: Vec<ThoughtNode>) -> Result<ThoughtNode, PlannerError> {
        nodes.into_iter()
            .filter_map(|n| n.score.map(|s| (n, s)))
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .map(|(n, _)| n)
            .ok_or(PlannerError::NoValidCandidates)
    }
}
```

---

## 7. ReplanTool — Mid-Execution Plan Revision

When a step fails or new information is discovered during execution, the agent can invoke the `ReplanTool` to revise the remaining plan.

### 7.1 Replan Trigger Taxonomy

```
 ┌─────────────────────────────────────────────────┐
 │               Replan Triggers                   │
 │                                                  │
 │  ┌──────────────┐  ┌─────────────────────────┐  │
 │  │ Step Failure │  │ New Information Discovered│ │
 │  │ (non-zero    │  │ (e.g., unexpected API,   │  │
 │  │  exit code)  │  │  missing dependency)     │  │
 │  └──────┬───────┘  └───────────┬─────────────┘  │
 │         │                      │                 │
 │  ┌──────┴──────────────────────┴──────────────┐  │
 │  │          Constraint Violation              │  │
 │  │   (runtime-detected constraint breach)     │  │
 │  └──────────────────┬────────────────────────┘  │
 │                     │                            │
 │                     ▼                            │
 │            ┌────────────────┐                    │
 │            │  ReplanTool    │                    │
 │            │  invocation    │                    │
 │            └────────────────┘                    │
 └─────────────────────────────────────────────────┘
```

### 7.2 ReplanTool Definition

```rust
/// Tool definition for mid-execution replanning.
pub struct ReplanTool;

#[derive(Debug, Serialize, Deserialize)]
pub struct ReplanInput {
    /// Why replanning is needed.
    pub reason: ReplanReason,
    /// What went wrong (for failure-triggered replans).
    pub failure_context: Option<FailureContext>,
    /// What new information was discovered.
    pub new_information: Option<String>,
    /// Which steps have already completed successfully.
    pub completed_steps: Vec<StepId>,
    /// The original intent (re-injected for context).
    pub original_intent: Intent,
    /// Optional hint about which planner to use.
    pub planner_hint: Option<PlannerType>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ReplanReason {
    StepFailed { step_id: StepId, error: String },
    NewInformation { summary: String },
    ConstraintViolation { constraint: String, detail: String },
    UserRequested { note: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FailureContext {
    pub step_description: String,
    pub tool_output: String,
    pub exit_code: Option<i32>,
    pub attempt_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PlannerType {
    OneShot,
    IterativeRefinement,
    TreeOfThought,
}

impl Tool for ReplanTool {
    type Input  = ReplanInput;
    type Output = Plan;

    fn name(&self) -> &str { "replan" }

    async fn execute(&self, input: Self::Input) -> Result<Self::Output, ToolError> {
        // Select planner based on hint or heuristic
        let planner: Box<dyn Planner> = match input.planner_hint {
            Some(PlannerType::IterativeRefinement) => {
                Box::new(IterativeRefinementPlanner::default())
            }
            Some(PlannerType::TreeOfThought) => {
                Box::new(TreeOfThoughtPlanner::default())
            }
            _ => Box::new(OneShotPlanner::default()),
        };

        // Augment intent with replan context
        let augmented_intent = Intent {
            goal: format!(
                "[REPLAN] {} — Reason: {:?}",
                input.original_intent.goal, input.reason
            ),
            constraints: input.original_intent.constraints.clone(),
            preferences: input.original_intent.preferences.clone(),
            acceptance_criteria: input.original_intent.acceptance_criteria.clone(),
            prior_context: Some(PriorContext {
                completed_steps: input.completed_steps,
                failure_context: input.failure_context,
                new_information: input.new_information,
            }),
        };

        let new_plan = planner.plan(&augmented_intent).await?;

        // Mark already-completed steps as Done in the new plan
        let adjusted = new_plan.adjust_for_completed(&input.completed_steps);
        Ok(adjusted)
    }
}
```

### 7.3 Replan Policy

Not every failure should trigger a replan. The policy determines when replanning is appropriate.

```rust
pub struct ReplanPolicy {
    /// Maximum consecutive replans before aborting.
    pub max_replans: u32,              // default: 3
    /// Minimum steps that must have succeeded before allowing replan.
    pub min_progress_before_replan: u32, // default: 1
    /// Whether to allow replan on the very first step failure.
    pub allow_first_step_replan: bool,  // default: true
    /// Cool-down period between replans (in seconds).
    pub cooldown_secs: u64,             // default: 0
}

impl ReplanPolicy {
    pub fn should_replan(&self, ctx: &ReplanContext) -> bool {
        if ctx.total_replans >= self.max_replans {
            tracing::warn!("Replan limit reached ({})", self.max_replans);
            return false;
        }
        if ctx.completed_step_count < self.min_progress_before_replan as usize
            && !self.allow_first_step_replan
        {
            return false;
        }
        true
    }
}
```

---

## 8. Planning ↔ TaskRunner Contract

### 8.1 Data Flow

```
 ┌──────────┐     Plan        ┌────────────┐    StepResult    ┌──────────┐
 │ Planner  │ ──────────────▶ │ TaskRunner │ ◀─────────────── │ Step     │
 └──────────┘                 └─────┬──────┘                  │ Executor │
                                    │                         └──────────┘
                                    │  on StepResult::Failed
                                    │  or StepResult::NeedsReplan
                                    │
                                    ▼
                             ┌────────────┐
                             │ ReplanTool │
                             └─────┬──────┘
                                   │  new Plan
                                   ▼
                             ┌────────────┐
                             │ TaskRunner │  ← replaces remaining steps
                             └────────────┘
```

### 8.2 Plan Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub steps: Vec<Step>,
    pub intent_hash: u64, // hash of the originating Intent
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub description: String,
    pub tool: ToolHint,
    pub dependencies: Vec<StepId>,
    pub rollback: Option<RollbackAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolHint {
    FileEdit { path: PathBuf },
    ShellCommand { command: String },
    Replan,
    Unspecified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackAction {
    pub description: String,
    pub strategy: RollbackStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RollbackStrategy {
    GitRestore { path: PathBuf },
    GitRevert { commit_sha: String },
    ShellCommand { command: String },
    Manual,
}
```

### 8.3 TaskRunner Interface

```rust
pub struct TaskRunner {
    plan: Plan,
    executor: StepExecutor,
    replan_policy: ReplanPolicy,
    completed: Vec<(StepId, StepResult)>,
    replan_count: u32,
}

impl TaskRunner {
    pub async fn run(&mut self) -> Result<RunSummary, RunnerError> {
        let step_order = self.topological_sort()?;

        for step_id in step_order {
            let step = self.plan.find_step(&step_id)?;
            let result = self.executor.execute(&step).await;

            match result {
                StepResult::Success(output) => {
                    self.completed.push((step_id, StepResult::Success(output)));
                }
                StepResult::Failed { error, recoverable } => {
                    if recoverable && self.replan_policy.should_replan(&self.replan_context()) {
                        let new_plan = self.invoke_replan(&step, &error).await?;
                        self.plan = new_plan;
                        self.replan_count += 1;
                        // Continue with the new plan
                        return self.run().await;
                    } else {
                        self.rollback_completed()?;
                        return Err(RunnerError::StepFailed { step_id, error });
                    }
                }
                StepResult::NeedsReplan { reason } => {
                    let new_plan = self.invoke_replan_with_reason(&step, reason).await?;
                    self.plan = new_plan;
                    self.replan_count += 1;
                    return self.run().await;
                }
            }
        }

        Ok(RunSummary {
            plan_id: self.plan.id,
            steps_completed: self.completed.len(),
            total_steps: self.plan.steps.len(),
            replans: self.replan_count,
        })
    }

    fn topological_sort(&self) -> Result<Vec<StepId>, RunnerError> {
        // Kahn's algorithm on step dependencies
        let mut in_degree: HashMap<StepId, usize> = HashMap::new();
        let mut adj: HashMap<StepId, Vec<StepId>> = HashMap::new();

        for step in &self.plan.steps {
            in_degree.entry(step.id).or_insert(0);
            for dep in &step.dependencies {
                *in_degree.entry(step.id).or_insert(0) += 1;
                adj.entry(*dep).or_default().push(step.id);
            }
        }

        let mut queue: VecDeque<StepId> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut order = Vec::new();
        while let Some(id) = queue.pop_front() {
            order.push(id);
            for &neighbor in adj.get(&id).unwrap_or(&vec![]) {
                let deg = in_degree.get_mut(&neighbor).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(neighbor);
                }
            }
        }

        if order.len() != self.plan.steps.len() {
            return Err(RunnerError::CyclicDependency);
        }
        Ok(order)
    }
}
```

---

## 9. Planner Selection Heuristic

`xaft` selects a planner based on task complexity signals:

```rust
pub fn select_planner(intent: &Intent) -> PlannerType {
    let complexity = estimate_complexity(intent);

    match complexity {
        c if c < 0.3  => PlannerType::OneShot,
        c if c < 0.7  => PlannerType::IterativeRefinement,
        _             => PlannerType::TreeOfThought,
    }
}

fn estimate_complexity(intent: &Intent) -> f32 {
    let mut score = 0.0;

    // Goal length heuristic
    score += (intent.goal.len() as f32 / 500.0).min(0.2);

    // Number of constraints
    score += (intent.constraints.len() as f32 * 0.1).min(0.3);

    // Ambiguity signals (question marks, vague words)
    let vague = ["maybe", "perhaps", "somehow", "figure out", "not sure"];
    for v in &vague {
        if intent.goal.to_lowercase().contains(v) { score += 0.1; }
    }

    // Acceptance criteria count (more = more complex)
    score += (intent.acceptance_criteria.len() as f32 * 0.05).min(0.2);

    score.min(1.0)
}
```

---

## 10. Configuration

```toml
# .xaft.toml

[planning]
default_planner = "oneshot"          # oneshot | iterative | tree_of_thought
auto_select = true                   # auto-select planner based on complexity

[planning.oneshot]
max_tokens = 4096
temperature = 0.2
enable_tier2 = true
enable_tier3 = true

[planning.iterative]
max_iterations = 3
quality_threshold = 0.80

[planning.tree_of_thought]
num_candidates = 3
max_depth = 2
beam_width = 2
scorer = "hybrid"
llm_weight = 0.6
heuristic_weight = 0.4

[planning.replan]
max_replans = 3
min_progress_before_replan = 1
allow_first_step_replan = true
cooldown_secs = 5
```

---

## 11. Error Taxonomy

| Error                          | Code   | Recovery                                |
|--------------------------------|--------|-----------------------------------------|
| `IntentError::NoAcceptanceCriteria` | P-001 | Require user to specify at least one    |
| `PlannerError::ExtractionFailed`    | P-002 | Retry with next tier or different planner |
| `PlannerError::RevisionFailed`      | P-003 | Fall back to original draft plan        |
| `PlannerError::NoValidCandidates`   | P-004 | Retry with more candidates or oneshot   |
| `RunnerError::CyclicDependency`     | P-005 | Report to user; request plan fix        |
| `RunnerError::StepFailed`           | P-006 | Rollback + optional replan              |
| `ReplanLimitReached`                | P-007 | Abort; present diagnosis to user        |

---

## 12. Future Considerations

1. **Streaming plan generation** — Emit partial plans as they are constructed for TUI preview.
2. **Plan caching** — Cache plans keyed by intent hash to avoid redundant LLM calls.
3. **Multi-agent planning** — Allow separate planning and execution agents with different system prompts.
4. **Plan diffing** — When replanning, show the user a structured diff between old and new plans.
5. **Confidence scores** — Attach per-step confidence estimates to help the user judge risk.
