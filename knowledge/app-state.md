# App state and event system

xaft's TUI and runtime keep separate state models, mirroring agenthicc's
separation of the kernel `AppState` (frozen domain state reduced from events)
from the reactive TUI presentation model.

## Runtime state

- `XaftRuntime` owns the session, conversation stores, approval gate, and the
  `SignalBus`.
- `RunRequest` carries the task, config, working dir, mode patch, tool filter,
  resume id, workflow, and prior messages.
- Signals are emitted for LLM start/complete, tool start/complete/approval,
  commit created, session update, and run complete.

## TUI state

`AppState` (in `crates/xaft-tui/src/state.rs`) is the single mutable
presentation model. It holds:

- `mutations: Vec<RenderMutation>` — commands consumed by the incremental
  renderer (CommitLine, StreamToken, FlushStream, SetEphemeral, UpdatePrompt,
  Resize, Shutdown).
- `agent_tracker` — per-agent status (Thinking, ToolCalling, AwaitingApproval).
- `tool_group` — the collapsed tool-group tracker (agenthicc parity).
- `active_trigger` — the currently open trigger dropdown (`/`, `@`, `$`, `#`).
- `mode_manager` — active mode + the Safe→Plan→Yolo cycle.
- `background_entries` — detached background pipelines with bounded buffers.

## Event flow

The `EventBridge` subscribes to the runtime `SignalBus` and forwards signals
as `TuiEvent`s into the single `mpsc` channel. The main render loop drains the
channel, mutates `AppState`, and repaints. All state mutation happens on the
main thread — no locks needed.

## Related

- `crates/xaft-tui/src/state.rs`
- `crates/xaft-tui/src/bridge.rs`
- [TUI guide](../docs/guides/tui.md)
