cat > ./01_multi_agent_coordination.md << 'EOF'
# Multi-Agent Coordination

## Coordination Patterns in xaft

`xaft` uses three coordination patterns from `agtrs-runtime`, selected automatically based on task structure.

### Pattern 1: Sequential Handoff (default)

For tasks where each step depends on the previous result:

```
PlannerAgent → intent_to_plan
CodeAgent    → implement step 1
CodeAgent    → implement step 2
FixerAgent   → fix failures
ReviewAgent  → review diff
CodeAgent    → incorporate review
```

Implemented via `HandoffOrchestrator`:

```rust
let orchestrator = HandoffOrchestratorBuilder::new()
    .add_agent("planner",  Arc::new(PlannerAgent::new(llm.clone())))
    .add_agent("code",     Arc::new(CodeAgent::new(llm.clone(), workspace.clone())))
    .add_agent("fixer",    Arc::new(FixerAgent::new(llm.clone(), shell.clone())))
    .add_agent("reviewer", Arc::new(ReviewAgent::new(cheap_llm.clone())))
    .with_store(Arc::new(HandoffAgentStore::default()))
    .with_coordinator_prompt(xaft_handoff_prompt())
    .build();
```

### Pattern 2: Parallel Subagent Delegation

For tasks with independent subtasks (no file conflicts):

```
CodeAgent (orchestrator)
├── SubagentTool("migrate_auth")    → CodeAgent instance A (worktree A)
├── SubagentTool("add_logging")     → CodeAgent instance B (worktree B)
└── SubagentTool("update_tests")    → CodeAgent instance C (worktree C)
                                       ↓ all complete
                               merge_worktrees(A, B, C)
```

### Pattern 3: Team Coordinator

For complex tasks where a coordinator routes subtasks to specialists:

```rust
#[team(name = "engineering_team", mode = "coordinator", max_rounds = 10)]
#[injectable]
pub struct EngineeringTeam {
    code_agent:   Inject<CodeAgent>,
    fixer_agent:  Inject<FixerAgent>,
    index_agent:  Inject<IndexAgent>,
    review_agent: Inject<ReviewAgent>,
}
```

## AgentMessageBus Coordination

Agents communicate via typed messages without going through the orchestrator:

```rust
// CodeAgent sends review request to ReviewAgent mid-execution
pub async fn request_inline_review(
    &self,
    diff: &str,
    ctx: &ToolContext,
) -> Result<ReviewResult, AgtrsError> {
    let bus = ctx.message_bus().expect("bus required");
    let cid = Uuid::new_v4().to_string();
    let mut rx = bus.subscribe("code_agent").await;

    bus.send(AgentMessage::builder("code_agent", AgentMessageType::Query)
        .to("review_agent")
        .correlation_id(cid.clone())
        .payload(serde_json::json!({"diff": diff, "context": "auth_migration"}))
        .build()).await?;

    let response = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(msg) = rx.recv().await {
                if msg.correlation_id.as_deref() == Some(&cid) {
                    return msg;
                }
            }
        }
    }).await.map_err(|_| AgtrsError::msg("review timeout"))?;

    serde_json::from_value(response.payload.unwrap_or_default())
        .map_err(|e| AgtrsError::Other(e.to_string()))
}
```

## Conflict-Aware Parallelism

Before spawning parallel agents, `PlanExecutor` performs static analysis:

```rust
pub fn batch_non_conflicting(steps: &[PlanStep]) -> Vec<Vec<PlanStep>> {
    let mut batches: Vec<Vec<PlanStep>> = Vec::new();
    let mut current_batch: Vec<PlanStep> = Vec::new();
    let mut used_files: HashSet<String> = HashSet::new();

    for step in steps {
        let step_files: HashSet<String> = step.target_files.iter().cloned().collect();

        if step_files.is_disjoint(&used_files) && !step.depends_on.iter().any(|dep| {
            current_batch.iter().any(|s| s.id == *dep)
        }) {
            // Can run in parallel with current batch
            current_batch.push(step.clone());
            used_files.extend(step_files);
        } else {
            // Flush current batch, start new one
            if !current_batch.is_empty() {
                batches.push(current_batch.drain(..).collect());
                used_files.clear();
            }
            current_batch.push(step.clone());
            used_files.extend(step_files);
        }
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}
```

## Agent Communication Protocol

