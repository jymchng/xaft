# Commands and plugins

Slash commands drive the TUI; plugins extend the registries at runtime.

## Slash commands

`crates/xaft-tui/src/slash/` is the command system:

- `parser.rs` — `COMMAND_TABLE` (sorted, alias-aware) + parsing + completions.
- `registry.rs` — `SlashCommandRegistry` + `SlashHandler` trait.
- `commands/` — handlers for `/help`, `/clear`, `/compact`, `/cost`
  (aliases `tokens`, `usage`), `/config`, `/mode`, `/model`, `/theme`,
  `/permissions`, `/resume`, `/rewind`, `/commit`, `/diff`, `/pr`, `/init`,
  `/doctor`, `/memory`, `/mcp`, `/bg`, `/vim`, `/emacs`, `/quit`.

While the agent is responding, local read-only commands and run controls
execute immediately; ordinary requests queue in FIFO order.

## Triggers

- `/` — command picker
- `@` — file mention picker
- `$` — skill-only picker
- `#` — input history recall

## Extension points

- `xaft-tools/src/dynamic/` — scripted tools created at runtime.
- MCP servers — external tools bridged into the same registry.
- Custom planners — implement the planner trait and register it.

## Related

- [Commands guide](../docs/guides/commands.md)
- `crates/xaft-tui/src/slash/`
