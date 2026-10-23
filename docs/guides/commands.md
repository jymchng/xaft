# Commands

Slash commands drive the TUI interactively.

## Built-in commands

| Command | Purpose |
|---|---|
| `/help` | Command list |
| `/clear` | Clear the transcript |
| `/compact` | Compact the conversation |
| `/cost` (aliases: `tokens`, `usage`) | Per-agent token/cost table |
| `/config` | Show the resolved configuration |
| `/mode` | List or switch mode (Safe → Plan → Yolo + aliases) |
| `/model` | Switch the active model |
| `/theme` | Cycle or set the TUI theme |
| `/permissions` | List tool permissions |
| `/resume` / `/rewind` | Session navigation |
| `/commit` / `/diff` / `/pr` | Git operations |
| `/init` | Create AGENTS.md / config |
| `/doctor` | Diagnose the environment |
| `/memory` | Memory tool access |
| `/mcp` | MCP server status |
| `/bg` | Background session control |
| `/loop [interval] <prompt or /command>` | Schedule a recurring prompt; `status` / `pause` / `resume` / `stop` control it |
| `/loops` | Every persisted loop job — `Enter` runs one now, `d` deletes |
| `/ps [terminal-id] [--json]` (alias: `processes`) | Background terminals — overlay with `s` to stop |
| `/stop [terminal-id\|all] [--force]` (alias: `stop-terminal`) | Stop background terminals |
| `/startup` | Startup phase readiness and timings |
| `/workers [--json]` | Durable parallel worker tasks for this session |
| `/login` | Sign in to the `[auth]` account provider; the outcome appears when the browser returns — see [Accounts](auth.md) |
| `/logout` | Sign out: revoke the stored tokens and delete them |
| `/vim` / `/emacs` | Input editing mode |
| `/quit` | Exit |

## Recurring prompts: `/loop`

```text
/loop 5m Check the deployment and fix a failing health check.
/loop 1h /status
/loop status | pause | resume | stop
/loops
```

The first iteration is due immediately but waits until the session is idle —
no turn running, no approval pending. Missed intervals are coalesced into one
iteration, and a loop never starts a concurrent turn. Each iteration is
submitted exactly like a typed message, so it runs under the active mode, the
approval gate and the session's conversation. Repeating `/loop` replaces the
session's loop. Loops expire after `max_age_s` and stop after
`max_consecutive_failures` failed iterations in a row (with exponential
backoff between retries). The live status shows above the prompt while the
session is idle.

A scheduled `/command` must be a registered slash command; `/loop`,
`/workflow`, `/compact` and `/quit` cannot be scheduled. Loops persist under
`core.data_dir/loops/<session>/loop.json` and are rehydrated only when that
session is explicitly resumed (`xaft --resume <id>` or `--continue`). Bounds
live in the `[loops]` config table — see [Configuration](configuration.md).

## Background terminals: `/ps` and `/stop`

`/ps` opens an overlay over the session's background terminals — the ones the
agent started with `run_command {"background": true}`. `↑↓`/`j`/`k`,
`PgUp`/`PgDn` and `Home`/`End` select, `Enter` shows details, `s` (or
`Ctrl+X`) stops the selected terminal, and `Esc` closes. `/ps --json` prints
every record instead. `/stop` stops every visible terminal, `/stop <id>` one,
and `/stop all` asks for `--confirm` (or `--force`, which also skips the
graceful grace period) first. Terminals still running when the TUI exits are
stopped with the session.

## Trigger pickers

- `/` — command picker
- `$` — skill-only picker
- `@` — file mention picker
- `#` — history recall

## Local vs agent commands

While the agent is responding, local read-only commands (e.g. `/usage`) and
run controls execute immediately; ordinary requests queue in FIFO order with
their position shown.

## Related

- [TUI](tui.md)
- [Reference: CLI](../reference/cli.md)
