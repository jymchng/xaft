# Configuration

xaft reads a single TOML config file with environment-variable overrides and
hot reload.

## Config file locations (precedence, highest first)

1. `.xaft/xaft.toml` — project-local config
2. `~/.config/xaft/xaft.toml` — user config
3. Built-in defaults (`xaft-config/src/defaults.rs`)

You can point xaft at an explicit file with `--config <path>`.

## Environment overrides

Every config key can be overridden with an env var. The mapping is
`POKE_VAULT_`-style: `xaft config set execution.provider openai` sets
`execution.provider`; the env var is `XAFT_EXECUTION_PROVIDER` (dotted paths
become `_`-joined upper-case). See `xaft-config/src/merge.rs` for the exact
merge order.

## Hot reload

`xaft-config/src/watcher.rs` watches the active config file and reloads on
change. The TUI picks up reloads and applies them to the next run request.

## Key sections

| Section | Keys | Purpose |
|---|---|---|
| `[execution]` | `provider`, `model`, `temperature`, `max_tokens` | LLM provider + sampling |
| `[security]` | `approval.*`, `sandbox.*`, `permissions.*` | Approval gates and sandboxing |
| `[workspace]` | `root`, `git_worktree`, `auto_commit` | Workspace + git isolation |
| `[session]` | `data_dir`, `resume_transcript_turns` | Session persistence + resume tail length |
| `[tui]` | `theme`, `mouse`, `keyboard_enhanced` | TUI appearance + input |
| `[memory]` | `recall_limit`, `auto_summarize` | Memory tool limits |

See [Reference: repository state](../reference/repository-state.md) for the full
set of defaults and validation rules.
