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
| `/vim` / `/emacs` | Input editing mode |
| `/quit` | Exit |

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
