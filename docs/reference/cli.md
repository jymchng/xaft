# CLI Reference

The `xaft` binary is the entry point (see `crates/xaft-cli`).

## Commands

| Command | Purpose |
|---|---|
| `xaft run "<task>"` | Run a task through the plan→code→verify→commit pipeline |
| `xaft --resume <id>` | Resume a session (with transcript replay) |
| `xaft config ...` | Get/set config keys (`config get`, `config set`, `config path`) |
| `xaft session ...` | List/inspect/delete sessions |
| `xaft version` | Print version and build info |
| `xaft completions <shell>` | Generate shell completions |

## Key flags

| Flag | Purpose |
|---|---|
| `--mode <name>` | Start in a specific mode (safe/plan/auto/…; aliases accepted) |
| `--workflow <name>` | Select a workflow |
| `--headless` | Stdin JSON-lines interface for pipelines/CI |
| `--dry-run` | Plan only, no mutations |
| `--auto-approve` | Auto-approve low/medium-risk tools |
| `--dangerously-skip-permissions` | Skip permission gates (requires terminal confirmation) |
| `--config <path>` | Use an explicit config file |

## Exit codes

- `0` — success
- `1` — task failed or cancelled
- `2` — usage/argument error

## Related

- [Quickstart](../guides/quickstart.md)
- [Configuration](../guides/configuration.md)
