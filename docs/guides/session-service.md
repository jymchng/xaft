# Session service

Sessions are the unit of durable, resumable work.

## Session store

`xaft-session` persists sessions and conversations to SQLite (behind the
`session` feature). Each session records:

- Status lifecycle (Active → Completed / Failed / Cancelled)
- Per-agent conversation keys (`<session>::workflow::<agent>`)
- Refresh/approval tokens for resume

## Resume

`xaft --resume <session-id>` reopens a session: prior conversation lines are
replayed from the durable store (newest 20 turns by default — see
[TUI resume](tui.md#resumed-transcript)), and the next task continues with the
full prior context.

## Client-neutral access

The session service exposes a typed command/event interface that the TUI,
headless mode, CLI, and future web/IDE clients all share. The HTTP/SSE
adapter (`session_service/` in agenthicc; mirrored by `xaft-session`)
provides bounded subscriptions and replay.

## Related

- [Memory](memory.md)
- [Reference: storage](../reference/storage.md)