```rust
/// Standard xaft inter-agent message format
pub struct XaftAgentMessage {
    pub from: String,
    pub to: Option<String>,
    pub message_type: XaftMessageType,
    pub correlation_id: Option<String>,
    pub session_id: Uuid,
    pub task_id: Option<Uuid>,
    pub step_id: Option<String>,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

pub enum XaftMessageType {
    ReviewRequest,
    ReviewResponse,
    FixRequest,
    FixResponse,
    StatusUpdate,
    PlanRevisionRequest,
    ApprovalRequest,
    ApprovalResponse,
    Custom(String),
}
```

## References

- agtrs: `agtrs-runtime/src/team.rs`, `agtrs-runtime/src/messaging.rs`
- agtrs: `agtrs-runtime/src/subagent.rs`
- Next: [Planning System →](02_planning_system.md)
EOF

cat > ./02_planning_system.md << 'EOF'
# Planning System

## Intent to Plan Pipeline

```
User input: "migrate all usages of the old error API to anyhow"
    ↓
Intent construction:
    goal: "migrate all usages of the old error API to anyhow"
    constraints: ["all tests must pass", "no public API changes"]
    preferences: ["minimize diff size", "preserve formatting"]
    acceptance_criteria: ["cargo test passes", "cargo clippy passes"]
    ↓
PlannerAgent::plan(intent, available_tools)
    ↓
OneShotPlanner → LLM call → Plan{steps: [...]}
    OR
IterativeRefinementPlanner → LLM call → critique → revise → Plan
    ↓
TaskRunner::submit(intent, &ctx)
    ↓
PlanExecutor::execute(plan, agents, session)
```

## Intent Builder (xaft CLI)

```rust
// xaft/src/intent.rs
pub fn parse_intent(args: &RunArgs) -> Intent {
    let mut builder = Intent::from_goal(&args.goal);

    for constraint in &args.constraints {
        builder = builder.constraint(constraint);
    }

    // Auto-add standard Rust project constraints
    builder = builder
        .constraint("cargo test --workspace must pass after changes")
        .constraint("cargo clippy --workspace -- -D warnings must pass")
        .acceptance_criterion("all tests pass")
        .acceptance_criterion("clippy clean");

    if let Some(budget) = args.budget {
        builder = builder.max_cost(budget);
    }

    if let Some(deadline_secs) = args.timeout {
        builder = builder.deadline(Duration::from_secs(deadline_secs));
    }

    builder.build()
}
```

## Planner Selection Logic

```rust
pub fn select_planner(intent: &Intent, config: &XaftConfig) -> Arc<dyn Planner> {
    match config.planner.strategy.as_str() {
        "iterative" => Arc::new(IterativeRefinementPlanner::new(Arc::clone(&llm))
            .with_max_iterations(config.planner.iterations.unwrap_or(2))),
        "tree" => Arc::new(TreeOfThoughtPlanner::new(Arc::clone(&llm))
            .with_branches(config.planner.branches.unwrap_or(3))),
        _ /* "oneshot" */ => Arc::new(OneShotPlanner::new(Arc::clone(&cheap_llm))),
    }
}
```

Default: `oneshot` with a cheap model (Gemini Flash). `iterative` for complex refactors. `tree` for ambiguous goals.

## PlanStep Extensions (xaft-specific)

Beyond agtrs's `PlanStep`, xaft adds:

```rust
pub struct XaftPlanStep {
    /// Base agtrs step
    pub base: PlanStep,
    /// Agent type recommended for this step
    pub agent_hint: AgentType,
    /// Expected files to be modified
    pub target_files: Vec<String>,
    /// Whether this step can run in parallel with others
    pub parallelizable: bool,
    /// Checkpoint after this step regardless of policy
    pub force_checkpoint: bool,
    /// Human review required before proceeding
    pub requires_review: bool,
}

pub enum AgentType {
    Planner,
    Code,
    Review,
    Fixer,
    Index,
}
```

## Mid-Execution Replanning

When a step fails and recovery is not possible via FixerAgent:

