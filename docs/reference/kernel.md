# Kernel Reference

`xaft-runtime` is the kernel crate: it boots the runtime, wires the session
and conversation stores, spawns the agent loop, and exposes the event bus.

## Entry points

- `XaftRuntime::bootstrap(config)` — create a runtime from config.
- `runtime.with_stores(session_store, conversation_store)` — attach durable
  stores (SQLite when the `session` feature is on).
- `runtime.with_approval_gate(gate)` — attach the approval gate.
- `runtime.signals()` — the `SignalBus` the TUI bridges from.
- `RunRequest` — the task descriptor (task, config, working dir, mode patch,
  tool filter, resume id, workflow, prior messages).

## Event loop

The runtime consumes the run request, classifies it, and drives the planner →
coder → qa → fixer handoff. Signals are emitted for LLM start/complete, tool
start/complete/approval, commit created, session update, and run complete.

## Mode plumbing

`RunRequest.mode_system_patch` and `RunRequest.mode_tool_filter` carry the
active mode's system prompt and tool filter into the runtime; `ModeManager`
applies them in the TUI before each run.

## Providers

`provider.rs` abstracts model providers behind a streaming `Provider` trait
with token accounting, thinking blocks, and retry. See
[Configuration](../guides/configuration.md) for selection.

## Related

- [Architecture](../guides/architecture.md)
- [Reference: repository state](./repository-state.md)
