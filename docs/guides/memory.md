# Memory

xaft provides durable, tiered memory so agents can recall prior context across
sessions.

## Tiers

| Tier | Scope | Backing |
|---|---|---|
| Session | Current session | `xaft-session` store |
| Project | Current working directory | `.xaft/memory/` |
| Global | User home | `~/.config/xaft/memory/` |

## Memory tools

`xaft-memory` exposes four tools:

- `remember` — store a fact under a key
- `recall` — look up a key (with fuzzy matching)
- `summarize` — produce a durable summary of the session
- `forget` — remove a stored key

## Conversation persistence

`xaft-session` persists conversations to SQLite (when the `session` feature is
enabled) with per-agent keys, so `--resume` can replay prior turns. The
conversation store also backs the resume-tail transcript replay.

## Related

- [Session service](session-service.md)
- [Reference: storage](../reference/storage.md)
