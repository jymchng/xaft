# Configuration

xaft reads a single TOML config file. Every key can be overridden by an
environment variable or a CLI flag. This guide documents the configuration
surface verified against `crates/xaft-config/src/types.rs`.

## Config file locations (precedence, highest first)

1. A config file passed with `--config <path>` (or `-c`)
2. `.xaft/xaft.toml` in the project directory
3. `~/.config/xaft/xaft.toml` (user config)
4. Built-in defaults

## Managing config from the CLI

```bash
xaft config show              # print the resolved config (pretty)
xaft config show --format json
xaft config init              # create a config template in the project
xaft config init --global     # create a config template in ~/.config/xaft/
xaft config validate          # validate the current config
xaft config validate -c path/to/xaft.toml
```

(Verified: `ConfigSubcommand` = Show / Init / Validate; `--format` accepts
`pretty` or `json`.)

## Top-level config sections

The `XaftConfig` struct (verified) has these sections:

| Section | Purpose |
|---|---|
| `[core]` | Log level, data dir, telemetry, AGENTS.md loading |
| `[agent.<name>]` | Named agent presets (model, provider, system prompt, tools) |
| `[provider.<name>]` | Provider connections (api key, base URL, retries, limits) |
| `[tool.<name>]` | Per-tool config (enabled, extra) |
| `[guardrail]` | Safety guardrails (file destruction, secrets, cost, approval) |
| `[mcp]` | MCP server definitions |
| `[tui]` | Theme, mouse, timestamps, layout, keybindings |
| `[plugins]` | Plugin configuration |
| `[model_tiers]` | flagship / standard / fast model routing |
| `[memory]` | Memory backend and auto behavior |
| `[mention]` | @-mention resolution |
| `[compaction]` | Conversation compaction |
| `[workflow]` | Workflow mode, meta-agent limits, handoff limits |

## Core config

```toml
[core]
log_level = "info"          # trace | debug | info | warn | error
data_dir = "~/.local/share/xaft"
telemetry = true
agents_md_enabled = true
agents_md_max_bytes = 65536
```

## Providers

xaft supports Anthropic, OpenAI, Ollama, and LiteLLM. Configure a provider
under `[provider.<name>]`:

```toml
[provider.anthropic]
provider_type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"   # read the key from this env var
base_url = "https://api.anthropic.com"
max_retries = 3
timeout_secs = 120

[provider.ollama]
provider_type = "ollama"
base_url = "http://localhost:11434"
```

Key fields (verified in `ProviderConfig`):

| Field | Meaning |
|---|---|
| `provider_type` | `anthropic` / `openai` / `ollama` / `litellm` |
| `api_key` | Inline key (discouraged — prefer `api_key_env`) |
| `api_key_env` | Env var name that holds the key |
| `base_url` | API base URL |
| `organization` | Org id (OpenAI) |
| `max_retries` | Retry count |
| `timeout_secs` | Request timeout |
| `headers` | Extra HTTP headers |
| `rpm_limit` / `tpm_limit` | Rate limits (requests/tokens per minute) |
| `models` | Model name → API model-id map |

## Environment variables

| Env var | Effect |
|---|---|
| `XAFT_MODEL_OVERRIDE` | Overrides the model (same as `--model`) |
| `ANTHROPIC_API_KEY` | Default Anthropic key |
| `OPENAI_API_KEY` | Default OpenAI key |

## Named agent presets

Presets bundle model, provider, system prompt, and tool permissions:

```toml
[agent.default]
model = "claude-sonnet-4"
provider = "anthropic"
system_prompt = "You are a careful Rust engineer."
max_turns = 30
temperature = 0.2
top_p = 0.9
allowed_tools = ["read_file", "write_file", "edit_file", "bash_exec", "git_*"]
denied_tools = []
parallel_tool_policy = "auto"
max_concurrent_tools = 4
allow_dynamic_tools = true
max_dynamic_tools = 8
dynamic_tool_approval = true
```

Use a preset with `xaft run --agent <name>` or `-a`.

## Guardrails

```toml
[guardrail]
file_destruction = true     # block destructive file operations
secret_leakage = true       # detect + redact secrets in tool output
cost_limit = true           # enforce token/cost limits
command_approval = true     # require approval before shell commands

[guardrail.cost_limit_config]
max_spend = 5.0             # USD per session
max_tokens_per_request = 4000
warn_at_percent = 80

[guardrail.secret_leakage_config]
patterns = ["AKIA[0-9A-Z]{16}", "sk-[a-zA-Z0-9]{20,}"]
# action: "block" | "redact" | "warn"
```

## TUI config

```toml
[tui]
theme = "dark"              # theme name
mouse = false
timestamps = false
conversation_height = 30
preserve_output_on_exit = true
use_alternate_screen = true
```

## Memory config

```toml
[memory]
enabled = true
backend = "json"            # storage backend
auto_remember = true
auto_summarize = false
project_scope_default = true
max_entries = 500
max_search_results = 10
```

## Workflow config

```toml
[workflow]
mode = "standard"           # standard | meta | dynamic
meta_max_spawned = 3
meta_max_parallel = 2
meta_allow_nesting = false
dynamic_max_handoffs = 14
```

## CLI overrides

Every run can override config without editing files:

```bash
xaft run "task" --model claude-3-5-sonnet-20241022
xaft run "task" --provider openai
xaft run "task" --max-turns 50
xaft run "task" --temperature 0.0
xaft run "task" --config ./my-xaft.toml
xaft run "task" --project-dir /path/to/repo
xaft run "task" --agent default
```

(Verified in `RunArgs`: `--model/-m`, `--provider`, `--max-turns`,
`--temperature`, `--config/-c`, `--project-dir`, `--agent/-a`.)

## Next

[Run your first task →](03-first-task.md)
