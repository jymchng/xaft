# Task Graph Execution

> Deep dive into `TaskRunner`: the state machine, plan generation, checkpoint
> resumability, replanning, approval gates, retry semantics, and the
> `TaskStore` persistence trait.

---

## 1. Overview

The `TaskRunner` is xauft's execution engine. It takes a user prompt, asks a
planner to decompose it into a `Plan` of `PlanStep`s, then drives each step
through a rigorous state machine with checkpoints, approval gates, and
retry semantics.

```
┌──────────┐     ┌──────────┐     ┌────────────────┐     ┌──────────┐
│  User    │────▶│ TaskRunner│────▶│    Planner     │────▶│  Plan    │
│  Prompt  │     │          │     │ (OneShot/      │     │ (Steps)  │
│          │     │          │     │  Iterative/    │     │          │
│          │     │          │     │  TreeOfThought)│     │          │
└──────────┘     └────┬─────┘     └────────────────┘     └──────────┘
                      │                                         │
                      │          drives step execution          │
                      │◀────────────────────────────────────────┘
                      │
                      ▼
              ┌───────────────┐
              │  State Machine│
              │  per step     │
              │               │
              │  Received     │
              │  Planned      │
              │  Running      │
              │  Suspended    │
              │  AwaitApproval│
              │  Completed    │
              │  Failed       │
              │  Cancelled    │
              └───────────────┘
```

---

## 2. State Machine

Every `PlanStep` transitions through the following states:

```
                              ┌─────────────┐
                              │  Received   │
                              └──────┬──────┘
                                     │
                                     │ planner assigns step
                                     ▼
                              ┌─────────────┐
                              │  Planned    │
                              └──────┬──────┘
                                     │
                                     │ executor picks up step
                                     ▼
                              ┌─────────────┐
                        ┌────▶│  Running    │◀───────┐
                        │     └──┬───┬───┬──┘        │
                        │        │   │   │            │
                        │        │   │   │            │
                 retry  │        │   │   │ await      │
                 from   │        │   │   │ approval   │  resume
                 checkpt│        │   │   │            │
                        │        │   │   ▼            │
                        │        │   │  ┌────────────┐│
                        │        │   │  │ Awaiting   ││
                        │        │   │  │ Approval   ││
                        │        │   │  └─────┬──────┘│
                        │        │   │        │       │
                        │        │   │   approved     │
                        │        │   │        │       │
                        │        │   │        └───────┘
                        │        │   │
                        │        │   │ suspended (checkpoint)
                        │        │   ▼
                        │        │  ┌────────────┐
                        │        │  │ Suspended  │
                        │        │  └─────┬──────┘
                        │        │        │
                        │        │   resume from checkpoint
                        │        │        │
                        │        └────────┘
                        │
                        │   ┌────────────┐
                        └───│  Failed    │
                            └────────────┘

                     ┌────────────┐
                     │ Completed  │   ← terminal (success)
                     └────────────┘

                     ┌────────────┐
                     │ Cancelled  │   ← terminal (user abort)
                     └────────────┘
```

### 2.1 State Enum

```rust
/// The lifecycle state of a single PlanStep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Step has been received but not yet planned.
    Received,

    /// Planner has assigned this step to a plan.
    Planned,

    /// Step is currently being executed by an agent.
    Running,

    /// Step is suspended; a checkpoint has been saved.
    /// Can be resumed from the checkpoint.
    Suspended,

    /// Step is waiting for user approval before proceeding.
    AwaitingApproval,

    /// Step completed successfully.
    Completed,

    /// Step failed. May be retried from last checkpoint.
    Failed,

    /// Step was cancelled by user request.
    Cancelled,
}

impl StepState {
    /// Whether this state is terminal (no further transitions).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Whether this state is resumable.
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::Suspended | Self::AwaitingApproval)
    }
}
```

### 2.2 State Transition Rules

```rust
impl StepState {
    /// Validate a state transition. Returns Ok if valid, Err otherwise.
    pub fn validate_transition(&self, next: StepState) -> Result<(), InvalidTransition> {
        match (self, next) {
            // Forward transitions
            (Self::Received, Self::Planned) => Ok(()),
            (Self::Planned, Self::Running) => Ok(()),
            (Self::Running, Self::Completed) => Ok(()),
            (Self::Running, Self::Failed) => Ok(()),
            (Self::Running, Self::Cancelled) => Ok(()),
            (Self::Running, Self::Suspended) => Ok(()),
            (Self::Running, Self::AwaitingApproval) => Ok(()),

            // Resume transitions
            (Self::Suspended, Self::Running) => Ok(()),
            (Self::Suspended, Self::Cancelled) => Ok(()),
            (Self::AwaitingApproval, Self::Running) => Ok(()),   // approved
            (Self::AwaitingApproval, Self::Cancelled) => Ok(()),  // rejected

            // Retry transition
            (Self::Failed, Self::Running) => Ok(()),  // retry from checkpoint
            (Self::Failed, Self::Cancelled) => Ok(()),

            // Invalid transitions
            (Self::Completed, _) => Err(InvalidTransition {
                from: *self, to: next,
                reason: "Completed steps cannot transition".into(),
            }),
            (Self::Cancelled, _) => Err(InvalidTransition {
                from: *self, to: next,
                reason: "Cancelled steps cannot transition".into(),
            }),
            _ => Err(InvalidTransition {
                from: *self, to: next,
                reason: format!("Invalid transition from {:?} to {:?}", self, next),
            }),
        }
    }
}
```

