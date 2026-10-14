# Context

Two issues:

1. **Stale binary**: The error "handoff already registered — terminating agent turn" was from
   an `AtomicBool` check in `before_llm_call` that I added and then removed from the source.
   The string no longer exists in source but the binary was compiled before the removal.
   Rebuild fixes the immediate error.

2. **Root cause still present**: After `handoff_to_agent` fires, `AgentExecutor` continues
   calling the LLM on subsequent iterations (the ReAct loop keeps going). There is no
   framework-level mechanism to terminate a turn after a tool fires. The agent can loop.

---

# Fix — Add `early_stop` flag to agtrs AgentConfig

`AgentExecutor::run()` (agtrs-runtime/src/executor.rs) has a `for iteration in
0..max_turns` loop. After all tool results are processed and added to messages (line ~593),
the loop restarts and calls `before_llm_call` → LLM again. We need a clean break point HERE.

**Change 1 — `AgentConfig` (agtrs-runtime/src/agent.rs)**

Add field:
```rust
pub struct AgentConfig {
    ...
    /// When set, the executor checks this flag after each batch of tool results.
    /// If `true`, the current agent turn ends immediately without another LLM call,
    /// as if the LLM had returned `StopReason::EndTurn`.
    pub early_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
}
```

**Change 2 — `AgentExecutor::run()` (agtrs-runtime/src/executor.rs)**

After tool results are drained (~line 593), insert:
```rust
// Check if a tool requested early termination (e.g. handoff_to_agent fired).
// Return Ok with accumulated content — no further LLM call.
if let Some(ref flag) = agent.config().early_stop {
    if flag.load(std::sync::atomic::Ordering::Acquire) {
        // Build the same AgentResponse as EndTurn would
        return Ok(build_agent_response(...));  // reuse existing EndTurn path
    }
}
```

This returns `Ok(AgentResponse)` (not `Err`), so `HandoffOrchestrator::run()` sees a
successful agent turn, detects the handoff in the store, and routes to the next agent.

**Change 3 — `HandoffTool::call()` (xaft-runtime/src/agent_registry.rs)**

Already sets `Arc<AtomicBool>` via `triggered` field (infrastructure in place). No change
needed here — the flag is already set on successful handoff.

**Change 4 — `NamedAgent` wiring (xaft-runtime/src/orchestrator.rs)**

Pass the `AtomicBool` flag into `AgentConfig.early_stop` when building planner/coder/fixer:
```rust
let planner_config = AgentConfig {
    system_prompt: planner_prompt(&wd),
    max_turns: 8,
    early_stop: Some(Arc::clone(&planner_stop)),
    ..Default::default()
};
```

This replaces the `handoff_triggered` field in `NamedAgent` (which was checking in
`before_llm_call` — the broken approach) with the proper agtrs mechanism.

---

# Files changed

| File | Change |
|---|---|
| `agtrs/agtrs-runtime/src/agent.rs` | Add `early_stop: Option<Arc<AtomicBool>>` to `AgentConfig` + `Default` impl |
| `agtrs/agtrs-runtime/src/executor.rs` | After tool results loop (~line 593), check `early_stop` flag, return `Ok` on match |
| `xaft-runtime/src/orchestrator.rs` | Pass `early_stop` via `AgentConfig` instead of custom `handoff_triggered` field in `NamedAgent` |
| `xaft-runtime/src/agent_registry.rs` | No change (flag mechanism already in HandoffTool) |

---

# Verification

```bash
cd /root/rust_projects/xaft
CARGO_TARGET_DIR=/root/rust_projects/xaft-target-local cargo test -p xaft-runtime
# All 65+ runtime tests pass
# planner-loops test: queue handoff tool call → confirm only 2 LLM responses consumed
# (not 8+ as with the old looping behavior)
```
