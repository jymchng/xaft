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