### 2.3 Task-Level State

The overall `Task` also has a state derived from its steps:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// Task received, not yet planned.
    Received,
    /// Planner has generated a plan.
    Planned,
    /// One or more steps are executing.
    Running,
    /// All steps suspended or awaiting approval.
    Suspended,
    /// All steps completed.
    Completed,
    /// One or more steps failed (unrecoverable).
    Failed,
    /// User cancelled the task.
    Cancelled,
}

impl TaskState {
    /// Derive task state from the states of its steps.
    pub fn from_steps(steps: &[PlanStep]) -> TaskState {
        if steps.is_empty() {
            return TaskState::Received;
        }
        let states: Vec<_> = steps.iter().map(|s| s.state).collect();

        if states.iter().all(|s| *s == StepState::Received) {
            return TaskState::Received;
        }
        if states.iter().all(|s| *s == StepState::Planned) {
            return TaskState::Planned;
        }
        if states.iter().all(|s| *s == StepState::Completed) {
            return TaskState::Completed;
        }
        if states.iter().any(|s| *s == StepState::Failed) {
            return TaskState::Failed;
        }
        if states.iter().any(|s| *s == StepState::Running) {
            return TaskState::Running;
        }
        if states.iter().all(|s| s.is_resumable()) {
            return TaskState::Suspended;
        }
        TaskState::Running
    }
}
```

---

## 3. Plan and PlanStep

### 3.1 Data Structures

```rust
/// A complete execution plan generated by a planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Unique plan identifier.
    pub id: PlanId,
    /// The task this plan addresses.
    pub task_id: TaskId,
    /// Ordered list of steps.
    pub steps: Vec<PlannedStep>,
    /// Planner that generated this plan.
    pub planner: PlannerType,
    /// Timestamp of plan creation.
    pub created_at: DateTime<Utc>,
    /// Plan revision number (incremented on replan).
    pub revision: u32,
}

/// A single step in an execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedStep {
    /// Unique step identifier.
    pub id: StepId,
    /// Human-readable description of what this step does.
    pub description: String,
    /// The agent role that should execute this step.
    pub assigned_role: AgentRole,
    /// Tools available to the agent for this step.
    pub available_tools: Vec<String>,
    /// Dependencies on other steps (must complete before this one).
    pub depends_on: Vec<StepId>,
    /// Current state.
    pub state: StepState,
    /// Checkpoint data (if suspended/failed).
    pub checkpoint: Option<Checkpoint>,
    /// Number of times this step has been retried.
    pub retry_count: u32,
    /// Maximum retries allowed.
    pub max_retries: u32,
    /// Risk level (determines approval requirements).
    pub risk: RiskLevel,
    /// Result (if completed).
    pub result: Option<StepResult>,
    /// Estimated token cost.
    pub estimated_cost: Option<TokenEstimate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Read-only operations. No approval needed.
    Low,
    /// Modifies files but in non-critical paths.
    Medium,
    /// Modifies critical files, runs shell commands, or deploys.
    High,
    /// Destructive operations (delete files, force-push, etc.).
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The step state at checkpoint time.
    pub state: StepState,
    /// Conversation history at checkpoint time.
    pub conversation: Vec<Message>,
    /// Tool call results at checkpoint time.
    pub tool_results: HashMap<ToolCallId, ToolOutput>,
    /// Files modified so far in this step.
    pub modified_files: Vec<PathBuf>,
    /// Timestamp.
    pub saved_at: DateTime<Utc>,
    /// Token usage at checkpoint time.
    pub token_usage: TokenUsage,
}
```

### 3.2 Step Dependency Graph

Steps form a DAG (directed acyclic graph). Independent steps can execute in
parallel; dependent steps wait.

```
  Plan: "Refactor auth module"

  Step 1: Analyze auth module        (no deps)
      │
      ├──────────────────────────┐
      ▼                          ▼
  Step 2: Write unit tests    Step 3: Refactor core
      │                          │
      │                          │
      └──────────┬───────────────┘
                 ▼
  Step 4: Run tests              (depends on 2, 3)
                 │
                 ▼
  Step 5: Update docs            (depends on 4)
```

```rust
impl Plan {
    /// Get steps that are ready to execute (all deps completed).
    pub fn ready_steps(&self) -> Vec<&PlannedStep> {
        let completed: HashSet<StepId> = self.steps.iter()
            .filter(|s| s.state == StepState::Completed)
            .map(|s| s.id)
            .collect();

        self.steps.iter()
            .filter(|s| s.state == StepState::Planned)
            .filter(|s| s.depends_on.iter().all(|dep| completed.contains(dep)))
            .collect()
    }

