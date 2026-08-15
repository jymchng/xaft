# Storage Reference

xaft persists sessions, conversations, and memory across runs.

## Session store

`xaft-session` (SQLite behind the `session` feature) stores:

- `Session` rows: id, status, timestamps, resume metadata
- `Conversation` rows keyed by `<session>::workflow::<agent>` (per-agent
  history), plus the session-level workflow key

## Memory tiers

`xaft-memory` stores tiered key/value facts:

- Session tier: in-memory per session
- Project tier: `.xaft/memory/` in the working directory
- Global tier: `~/.config/xaft/memory/`

`recall` performs fuzzy matching; `summarize` writes durable session
summaries; `forget` deletes keys.

## Config storage

`xaft-config` reads `.xaft/xaft.toml`, `~/.config/xaft/xaft.toml`, and env
overrides (see [Configuration](../guides/configuration.md)).

## Related

- [Session service](../guides/session-service.md)
- [Memory](../guides/memory.md)
