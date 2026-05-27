# Planning Cascade

## Purpose

The planning cascade is xaft's two-phase strategy for decomposing complex goals into executable plans. Before any agent writes code, the planning cascade determines *what* needs to be done and *in what order*. It starts with a fast, single-pass `OneShotPlanner` and, if the goal is sufficiently complex, escalates to an `IterativeRefinementPlanner` that refines the plan through multiple rounds. This document explains how the `PlanModeAgent` drives the cascade, how escalation decisions are made, how plans are emitted as signals, and how the plan feeds into the downstream `AgentExecutor`.

The planning cascade exists because a single LLM call is often insufficient for complex tasks. A "build me a REST API" goal requires understanding the requirements, choosing a framework, designing routes, defining data models, implementing handlers, writing tests, and configuring the build. The cascade breaks this into a structured plan that the coding agent can follow step by step, reducing hallucination and improving task completion rates.

## Mental Model

Think of the planning cascade as a **funnel**. Goals enter at the top, and a refined plan exits at the bottom. The funnel has two stages:

```
Goal: "Build a REST API for a todo app"
       │
       ▼
┌──────────────────────┐
│  OneShotPlanner      │  Fast, single LLM call
│  plan() → Plan       │  Produces initial plan
└──────────┬───────────┘
           │
           ▼
   should_escalate()?
           │
     ┌─────┴─────┐
     │ NO        │ YES
     ▼           ▼
  Emit Plan   ┌──────────────────────────────┐
              │ IterativeRefinementPlanner   │  Multi-round refinement
              │ plan() with max_iterations   │  Each round improves plan
              └──────────┬───────────────────┘
                         │
                         ▼
                    Emit Plan
                         │
                         ▼
              ┌────────────────────┐
              │ Inject as context  │  Plan becomes a system message
              └──────────┬─────────┘
                         │
                         ▼
              ┌────────────────────┐
              │ AgentExecutor::run │  Coding agent executes the plan
              └────────────────────┘
```

The `PlanModeAgent` is the orchestrator that drives this cascade. It runs when the user's goal is first received and before any code is written. The agent itself is not a coding agent—it's a planning agent with a specialized system prompt and access to read-only tools.

## Extension Patterns

### PlanModeAgent.run()

The `PlanModeAgent` is the entry point for planning. Its `run()` method follows this sequence:

1. **Extract the goal** from the user's initial message or the handoff summary.
2. **Run `OneShotPlanner::plan()`** with the goal and workspace context.
3. **Check `should_escalate()`** against the `EscalationPolicy`.
4. If escalation is needed, **run `IterativeRefinementPlanner::plan()`** with the initial plan as a starting point.
5. **Emit the plan** as a signal (`XaftPlanCreated` or `XaftPlanEmpty` if no plan was produced).
6. **Optionally inject the plan** as a context message for the coding agent.
7. **Delegate to `AgentExecutor::run()`** to begin execution.

```rust
impl PlanModeAgent {
    pub async fn run(&self, goal: &str, ctx: &AgentContext) -> Result<PlanOutcome> {
        // Phase 1: One-shot planning
        let initial_plan = self.one_shot_planner.plan(goal, ctx).await?;

        // Phase 2: Escalation check
        let plan = if self.should_escalate(&initial_plan) {
            self.iterative_planner
                .plan(initial_plan, goal, ctx)
                .await?
        } else {
            initial_plan
        };

        // Phase 3: Emit signal
        if plan.steps.is_empty() {
            ctx.signal_bus.try_emit_signal(XaftPlanEmpty {
                goal: goal.to_string(),
            }).await;
            return Ok(PlanOutcome::Empty);
        }

        ctx.signal_bus.try_emit_signal(XaftPlanCreated {
            goal: goal.to_string(),
            steps: plan.steps.iter().map(|s| s.description.clone()).collect(),
            estimated_complexity: plan.complexity_score,
        }).await;

        // Phase 4: Inject as context
        if self.inject_plan_as_context {
            let plan_text = format_plan_as_message(&plan);
            ctx.message_store
                .inject_system_message(&ctx.conversation_key, plan_text)
                .await?;
        }

        // Phase 5: Delegate to executor
        let outcome = self.executor.run(ctx).await?;
        Ok(PlanOutcome::Executed(outcome))
    }
}
```