    /// Check if all steps are in a terminal state.
    pub fn is_finished(&self) -> bool {
        self.steps.iter().all(|s| s.state.is_terminal())
    }

    /// Get the critical path (longest dependency chain).
    pub fn critical_path(&self) -> Vec<StepId> {
        // Topological sort with longest-path calculation
        let mut distances: HashMap<StepId, usize> = HashMap::new();
        let mut predecessor: HashMap<StepId, StepId> = HashMap::new();

        for step in &self.steps {
            let dep_dist = step.depends_on.iter()
                .filter_map(|d| distances.get(d))
                .max()
                .copied()
                .unwrap_or(0);
            distances.insert(step.id, dep_dist + 1);

            if let Some(max_dep) = step.depends_on.iter()
                .max_by_key(|d| distances.get(d).copied().unwrap_or(0))
            {
                predecessor.insert(step.id, *max_dep);
            }
        }

        // Trace back from the step with max distance
        let end = distances.iter()
            .max_by_key(|(_, &d)| d)
            .map(|(&id, _)| id);

        let mut path = Vec::new();
        if let Some(mut current) = end {
            path.push(current);
            while let Some(&pred) = predecessor.get(&current) {
                path.push(pred);
                current = pred;
            }
            path.reverse();
        }
        path
    }
}
```

---

## 4. Planner Implementations

### 4.1 Planner Trait

```rust
/// Trait for plan generation strategies.
#[async_trait]
pub trait Planner: Send + Sync {
    /// Generate a plan for the given task.
    async fn plan(&self, task: &Task) -> Result<Plan, PlannerError>;

    /// Revise an existing plan (mid-execution replanning).
    async fn replan(
        &self,
        task: &Task,
        current_plan: &Plan,
        reason: &ReplanReason,
    ) -> Result<Plan, PlannerError>;

