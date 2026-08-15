# Slash commands

Slash commands drive the TUI interactively. The full command table is
verified against `crates/xaft-tui/src/slash/parser.rs` (`COMMAND_TABLE`).

## Command reference

| Command | Aliases | Purpose |
|---|---|---|
| `/help` | — | Show the command list |
| `/clear` | `/cls` | Clear the transcript |
| `/compact` | `/ctx` | Compact the conversation context |
| `/cost` | `/tokens`, `/usage` | Per-agent token and cost table |
| `/config` | — | Show the resolved configuration |
| `/mode` | — | List or switch mode (Safe → Plan → Yolo + aliases) |
| `/model` | — | Switch the active model |
| `/agents` | — | List or manage agents |
| `/permissions` | — | List tool permissions |
| `/resume` | — | Resume a session |
| `/rewind` | — | Rewind to a previous message index |
| `/commit` | — | Commit the current worktree changes |
| `/diff` | — | Show the current diff |
| `/pr` | — | Open or create a pull request |
| `/init` | — | Create AGENTS.md / config scaffold |
| `/doctor` | — | Diagnose the environment |
| `/memory` | — | Access memory tools |
| `/mcp` | — | MCP server status |
| `/login` / `/logout` | — | Provider auth |
| `/theme` | — | Cycle or set the colour theme |
| `/vim` / `/emacs` | — | Input editing mode |
| `/exit` | `/q`, `/quit` | Exit xaft |

## Trigger pickers

- `/` — command picker
- `$` — skill-only picker
- `@` — file mention picker
- `#` — history recall

## Local vs agent commands

While the agent is responding, **local read-only commands** (e.g. `/usage`,
`/config`) and **run controls** execute immediately; ordinary requests,
skills, mutations, and workflow actions queue in FIFO order with their queue
position shown. So `/usage` never blocks on the agent — it shows the current
token/cost snapshot instantly.

## Notes on specific commands

- `/cost` (aliases `/tokens`, `/usage`) renders a per-agent table with
  CALLS / IN / OUT / CACHE_R / COST columns and a TOTAL row.
- `/config` opens the configuration display; `/config <key.path>` shows a
  specific key.
- `/mode` with no args lists the cycle (Safe → Plan → Yolo), the full
  registry, and the aliases (`yolo≡auto, ask/guard≡safe, review≡plan`).
- `/resume` resumes the most recent session in the current directory;
  `/rewind <n>` rewinds to message index `n`.
- `/doctor` checks provider connectivity and MCP server health.
- `/memory` exposes the memory tool set (see [Memory](08-memory.md)).
- `/exit`, `/q`, `/quit` all exit the TUI (with an escape-confirmation dialog
  if there is a pending run).

## Next

- [Sessions →](07-sessions.md)
- [Tools →](09-tools.md)