```rust
pub async fn handle_step_failure(
    runner: &TaskRunner,
    planner: &PlannerAgent,
    task_id: Uuid,
    failed_step: &PlanStep,
    error: &str,
    session: &XaftSession,
) -> Result<(), XaftError> {
    let task = runner.get_task(task_id).await?;
    let completed = task.completed_steps();

    // Ask planner to generate revised plan
    let new_plan = planner.replan(
        &task.intent,
        completed,
        Some(failed_step.clone()),
        error,
        session.available_tool_names(),
    ).await?;

    // Apply revised plan (skips already-completed steps)
    runner.revise_plan(task_id, new_plan).await?;

    Ok(())
}
```

## Plan Visualization (TUI)

```
Plan: Migrate error API to anyhow (7 steps)

  ✓  1. Index affected files          code    0.3s   $0.002
  ✓  2. Add anyhow dependency         code    1.2s   $0.008
  ⟳  3. Migrate src/auth.rs          code    ...    ...
  ○  4. Migrate src/api.rs            code
  ○  5. Migrate tests/integration.rs  code
  ○  6. Run test suite               [shell]
  ○  7. Fix test failures (if any)    fixer
```

## References

- agtrs: `agtrs-runtime/src/planner.rs`
- agtrs: `agtrs-runtime/src/task.rs`
- agtrs example: `agtrs-examples/src/bin/02_task_planner.rs`
- Next: [Task Graph →](03_task_graph.md)
EOF

cat > ./03_task_graph.md << 'EOF'
# Task Graph Execution

## DAG Execution Model

`xaft` models task execution as a directed acyclic graph (DAG) where nodes are `PlanStep`s and edges represent `depends_on` relationships.

```
        [step-1: index]
              │
    ┌─────────┴───────────┐
    │                     │
[step-2: edit auth]  [step-3: edit api]    ← parallel (no shared files)
    │                     │
    └─────────┬───────────┘
              │
        [step-4: run tests]
              │
    ┌─────────┴───────────┐
    │                     │
[success: commit]   [failure: fixer]
```

## DAG Scheduler

```rust
pub struct DagScheduler {
    steps: HashMap<String, XaftPlanStep>,
    completed: HashSet<String>,
    in_flight: HashSet<String>,
}

impl DagScheduler {
    /// Returns steps whose dependencies are all completed.
    pub fn ready_steps(&self) -> Vec<&XaftPlanStep> {
        self.steps.values()
            .filter(|step| {
                !self.completed.contains(&step.base.id)
                && !self.in_flight.contains(&step.base.id)
                && step.base.depends_on.iter().all(|dep| self.completed.contains(dep))
            })
            .collect()
    }

    pub fn mark_in_flight(&mut self, step_id: &str) {
        self.in_flight.insert(step_id.to_string());
    }

    pub fn mark_complete(&mut self, step_id: &str) {
        self.in_flight.remove(step_id);
        self.completed.insert(step_id.to_string());
    }

    pub fn is_complete(&self) -> bool {
        self.completed.len() == self.steps.len()
    }
}
```

## Execution Loop

```rust
pub async fn execute_dag(
    scheduler: &mut DagScheduler,
    session: &XaftSession,
    ui_tx: mpsc::Sender<UiEvent>,
) -> Result<(), XaftError> {
    loop {
        if scheduler.is_complete() { break; }

        let ready = scheduler.ready_steps();
        if ready.is_empty() {
            // Wait for in-flight steps to complete
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        // Partition by parallelizability
        let (parallel, sequential): (Vec<_>, Vec<_>) = ready.iter()
            .partition(|s| s.parallelizable);

        if parallel.len() > 1 {
            // Execute non-conflicting steps in parallel
            let handles: Vec<_> = parallel.iter().map(|step| {
                scheduler.mark_in_flight(&step.base.id);
                let step = (*step).clone();
                let session = Arc::clone(session);
                let tx = ui_tx.clone();
                tokio::spawn(async move {
                    execute_step(&step, &session, tx).await
                })
            }).collect();

            for (handle, step) in handles.into_iter().zip(parallel.iter()) {
                match handle.await? {
                    Ok(_) => scheduler.mark_complete(&step.base.id),
                    Err(e) => return Err(e),
                }
            }
        } else {
            // Execute first ready step sequentially
            let step = ready[0];
            scheduler.mark_in_flight(&step.base.id);
            execute_step(step, session, ui_tx.clone()).await?;
            scheduler.mark_complete(&step.base.id);
        }
    }

    Ok(())
}
```

## Checkpoint Integration

After every step completion (or per `CheckpointPolicy`):