    fn planner_type(&self) -> PlannerType;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlannerType {
    OneShot,
    IterativeRefinement,
    TreeOfThought,
}
```

### 4.2 OneShotPlanner

Generates the plan in a single LLM call using structured output.

```rust
pub struct OneShotPlanner<P: LlmProvider> {
    provider: P,
    model: String,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct PlanOutput {
    steps: Vec<PlannedStepOutput>,
    reasoning: String,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct PlannedStepOutput {
    description: String,
    assigned_role: String,
    depends_on: Vec<String>,
    risk: String,
    available_tools: Vec<String>,
}

#[async_trait]
impl<P: LlmProvider> Planner for OneShotPlanner<P> {
    async fn plan(&self, task: &Task) -> Result<Plan, PlannerError> {
        let system_prompt = r#"
You are a task planner for xauft, an autonomous coding agent.
Decompose the given task into a sequence of concrete steps.

Rules:
1. Each step should be independently executable by a specialised agent.
2. Specify dependencies explicitly (step IDs from prior steps).
3. Assign the most appropriate agent role to each step.
4. Assess risk level (low/medium/high/critical) for each step.
5. List the tools each step needs.

Output your plan as structured JSON conforming to the output schema.
"#;

        let structured = StructuredLlm::<PlanOutput>::new(
            &self.provider,
            &self.model,
            system_prompt,
        );

        let output = structured.generate(&task.description).await?;

        // Convert to Plan
        let mut steps = Vec::new();
        for (i, step_out) in output.steps.iter().enumerate() {
            let step_id = StepId::from_index(i);
            steps.push(PlannedStep {
                id: step_id,
                description: step_out.description.clone(),
                assigned_role: AgentRole::from_str(&step_out.assigned_role)?,
                available_tools: step_out.available_tools.clone(),
                depends_on: step_out.depends_on.iter()
                    .filter_map(|d| StepId::parse(d))
                    .collect(),
                state: StepState::Planned,
                checkpoint: None,
                retry_count: 0,
                max_retries: 3,
                risk: RiskLevel::from_str(&step_out.risk)?,
                result: None,
                estimated_cost: None,
            });
        }

        Ok(Plan {
            id: PlanId::new(),
            task_id: task.id,
            steps,
            planner: PlannerType::OneShot,
            created_at: Utc::now(),
            revision: 0,
        })
    }

    async fn replan(
        &self,
        task: &Task,
        current_plan: &Plan,
        reason: &ReplanReason,
    ) -> Result<Plan, PlannerError> {
        // OneShot replanner: regenerate from scratch with context
        let context = format!(
            "Original task: {}\n\nCurrent plan (revision {}):\n{}\n\n\
             Replan reason: {:?}",
            task.description,
            current_plan.revision,
            current_plan.format_steps(),
            reason,
        );
        // ... same structured generation with added context
        let structured = StructuredLlm::<PlanOutput>::new(
            &self.provider, &self.model, REPLANNER_SYSTEM_PROMPT,
        );
        let output = structured.generate(&context).await?;
        // Convert, incrementing revision
        let mut new_plan = self.plan(task).await?;
        new_plan.revision = current_plan.revision + 1;
        Ok(new_plan)
    }

    fn planner_type(&self) -> PlannerType { PlannerType::OneShot }
}
```

### 4.3 IterativeRefinementPlanner

Generates an initial plan, then iteratively refines it through self-critique.

```rust
pub struct IterativeRefinementPlanner<P: LlmProvider> {
    provider: P,
    model: String,
    /// Number of refinement iterations.
    iterations: usize,
}

#[async_trait]
impl<P: LlmProvider> Planner for IterativeRefinementPlanner<P> {
    async fn plan(&self, task: &Task) -> Result<Plan, PlannerError> {
        // Iteration 0: generate initial plan
        let mut current_plan = self.generate_initial(task).await?;

        for i in 0..self.iterations {
            // Critique phase
            let critique = self.critique(task, &current_plan).await?;

            if critique.approved {
                break; // Plan is good enough
            }

            // Refine phase
            current_plan = self.refine(task, &current_plan, &critique).await?;
        }

        Ok(current_plan)
    }
}

#[derive(Debug, JsonSchema, Deserialize)]
struct CritiqueOutput {
    approved: bool,
    issues: Vec<String>,
    suggestions: Vec<String>,
    missing_considerations: Vec<String>,
}
```

### 4.4 TreeOfThoughtPlanner

Explores multiple plan candidates in parallel, then selects the best.

```rust
pub struct TreeOfThoughtPlanner<P: LlmProvider> {
    provider: P,
    model: String,
    /// Number of candidate branches to explore.
    branching_factor: usize,
    /// Maximum depth of the thought tree.
    max_depth: usize,
}

#[async_trait]
impl<P: LlmProvider> Planner for TreeOfThoughtPlanner<P> {
    async fn plan(&self, task: &Task) -> Result<Plan, PlannerError> {
        let mut candidates: Vec<PlanCandidate> = Vec::new();

        // Level 0: Generate initial candidates
        for _ in 0..self.branching_factor {
            let plan = self.generate_initial(task).await?;
            candidates.push(PlanCandidate {
                plan,
                score: None,
                depth: 0,
            });
        }

        // Expand and evaluate
        for depth in 0..self.max_depth {
            // Score all candidates
            for candidate in &mut candidates {
                if candidate.score.is_none() {
                    candidate.score = Some(self.evaluate(task, &candidate.plan).await?);
                }
            }

            // Keep top-K candidates
            candidates.sort_by(|a, b| {
                b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal)
            });
            candidates.truncate(self.branching_factor);

            // Expand each candidate
            if depth < self.max_depth - 1 {
                let mut expanded = Vec::new();
                for candidate in &candidates {
                    for _ in 0..self.branching_factor {
                        let variant = self.vary(task, &candidate.plan).await?;
                        expanded.push(PlanCandidate {
                            plan: variant,
                            score: None,
                            depth: depth + 1,
                        });
                    }
                }
                candidates = expanded;
            }
        }

        // Return best candidate
        candidates.into_iter()
            .max_by_key(|c| c.score)
            .map(|c| c.plan)
            .ok_or(PlannerError::NoViablePlan)
    }
}
```

---

## 5. TaskRunner

### 5.1 Core Implementation

```rust
pub struct TaskRunner<P: LlmProvider> {
    provider: Arc<P>,
    planner: Box<dyn Planner>,
    store: Arc<dyn TaskStore>,
    bus: AgentMessageBus,
    pool: SubagentPool<P>,
    config: TaskRunnerConfig,
}

#[derive(Debug, Clone)]
pub struct TaskRunnerConfig {
    /// Maximum concurrent step executions.
    pub max_concurrent_steps: usize,
    /// Default step timeout.
    pub step_timeout: Duration,
    /// Checkpoint interval (save every N tool calls).
    pub checkpoint_interval: usize,
    /// Whether to require approval for high-risk steps.
    pub approval_for_high_risk: bool,
    /// Whether to require approval for critical-risk steps.
    pub approval_for_critical: bool,
    /// Maximum replan attempts.
    pub max_replans: u32,
}

impl<P: LlmProvider + Clone + 'static> TaskRunner<P> {
    /// Submit a new task and begin execution.
    pub async fn submit(&self, task: Task) -> Result<TaskId, RunnerError> {
        let task_id = task.id;
        self.store.save_task(&task).await?;

        // Transition to Planned
        let plan = self.planner.plan(&task).await?;
        self.store.save_plan(&plan).await?;

        // Begin execution loop
        let runner = self.clone();
        tokio::spawn(async move {
            if let Err(e) = runner.run_loop(task_id).await {
                runner.store.update_task_state(task_id, TaskState::Failed).await.ok();
                tracing::error!("Task runner failed: {:?}", e);
            }
        });

        Ok(task_id)
    }

