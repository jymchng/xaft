# Memory

xaft can persist facts, insights, and prior fixes so agents recall them across
sessions. The memory system lives in `crates/xaft-memory`.

## Memory tools

The agent gets four memory tools:

| Tool | Purpose | Input |
|---|---|---|
| `remember` | Store a fact or insight for future recall | `content` (text), `tags` (optional list) |
| `recall` | Search project memory for relevant entries | `query` (search text) |
| `summarize` | Compress old memories into durable summaries | — |
| `forget` | Delete stale or incorrect memories | key / content to delete |

Example — the agent storing an architectural insight:

```json
{
  "content": "The auth service uses JWT tokens with 1-hour expiry",
  "tags": ["architecture", "auth"]
}
```

Then, on a later session, `recall` with a query like `"JWT expiry"` surfaces
that stored fact so the agent doesn't have to rediscover it.

## Configuration

`[memory]` in `xaft.toml` (verified in `MemoryConfig`):

```toml
[memory]
enabled = true
backend = "sqlite"            # "sqlite" | "in_memory"
auto_remember = true          # extract + store facts from agent turns
auto_summarize = false        # compress old memories when the store grows
project_scope_default = true  # default to project-scoped memory
max_entries = 500             # memories before auto-summarization triggers
max_search_results = 10       # recall result cap
```

### Backends

- `sqlite` — durable memory persisted to disk (default).
- `in_memory` — ephemeral memory that does not survive restarts. Useful for
  tests and short-lived runs.

### Auto behavior

- With `auto_remember = true`, xaft extracts candidate facts from agent turns
  and stores them automatically.
- With `auto_summarize = true`, old memories are compressed into summaries
  when the store approaches `max_entries`.

## Using memory from the TUI

- `/memory` exposes the memory tool set interactively.
- The `/cost` and `/config` commands remain unaffected; memory is a separate
  subsystem.

## Scope

Memory is **project-scoped** by default (`project_scope_default = true`), so
facts stored while working in one repo are not leaked into another.

## Next

- [Tools →](09-tools.md)
- [Configuration →](02-configuration.md)