### OneShotPlanner::plan()

The `OneShotPlanner` makes a single LLM call with a planning-specific system prompt and returns a structured plan:

```rust
impl OneShotPlanner {
    pub async fn plan(&self, goal: &str, ctx: &AgentContext) -> Result<Plan> {
        let system_prompt = format!(
            "You are a planning agent. Given the goal below, produce a \
             step-by-step plan. Each step should be a single, atomic action \
             that a coding agent can execute. Respond in JSON format:\n\
             {{\"steps\": [{{\"description\": \"...\", \"tool_hint\": \"...\"}}]}}\n\n\
             Workspace root: {}\n\
             Existing files: {}",
            ctx.workspace.root().display(),
            ctx.workspace.file_listing().await?.join(", "),
        );

        let response = self.llm_client.chat(
            system_prompt,
            goal,
        ).await?;

        let plan: Plan = parse_plan_from_response(&response)?;
        Ok(plan)
    }
}
```

The `tool_hint` field is optional but valuable—it suggests which tool the coding agent should use for each step (e.g., `write_file`, `shell`, `read_file`), giving the executor a head start.

### EscalationPolicy and should_escalate()

The `EscalationPolicy` determines when the initial plan needs refinement:

```rust
pub enum EscalationPolicy {
    /// Never escalate — one-shot plan is always sufficient
    Never,
    /// Always escalate — always run iterative refinement
    Always,
    /// Escalate if the plan has more than N steps or complexity above threshold
    Threshold {
        max_steps: usize,
        complexity_threshold: f64,
    },
}
```

The `should_escalate()` method evaluates the policy against the initial plan:

```rust
fn should_escalate(&self, plan: &Plan) -> bool {
    match &self.escalation_policy {
        EscalationPolicy::Never => false,
        EscalationPolicy::Always => true,
        EscalationPolicy::Threshold { max_steps, complexity_threshold } => {
            plan.steps.len() > *max_steps 
                || plan.complexity_score > *complexity_threshold
        }
    }
}
```

The threshold-based policy is the most common choice. A plan with 3 simple steps doesn't need refinement, but a plan with 15 steps spanning multiple files and tools benefits from iterative review.

### IterativeRefinementPlanner::plan()

The iterative planner improves the initial plan through multiple rounds of LLM critique and revision:

```rust
impl IterativeRefinementPlanner {
    pub async fn plan(
        &self, 
        initial_plan: Plan, 
        goal: &str, 
        ctx: &AgentContext,
    ) -> Result<Plan> {
        let mut current_plan = initial_plan;

        for i in 0..self.max_refinement_iterations {
            // Critique: identify weaknesses in the current plan
            let critique = self.critique(&current_plan, goal, ctx).await?;

            if critique.issues.is_empty() {
                // No issues found — plan is good enough
                break;
            }

            // Revise: address the critique
            current_plan = self.revise(&current_plan, &critique, goal, ctx).await?;
        }

        Ok(current_plan)
    }
}
```

Each iteration has two sub-steps: **critique** (find problems like missing steps, vague descriptions, incorrect ordering) and **revise** (fix those problems). The planner stops early if a critique round finds no issues, avoiding unnecessary LLM calls.

### Plan Signal Emission

Plans are communicated to the rest of the system via signals:

- **`XaftPlanCreated`**: Emitted when a non-empty plan is produced. Contains the goal, step descriptions, and estimated complexity.
- **`XaftPlanEmpty`**: Emitted when the planner cannot produce a plan. Contains just the goal.

These signals are consumed by the TUI (to display the plan to the user), the event bridge (to forward to external consumers), and the agent runtime (to track planning state).

## Common Pitfalls