    /// Main execution loop.
    async fn run_loop(&self, task_id: TaskId) -> Result<(), RunnerError> {
        loop {
            let plan = self.store.load_plan(task_id).await?;

            if plan.is_finished() {
                let state = TaskState::from_steps(&plan.steps);
                self.store.update_task_state(task_id, state).await?;
                return Ok(());
            }

            // Get steps ready to execute
            let ready = plan.ready_steps();
            if ready.is_empty() {
                // All remaining steps are blocked — this is a deadlock
                return Err(RunnerError::Deadlock {
                    task_id,
                    blocked_steps: plan.steps.iter()
                        .filter(|s| !s.state.is_terminal())
                        .map(|s| s.id)
                        .collect(),
                });
            }

            // Execute ready steps (respecting concurrency limits)
            let mut join_set = JoinSet::new();
            for step in ready {
                let step_id = step.id;
                let runner = self.clone();
                join_set.spawn(async move {
                    runner.execute_step(task_id, step_id).await
                });
            }

            // Wait for at least one step to complete
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(Ok(())) => { /* step completed */ }
                    Ok(Err(e)) => {
                        tracing::warn!("Step failed: {:?}", e);
                        // Continue — other steps may still succeed
                    }
                    Err(_) => { /* join error */ }
                }
            }
        }
    }

    /// Execute a single step.
    async fn execute_step(
        &self,
        task_id: TaskId,
        step_id: StepId,
    ) -> Result<(), RunnerError> {
        let mut plan = self.store.load_plan(task_id).await?;
        let step = plan.steps.iter_mut().find(|s| s.id == step_id)
            .ok_or(RunnerError::StepNotFound(step_id))?;

        // Check if approval is needed
        if self.needs_approval(&step.risk) {
            step.state = StepState::AwaitingApproval;
            self.store.save_plan(&plan).await?;
            return Ok(()); // Will be resumed when approval comes in
        }

        // Transition to Running
        step.state.validate_transition(StepState::Running)?;
        step.state = StepState::Running;
        self.store.save_plan(&plan).await?;

        // Acquire agent from pool
        let agent = self.pool.acquire_agent(step.assigned_role).await
            .map_err(RunnerError::PoolError)?;

        // Execute with timeout and checkpointing
        let result = tokio::time::timeout(
            self.config.step_timeout,
            self.execute_with_checkpoints(agent, &step),
        ).await;

        match result {
            Ok(Ok(step_result)) => {
                // Success
                let mut plan = self.store.load_plan(task_id).await?;
                let step = plan.steps.iter_mut().find(|s| s.id == step_id).unwrap();
                step.state = StepState::Completed;
                step.result = Some(step_result);
                self.store.save_plan(&plan).await?;
                Ok(())
            }
            Ok(Err(e)) => {
                // Agent error — save checkpoint and retry or fail
                let mut plan = self.store.load_plan(task_id).await?;
                let step = plan.steps.iter_mut().find(|s| s.id == step_id).unwrap();
                step.checkpoint = Some(Checkpoint {
                    state: StepState::Failed,
                    conversation: vec![], // would be populated
                    tool_results: HashMap::new(),
                    modified_files: vec![],
                    saved_at: Utc::now(),
                    token_usage: TokenUsage::default(),
                });
                step.retry_count += 1;

                if step.retry_count <= step.max_retries {
                    step.state = StepState::Running; // will retry
                } else {
                    step.state = StepState::Failed;
                }
                self.store.save_plan(&plan).await?;
                Err(RunnerError::StepFailed { step_id, source: e })
            }
            Err(_) => {
                // Timeout — suspend with checkpoint
                let mut plan = self.store.load_plan(task_id).await?;
                let step = plan.steps.iter_mut().find(|s| s.id == step_id).unwrap();
                step.state = StepState::Suspended;
                step.checkpoint = Some(Checkpoint {
                    state: StepState::Suspended,
                    conversation: vec![],
                    tool_results: HashMap::new(),
                    modified_files: vec![],
                    saved_at: Utc::now(),
                    token_usage: TokenUsage::default(),
                });
                self.store.save_plan(&plan).await?;
                Err(RunnerError::StepTimeout { step_id, timeout: self.config.step_timeout })
            }
        }
    }

    fn needs_approval(&self, risk: &RiskLevel) -> bool {
        match risk {
            RiskLevel::Low => false,
            RiskLevel::Medium => false,
            RiskLevel::High => self.config.approval_for_high_risk,
            RiskLevel::Critical => self.config.approval_for_critical,
        }
    }
}
```

---

## 6. Checkpoints and Resumability

### 6.1 Checkpoint Strategy

Checkpoints are saved at:

1. **Step boundaries** — before each step starts.
2. **Tool call intervals** — every N tool calls within a step.
3. **On suspension** — when a step is suspended or awaits approval.
4. **On failure** — before retry, to enable resumption.

```
  Step Execution Timeline:
  ──────────────────────────────────────────────────▶ time
  │  start  │  tool_1  │  tool_2  │  tool_3  │  end │
  │    ●    │    ●     │    ●     │    ●     │  ●   │
  │  CP-0   │  CP-1   │  CP-2   │  CP-3   │  CP-4 │
  │         │          │          │          │       │
  │  ▶ can  │  ▶ can   │  ▶ can   │  ▶ can   │ done  │
  │  resume │  resume  │  resume  │  resume  │       │
  │  here   │  here    │  here    │  here    │       │