```rust
async fn save_step_checkpoint(
    session: &XaftSession,
    step: &XaftPlanStep,
    result: &StepResult,
) -> Result<(), XaftError> {
    let checkpoint = Checkpoint {
        checkpoint_id: Uuid::new_v4(),
        task_id: session.current_task_id(),
        session_id: session.session_id,
        step_index: step.base.sequence,
        completed_steps: session.completed_steps().await,
        worktree_path: session.active_worktree_path().await,
        worktree_branch: session.active_branch().await,
        conversation_snapshot: session.conversation_snapshot().await,
        context_state: session.context_state_snapshot().await,
        saved_at: Utc::now(),
    };

    session.task_runner.save_checkpoint(checkpoint).await?;
    session.signal_bus.emit(CheckpointSaved {
        task_id: session.current_task_id(),
        step: step.base.sequence,
    }).await;

    Ok(())
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs`
- agtrs: `agtrs-graph/src/validate.rs`
- Next: [Agent Handoffs →](04_agent_handoffs.md)
EOF

echo "Orchestration docs done"

cat > ./04_agent_handoffs.md << 'EOF'
# Agent Handoffs

## Handoff Protocol

When one agent transfers control to another, the `HandoffContext` carries sufficient state for the receiving agent to continue without repeating work.

```rust
pub struct XaftHandoffContext {
    /// Previous agent name
    pub from_agent: String,
    /// Receiving agent name
    pub to_agent: String,
    /// Human-readable summary of work done so far
    pub summary: String,
    /// Files modified so far (in worktree)
    pub modified_files: Vec<PathBuf>,
    /// Current git diff (condensed)
    pub current_diff_summary: String,
    /// Any structured artifacts from the previous agent
    pub artifacts: HashMap<String, serde_json::Value>,
    /// Reason for handoff
    pub reason: HandoffReason,
}

pub enum HandoffReason {
    StepComplete,           // normal transition
    TestFailure { error: String },  // CodeAgent → FixerAgent
    ReviewRequested,        // CodeAgent → ReviewAgent
    OutOfContext,           // agent hit token limit
    CostLimit,              // agent hit cost limit
}
```

## CodeAgent → FixerAgent Handoff

```rust
// In PlanExecutor, after CodeAgent step:
let test_result = shell.run("cargo test --workspace 2>&1", None).await?;

if test_result.exit_code != 0 {
    let handoff = XaftHandoffContext {
        from_agent: "code".into(),
        to_agent: "fixer".into(),
        summary: code_response.content.clone(),
        modified_files: workspace.list_modified().await?,
        current_diff_summary: git.diff_summary(worktree).await?,
        artifacts: HashMap::from([
            ("test_error".into(), serde_json::json!(test_result.stderr)),
            ("test_stdout".into(), serde_json::json!(test_result.stdout)),
        ]),
        reason: HandoffReason::TestFailure { error: test_result.stderr.clone() },
    };

    handoff_store.set_active_agent(&session_id, "fixer").await;
    handoff_store.set_pending_context(&session_id, &handoff).await;

    // Run FixerAgent with context
    let mut fixer_ctx = build_agent_context("fixer", &session);
    inject_handoff_context(&mut fixer_ctx, &handoff);
    AgentExecutor::run(&fixer_agent, Message::user(
        format!("Fix these test failures:\n```\n{}\n```", test_result.stderr)
    ), &mut fixer_ctx).await?;
}
```

## Context Window Preservation

When handing off between agents, the conversation history is condensed:

```rust
fn build_handoff_system_message(handoff: &XaftHandoffContext) -> Message {
    Message::system(format!(
        r#"You are continuing work started by the {} agent.

## Summary of Work Done
{}

## Modified Files
{}

## Current Diff (condensed)
```diff
{}
```

## Your Task
{}
"#,
        handoff.from_agent,
        handoff.summary,
        handoff.modified_files.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n"),
        handoff.current_diff_summary,
        match &handoff.reason {
            HandoffReason::TestFailure { error } => format!("Fix these errors:\n```\n{error}\n```"),
            HandoffReason::ReviewRequested => "Review the above changes for correctness.".into(),
            _ => "Continue the task.".into(),
        }
    ))
}
```

## References

- agtrs: `agtrs-runtime/src/team.rs` (HandoffOrchestrator, HandoffAgentStore)
- agtrs guide: `guides/14-team-and-handoff.md`
EOF

