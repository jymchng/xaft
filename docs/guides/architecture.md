# Architecture

xaft is a Rust workspace of focused crates built on a task-based, event-driven
runtime. The design keeps the TUI responsive (never blocked on LLM calls, tool
execution, or file I/O) and the runtime observable (every signal is emitted on
a broadcast bus).

## Crate map

| Crate | Responsibility |
|---|---|
| `xaft-runtime` | Core runtime, event loop, provider factory, session store, orchestration, compactor |
| `xaft-tui` | Conversational streaming terminal renderer, triggers, approvals, modes, menus |
| `xaft-cli` | Command-line parsing, dispatch, config/session/run commands |
| `xaft-tools` | Filesystem, git, and shell tools + dynamic tool factory + registry |
| `xaft-agent` | Agent loop: streaming, signals, plan mode |
| `xaft-agents` | Named agents (Planner, Coder, QA, Fixer), handoff, registry |
| `xaft-config` | Typed config, TOML load/merge, validation, hot-reload watcher |
| `xaft-session` | Durable session + conversation persistence |
| `xaft-memory` | Recall/remember/summarize/forget memory tools |
| `xaft-skills` | Loadable agent knowledge files |

## The three concurrent tasks

The TUI feeds events into a single `mpsc::unbounded` channel from three
sources:

1. **Runtime loop task** — receives agent-runtime messages (LLM tokens, tool
   results, agent output) and forwards them as `TuiEvent` variants.
2. **Terminal event reader** — `crossterm::event::EventStream` for keys, mouse,
   and resize.
3. **Tick spawner** — emits a frame tick every 16 ms to drive cursors,
   spinners, and token counters.

The main render loop drains the channel, mutates `AppState`, and repaints —
single-threaded, lock-free.

## Event bridge

The `EventBridge` subscribes to the runtime `SignalBus` and converts each
signal (LLM start/complete, tool start/complete/approval, commit created,
session update) into a `TuiEvent`. The runtime never needs to know about the
TUI.

## Provider abstraction

`xaft-runtime/src/provider.rs` abstracts model providers (Anthropic, OpenAI,
Ollama, LiteLLM) behind a common `Provider` trait with streaming, thinking
blocks, token accounting, and retry. See [Configuration](configuration.md) for
provider selection.

## Observability

- `SignalBus` — type-safe broadcast event system (tracing + TUI events)
- Structured `tracing` spans with JSON output option
- Cost/token tracking per agent (`AgentStats`), surfaced via `/cost` in the TUI

## Related

- [TUI](tui.md) — rendering architecture, triggers, approvals
- [Workflows](workflows.md) — orchestration and handoff
- [Reference: kernel](../reference/kernel.md) — the kernel entry points