```

### 6.2 Checkpoint Persistence

```rust
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Save a checkpoint.
    async fn save(&self, task_id: TaskId, step_id: StepId, checkpoint: &Checkpoint)
        -> Result<(), StoreError>;

    /// Load the latest checkpoint for a step.
    async fn load_latest(
        &self,
        task_id: TaskId,
        step_id: StepId,
    ) -> Result<Option<Checkpoint>, StoreError>;

    /// List all checkpoints for a task.
    async fn list(&self, task_id: TaskId) -> Result<Vec<(StepId, Checkpoint)>, StoreError>;

    /// Delete checkpoints older than a given duration.
    async fn cleanup(&self, older_than: Duration) -> Result<usize, StoreError>;
}
```

### 6.3 Resume from Checkpoint

```rust
impl<P: LlmProvider + Clone + 'static> TaskRunner<P> {
    /// Resume a suspended task from its last checkpoint.
    pub async fn resume(&self, task_id: TaskId) -> Result<(), RunnerError> {
        let plan = self.store.load_plan(task_id).await?;

        for step in &plan.steps {
            if step.state == StepState::Suspended {
                if let Some(checkpoint) = &step.checkpoint {
                    // Restore agent context from checkpoint
                    let agent = self.pool.acquire_agent(step.assigned_role).await?;
                    agent.restore_context(&checkpoint.conversation, &checkpoint.tool_results)?;

                    // Re-execute step from checkpoint
                    self.execute_step(task_id, step.id).await?;
                }
            }
        }

        // Continue the main run loop
        self.run_loop(task_id).await
    }

    /// Resume a specific step that was awaiting approval.
    pub async fn approve_step(
        &self,
        task_id: TaskId,
        step_id: StepId,
        approval: ApprovalDecision,
    ) -> Result<(), RunnerError> {
        let mut plan = self.store.load_plan(task_id).await?;
        let step = plan.steps.iter_mut().find(|s| s.id == step_id)
            .ok_or(RunnerError::StepNotFound(step_id))?;

        match approval {
            ApprovalDecision::Approved => {
                step.state.validate_transition(StepState::Running)?;
                step.state = StepState::Running;
                self.store.save_plan(&plan).await?;
                self.execute_step(task_id, step_id).await
            }
            ApprovalDecision::Rejected => {
                step.state.validate_transition(StepState::Cancelled)?;
                step.state = StepState::Cancelled;
                self.store.save_plan(&plan).await?;
                Ok(())
            }
            ApprovalDecision::RequestChanges { feedback } => {
                // Inject feedback into step context and replan
                let reason = ReplanReason::UserFeedback { step_id, feedback };
                let new_plan = self.planner.replan(
                    &self.store.load_task(task_id).await?,
                    &plan,
                    &reason,
                ).await?;
                self.store.save_plan(&new_plan).await?;
                Ok(())
            }
        }
    }
}
```

---

## 7. ReplanTool: Mid-Execution Plan Revision

### 7.1 Motivation

During execution, agents may discover that the original plan is inadequate:
unexpected complexity, new dependencies, or failed steps. The `ReplanTool`
allows agents to request a plan revision mid-execution.

```rust
/// Tool that allows an agent to request plan revision.
pub struct ReplanTool {
    planner: Arc<dyn Planner>,
    store: Arc<dyn TaskStore>,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct ReplanInput {
    /// Why the current plan needs revision.
    reason: String,
    /// Which steps are problematic.
    affected_steps: Vec<StepId>,
    /// Suggested modifications (optional).
    suggestions: Vec<String>,
    /// Whether to add new steps, remove steps, or both.
    action: ReplanAction,
}

#[derive(Debug, JsonSchema, Deserialize)]
enum ReplanAction {
    AddSteps,
    RemoveSteps,
    ModifySteps,
    FullReplan,
}

#[derive(Debug, JsonSchema, Serialize)]
struct ReplanOutput {
    new_plan_revision: u32,
    added_steps: Vec<StepId>,
    removed_steps: Vec<StepId>,
    modified_steps: Vec<StepId>,
}

#[async_trait]
impl Tool for ReplanTool {
    fn name(&self) -> &str { "replan" }
    fn description(&self) -> &str {
        "Request revision of the current execution plan. Use when you discover \
         that the current plan is inadequate or when unexpected complications arise."
    }