1. **Over-planning simple tasks.** If the goal is "fix the typo in README.md," running a one-shot planner (let alone an iterative one) is wasteful. Consider bypassing planning entirely for trivial goals.

2. **Setting `max_refinement_iterations` too high.** Each iteration costs an LLM call. Three iterations is usually sufficient; more than five is almost always wasteful.

3. **Ignoring the `tool_hint` field.** The coding agent works better when it has a hint about which tool to use. Without hints, it spends turns discovering the right tool through trial and error.

4. **EscalationPolicy::Always for fast-iteration workflows.** If you're using xaft for quick fixes, always escalating to iterative refinement adds unnecessary latency. Use `Threshold` or `Never` instead.

5. **Not injecting the plan as context.** If the plan is emitted as a signal but not injected into the coding agent's message history, the agent has no access to it. The plan exists in a vacuum and the agent improvises.

6. **Plans with vague step descriptions.** "Improve the code" is not an actionable step. Each step should specify what to do, where, and with which tool. The iterative refinement phase is designed to catch this, but only if the critique step checks for specificity.

7. **Complexity score calibration.** The `complexity_threshold` in `EscalationPolicy::Threshold` needs tuning per domain. A threshold of 0.5 might be too low for a web project (too many escalations) and too high for a systems project (not enough escalations).

## Invariants

- **The planning cascade always emits exactly one signal:** either `XaftPlanCreated` or `XaftPlanEmpty`. Never both, never neither.
- **`OneShotPlanner::plan()` always produces a `Plan`** (which may have zero steps). It never returns an error due to inability to plan—only due to infrastructure failures (LLM timeout, network error).
- **`max_refinement_iterations` is an upper bound, not a target.** The iterative planner stops as soon as a critique finds no issues.
- **The plan's step order is the intended execution order.** The coding agent should execute steps in sequence unless it has a good reason to deviate.
- **Plan injection happens before `AgentExecutor::run()` is called.** The coding agent always has the plan available from its first turn.
- **`should_escalate()` is a pure function of the plan and policy.** It has no side effects and does not make LLM calls.

## Examples

### Configuring Planning for a Web Project

```rust
let plan_agent = PlanModeAgent::new(
    OneShotPlanner::new(llm_client.clone()),
    IterativeRefinementPlanner::new(llm_client.clone())
        .with_max_refinement_iterations(3),
    EscalationPolicy::Threshold {
        max_steps: 5,
        complexity_threshold: 0.6,
    },
    true,  // inject_plan_as_context
);

let outcome = plan_agent.run(
    "Add user authentication with JWT tokens to the existing Express.js API",
    &agent_ctx,
).await?;
```

### Plan Output Example

For the goal "Add user authentication with JWT tokens," the one-shot planner might produce:

```json
{
  "steps": [
    {"description": "Install jsonwebtoken and bcrypt packages", "tool_hint": "shell"},
    {"description": "Create src/auth/jwt.ts with sign and verify functions", "tool_hint": "write_file"},
    {"description": "Create src/auth/middleware.ts with authenticate middleware", "tool_hint": "write_file"},
    {"description": "Add POST /auth/login and POST /auth/register routes", "tool_hint": "write_file"},
    {"description": "Add password hashing to user creation", "tool_hint": "write_file"},
    {"description": "Write tests for auth endpoints", "tool_hint": "write_file"},
    {"description": "Run tests and verify all pass", "tool_hint": "shell"}
  ]
}
```

This 7-step plan exceeds the `max_steps: 5` threshold, so `should_escalate()` returns `true` and the iterative planner refines it—perhaps splitting step 4 into separate login and register steps, or adding a migration step for the password column.

### Bypassing Planning for Simple Tasks

```rust
// For goals that are clearly trivial, skip planning entirely
if is_trivial_goal(&goal) {
    let outcome = executor.run(&agent_ctx).await?;
    return Ok(PlanOutcome::Executed(outcome));
}
```

The `is_trivial_goal` heuristic can check for short goals (< 20 words), goals that mention specific files, or goals that match known simple patterns.
