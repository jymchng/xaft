# Configuration System

## Config File Hierarchy

```
Priority (highest wins):
5. CLI flags
4. Environment variables (XAFT_*)
3. .xaft/config.toml (project-level)
2. ~/.config/xaft/config.toml (user-level)
1. Built-in defaults
```

## Full Config Schema

```toml
# ~/.config/xaft/config.toml

[general]
# Default LLM provider
primary_provider = "anthropic"      # "anthropic" | "gemini" | "openai" | "ollama"
cheap_provider = "gemini"
embedding_provider = "voyage"       # for semantic search

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"   # env var name (not value)
model = "claude-3-5-sonnet-20241022"
max_tokens = 4096
timeout_secs = 120

[providers.gemini]
api_key_env = "GEMINI_API_KEY"
model = "gemini-2.0-flash"
max_tokens = 4096

[providers.voyage]
api_key_env = "VOYAGE_API_KEY"
model = "voyage-code-2"

[agents]
[agents.planner]
provider = "cheap"
max_turns = 3
temperature = 0.3

[agents.code]
provider = "primary"
max_turns = 20
temperature = 0.2
max_cost_usd = 2.00
parallel_tool_calls = true
memory_window_tokens = 40000
summarize_at = 0.80

[agents.fixer]
provider = "primary"
max_turns = 10
temperature = 0.2
max_cost_usd = 1.00

[agents.reviewer]
provider = "cheap"
max_turns = 5
temperature = 0.3

[planner]
strategy = "oneshot"        # "oneshot" | "iterative" | "tree"
iterations = 2              # for iterative
branches = 3                # for tree

[safety]
auto_approve = "none"       # "none" | "low" | "medium" | "high"
approval_timeout_secs = 60
approval_timeout_action = "deny"
strict_shell_mode = true

[safety.allowed_commands]
commands = ["cargo", "git", "grep", "rg", "fd", "cat", "head", "tail", "wc", "diff", "ls", "rustfmt"]

[budget]
session_budget_usd = 5.00
task_budget_usd = 2.00
warn_at_percent = 80

[logging]
format = "pretty"           # "pretty" | "json"
level = "info"
output = "stderr"

[ui]
theme = "dark"
frame_rate = 30             # TUI frames per second
max_output_lines = 2000     # per agent pane
auto_scroll = true
show_thinking = false       # show extended thinking blocks

[git]
auto_stage = true
auto_commit = false         # require explicit commit
commit_message_model = "cheap"
create_pr_after_task = false
pr_base_branch = "main"

[index]
auto_build = true           # build index on first run
incremental = true          # watch for file changes
languages = ["rust", "typescript", "python"]
exclude_patterns = ["target/**", "node_modules/**", ".git/**"]

[server]
host = "127.0.0.1"
port = 7080
require_auth = true
api_keys_env = "XAFT_API_KEYS"     # comma-separated keys
max_concurrent_sessions = 4

[storage]
backend = "sqlite"          # "sqlite" | "memory"
session_ttl_secs = 86400    # 24h
```

## XaftConfig Struct

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct XaftConfig {
    pub general: GeneralConfig,
    pub providers: ProvidersConfig,
    pub agents: AgentsConfig,
    pub planner: PlannerConfig,
    pub safety: SafetyConfig,
    pub budget: BudgetConfig,
    pub logging: LoggingConfig,
    pub ui: UiConfig,
    pub git: GitConfig,
    pub index: IndexConfig,
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub project_root: PathBuf,  // resolved at startup
}

impl XaftConfig {
    pub fn load(args: &Cli) -> Result<Self, ConfigError> {
        let mut config = Self::default();

        // Layer 1: User config
        if let Some(path) = dirs::config_dir().map(|d| d.join("xaft/config.toml")) {
            if path.exists() {
                config.merge(Self::from_file(&path)?);
            }
        }

        // Layer 2: Project config
        let project_config = find_project_root()?.join(".xaft/config.toml");
        if project_config.exists() {
            config.merge(Self::from_file(&project_config)?);
        }

        // Layer 3: Environment variables
        config.merge_env()?;

        // Layer 4: CLI flags
        config.merge_args(args)?;

        config.validate()?;
        Ok(config)
    }
}
```

## Environment Variables

| Variable | Equivalent config | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | providers.anthropic.api_key | Anthropic API key |
| `GEMINI_API_KEY` | providers.gemini.api_key | Google Gemini API key |
| `VOYAGE_API_KEY` | providers.voyage.api_key | Voyage AI API key |
| `XAFT_MODEL` | agents.code.model | Override primary model |
| `XAFT_BUDGET_USD` | budget.session_budget_usd | Session cost budget |
| `XAFT_AUTO_APPROVE` | safety.auto_approve | Auto-approve level |
| `XAFT_LOG` | logging.level | Log level |
| `XAFT_API_KEYS` | server.api_keys | Comma-separated server API keys |
| `NO_COLOR` | ui.theme | Disable color output |

## References

- Next: [Deployment & Packaging →](02_deployment_packaging.md)