    async fn call(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let input: ReplanInput = serde_json::from_value(input)?;

        let task = self.store.load_current_task().await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let current_plan = self.store.load_current_plan().await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let reason = ReplanReason::AgentRequested {
            agent_reason: input.reason,
            affected_steps: input.affected_steps,
        };

        let new_plan = self.planner.replan(&task, &current_plan, &reason).await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        self.store.save_plan(&new_plan).await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        Ok(ToolOutput::Json(serde_json::to_value(ReplanOutput {
            new_plan_revision: new_plan.revision,
            added_steps: new_plan.steps.iter()
                .filter(|s| !current_plan.steps.iter().any(|os| os.id == s.id))
                .map(|s| s.id)
                .collect(),
            removed_steps: current_plan.steps.iter()
                .filter(|s| !new_plan.steps.iter().any(|ns| ns.id == s.id))
                .map(|s| s.id)
                .collect(),
            modified_steps: vec![],
        })?))
    }
}
```

---

## 8. Approval Integration

### 8.1 Risk-Based Approval

```rust
/// Approval configuration for the task runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalConfig {
    /// Risk levels that require approval.
    pub require_approval_for: Vec<RiskLevel>,
    /// Auto-approve after timeout (None = never auto-approve).
    pub auto_approve_timeout: Option<Duration>,
    /// Callback for approval requests.
    #[serde(skip)]
    pub approval_handler: Option<Arc<dyn ApprovalHandler>>,
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Request approval for a step. Returns the decision.
    async fn request_approval(
        &self,
        step: &PlannedStep,
        context: &ApprovalContext,
    ) -> ApprovalDecision;
}

#[derive(Debug, Clone)]
pub struct ApprovalContext {
    pub task_description: String,
    pub step_description: String,
    pub risk_level: RiskLevel,
    pub files_to_modify: Vec<PathBuf>,
    pub commands_to_run: Vec<String>,
    pub estimated_impact: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    RequestChanges { feedback: String },
}
```

### 8.2 CLI Approval Flow

In the CLI, approval requests are surfaced to the user:

```
⚠️  Approval Required — Step 3: "Modify authentication middleware"
   Risk: HIGH
   Files: src/auth/middleware.rs, src/auth/session.rs
   Commands: cargo test

   [a] Approve  [r] Reject  [c] Request changes  [i] More info
```

```rust
pub struct CliApprovalHandler {
    stdin: Stdin,
    stdout: Stdout,
}

#[async_trait]
impl ApprovalHandler for CliApprovalHandler {
    async fn request_approval(
        &self,
        step: &PlannedStep,
        context: &ApprovalContext,
    ) -> ApprovalDecision {
        println!("⚠️  Approval Required — Step: \"{}\"", step.description);
        println!("   Risk: {:?}", step.risk);
        println!("   Files: {}", context.files_to_modify.join(", "));
        println!("   Commands: {}", context.commands_to_run.join("; "));
        println!();
        println!("   [a] Approve  [r] Reject  [c] Request changes");

        loop {
            let mut input = String::new();
            self.stdin.read_line(&mut input).await.ok();
            match input.trim() {
                "a" => return ApprovalDecision::Approved,
                "r" => return ApprovalDecision::Rejected,
                "c" => {
                    println!("   Enter feedback:");
                    let mut feedback = String::new();
                    self.stdin.read_line(&mut feedback).await.ok();
                    return ApprovalDecision::RequestChanges {
                        feedback: feedback.trim().to_string(),
                    };
                }
                _ => println!("   Invalid option. Try again."),
            }
        }
    }
}
```

---

## 9. Retry Semantics

### 9.1 Retry Policy

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retries per step.
    pub max_retries: u32,
    /// Base delay between retries.
    pub base_delay: Duration,
    /// Maximum delay cap.
    pub max_delay: Duration,
    /// Backoff multiplier.
    pub backoff_multiplier: f64,
    /// Whether to resume from checkpoint or restart the step.
    pub resume_from_checkpoint: bool,
}

impl RetryPolicy {
    /// Calculate the delay before the next retry.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_secs = self.base_delay.as_secs_f64()
            * self.backoff_multiplier.powi(attempt as i32);
        let capped = delay_secs.min(self.max_delay.as_secs_f64());
        Duration::from_secs_f64(capped)
    }
}
```

### 9.2 Retry Flow

```
  Step Fails (attempt 1)
       │
       ▼
  Save Checkpoint ──▶ Wait (exponential backoff)
       │
       ▼
  Resume from Checkpoint (attempt 2)
       │
       ├──▶ Success ──▶ Completed ✓
       │
       ├──▶ Fail again ──▶ Save Checkpoint ──▶ Wait ...
       │
       └──▶ Max retries exceeded ──▶ Failed ✗
                                         │
                                         ▼
                                   ReplanTrigger?
                                         │
                                   ┌─────┴──────┐
                                   │  Yes        │  No
                                   ▼             ▼
                              Replan          Abort Task
```

---

## 10. TaskStore Trait

### 10.1 Interface

