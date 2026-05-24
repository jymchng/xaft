# Cost Routing & Budget Enforcement

## Cost Architecture

```
CostTracker (Arc, shared across session)
    ├── session_total_usd: AtomicF64
    ├── task_total_usd:    AtomicF64
    ├── per_agent_cost:    HashMap<String, AtomicF64>
    └── per_model_cost:    HashMap<String, AtomicF64>

PricingTable
    ├── anthropic_defaults()
    ├── gemini_defaults()
    └── custom(HashMap<model, TokenPrice>)

Budget enforcement:
    AgentConfig::max_cost_usd  → per-agent run limit
    XaftConfig::session_budget → per-session limit
    XaftConfig::task_budget    → per-task limit
```

## Real-Time Cost Tracking

```rust
// Plugged in via SignalBus sync handler
bus.on::<ModelCallComplete>(move |s| {
    cost_tracker.add_model_call(
        &s.model,
        &s.agent_name,
        s.cost_usd,
    );

    // Emit CostUpdate for TUI
    ui_tx.try_send(UiEvent::CostUpdate {
        session_total: cost_tracker.session_total(),
        task_total: cost_tracker.task_total(),
        last_call: s.cost_usd,
    }).ok();
});
```

## Budget Enforcement Levels

```
Level 1: Per-model-call (executor checks after each LLM call)
    if total_cost > config.session_budget:
        return Err(AgtrsError::CostBudgetExceeded)

Level 2: Per-agent-run (AgentConfig::max_cost_usd)
    checked by AgentExecutor after each turn

Level 3: Per-task (XaftConfig::task_budget)
    checked by PlanExecutor after each step

Level 4: Per-session (XaftConfig::session_budget)
    checked by SessionManager
```

## Cost-Aware Model Routing

Route to cheaper models when the task doesn't require maximum capability:

```rust
pub fn select_model_for_task(task_type: &str, cost_remaining: f64) -> &'static str {
    if cost_remaining < 0.10 {
        // Very low budget — use cheapest
        return "gemini-2.0-flash";
    }

    match task_type {
        "plan" | "summarize" | "classify" => "gemini-2.0-flash",
        "review" | "explain"              => "claude-3-haiku",
        "code" | "fix" | "complex"        => "claude-3-5-sonnet-20241022",
        _                                 => "claude-3-5-sonnet-20241022",
    }
}
```

## Cost Reports

```bash
$ xaft cost ses-abc123

Session: ses-abc123
Duration: 4m 32s
Intent: "migrate auth module to JWT"

Model Usage:
  claude-3-5-sonnet  |  12,450 in  |  4,230 out  |  $0.089
  gemini-2.0-flash   |   3,200 in  |    890 out  |  $0.003

Tool Calls:
  read_file      ×12   (0 cost)
  write_file     ×5    (0 cost)
  run_cargo      ×4    (0 cost)
  search_code    ×8    (0 cost)

Total: $0.092 / $2.00 budget (4.6%)
```

## References

- agtrs: `agtrs-runtime/src/cost.rs`, `agtrs-runtime/src/signals.rs`
- agtrs example: `agtrs-examples/src/bin/03_user_budget_caps.rs`