```rust
/// Persistence interface for task state, plans, and checkpoints.
#[async_trait]
pub trait TaskStore: Send + Sync {
    // Task operations
    async fn save_task(&self, task: &Task) -> Result<(), StoreError>;
    async fn load_task(&self, task_id: TaskId) -> Result<Task, StoreError>;
    async fn update_task_state(&self, task_id: TaskId, state: TaskState) -> Result<(), StoreError>;
    async fn list_tasks(&self, filter: TaskFilter) -> Result<Vec<TaskSummary>, StoreError>;

    // Plan operations
    async fn save_plan(&self, plan: &Plan) -> Result<(), StoreError>;
    async fn load_plan(&self, task_id: TaskId) -> Result<Plan, StoreError>;
    async fn load_plan_revision(&self, task_id: TaskId, revision: u32) -> Result<Plan, StoreError>;

    // Checkpoint operations
    async fn save_checkpoint(
        &self,
        task_id: TaskId,
        step_id: StepId,
        checkpoint: &Checkpoint,
    ) -> Result<(), StoreError>;
    async fn load_latest_checkpoint(
        &self,
        task_id: TaskId,
        step_id: StepId,
    ) -> Result<Option<Checkpoint>, StoreError>;

    // Event log
    async fn append_event(&self, event: TaskEvent) -> Result<(), StoreError>;
    async fn load_events(&self, task_id: TaskId) -> Result<Vec<TaskEvent>, StoreError>;
}
```

### 10.2 Implementations

| Implementation     | Storage Backend     | Use Case                    |
|--------------------|---------------------|-----------------------------|
| `FileTaskStore`    | JSON files on disk  | Default CLI usage           |
| `SqliteTaskStore`  | SQLite database     | Persistent local sessions   |
| `MemoryTaskStore`  | In-memory HashMap   | Testing and ephemeral runs  |
| `RemoteTaskStore`  | HTTP/gRPC API       | Distributed xauft (future)  |

### 10.3 File-Based Store Layout

```
~/.xaft/store/
├── tasks/
│   ├── task_01HXYZ.json          # Task definition
│   └── task_01HXYZ/
│       ├── plan_rev0.json        # Initial plan
│       ├── plan_rev1.json        # Replan revision 1
│       ├── checkpoints/
│       │   ├── step_0_cp0.json   # Step 0, checkpoint 0
│       │   ├── step_0_cp1.json   # Step 0, checkpoint 1
│       │   └── step_1_cp0.json
│       └── events.log            # Append-only event log
```

---

## 11. Event Sourcing

All state transitions are recorded as events for auditability and debugging:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TaskEvent {
    #[serde(rename = "task.created")]
    Created { task_id: TaskId, description: String, timestamp: DateTime<Utc> },

    #[serde(rename = "task.planned")]
    Planned { task_id: TaskId, plan_id: PlanId, step_count: usize, timestamp: DateTime<Utc> },

    #[serde(rename = "task.replanned")]
    Replanned { task_id: TaskId, plan_id: PlanId, revision: u32, reason: String, timestamp: DateTime<Utc> },

    #[serde(rename = "step.started")]
    StepStarted { task_id: TaskId, step_id: StepId, agent_role: AgentRole, timestamp: DateTime<Utc> },

    #[serde(rename = "step.checkpoint_saved")]
    CheckpointSaved { task_id: TaskId, step_id: StepId, checkpoint_seq: u32, timestamp: DateTime<Utc> },

    #[serde(rename = "step.suspended")]
    StepSuspended { task_id: TaskId, step_id: StepId, reason: String, timestamp: DateTime<Utc> },

    #[serde(rename = "step.awaiting_approval")]
    AwaitingApproval { task_id: TaskId, step_id: StepId, risk: RiskLevel, timestamp: DateTime<Utc> },

    #[serde(rename = "step.approved")]
    Approved { task_id: TaskId, step_id: StepId, timestamp: DateTime<Utc> },

    #[serde(rename = "step.completed")]
    StepCompleted { task_id: TaskId, step_id: StepId, duration: Duration, timestamp: DateTime<Utc> },

    #[serde(rename = "step.failed")]
    StepFailed { task_id: TaskId, step_id: StepId, error: String, retry_count: u32, timestamp: DateTime<Utc> },

    #[serde(rename = "step.retried")]
    StepRetried { task_id: TaskId, step_id: StepId, attempt: u32, timestamp: DateTime<Utc> },

    #[serde(rename = "task.completed")]
    Completed { task_id: TaskId, total_duration: Duration, total_tokens: TokenUsage, timestamp: DateTime<Utc> },

    #[serde(rename = "task.failed")]
    Failed { task_id: TaskId, error: String, timestamp: DateTime<Utc> },
}
```

---

## 12. Configuration Reference

```toml
[xauft.task_runner]
max_concurrent_steps = 3
step_timeout_secs = 300
checkpoint_interval = 5          # every 5 tool calls
approval_for_high_risk = true
approval_for_critical = true
max_replans = 3

[xauft.task_runner.retry]
max_retries = 3
base_delay_secs = 2
max_delay_secs = 60
backoff_multiplier = 2.0
resume_from_checkpoint = true

[xauft.task_runner.planner]
type = "one_shot"                # "one_shot" | "iterative" | "tree_of_thought"
model = "gpt-4o"

[xauft.task_runner.planner.iterative]
iterations = 3

[xauft.task_runner.planner.tree_of_thought]
branching_factor = 3
max_depth = 2
evaluation_model = "gpt-4o"
```
