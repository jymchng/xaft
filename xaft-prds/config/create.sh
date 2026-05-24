cat > ./config/01_configuration_system.md << 'EOF'
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
EOF

cat > ./config/02_deployment_packaging.md << 'EOF'
# Deployment & Packaging

## Release Targets

| Platform | Format | Distribution |
|---|---|---|
| Linux x86_64 | Static binary (musl) | GitHub Releases, cargo-binstall |
| Linux ARM64 | Static binary (musl) | GitHub Releases |
| macOS ARM64 (Apple Silicon) | Signed binary | GitHub Releases, Homebrew |
| macOS x86_64 | Signed binary | GitHub Releases, Homebrew |
| Windows x86_64 | .exe + installer | GitHub Releases, winget |

## Build Configuration

```toml
# Cargo.toml [profile.release]
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"      # smaller binary, no unwinding overhead

# Target maximum performance on all cores
[target.x86_64-unknown-linux-musl]
rustflags = ["-C", "target-cpu=x86-64-v3"]
```

## Static Binary (Linux)

```bash
# Build static binary using musl (zero system library deps)
cargo build --release --target x86_64-unknown-linux-musl
# Output: ~12MB stripped static binary

# Verify no dynamic deps
ldd target/x86_64-unknown-linux-musl/release/xaft
# → statically linked
```

## Homebrew Formula

```ruby
class Xaft < Formula
  desc "Autonomous coding CLI built on the agtrs framework"
  homepage "https://github.com/yourorg/xaft"
  version "0.1.0"

  on_macos do
    on_arm { url "https://github.com/yourorg/xaft/releases/download/v#{version}/xaft-aarch64-apple-darwin.tar.gz" }
    on_intel { url "https://github.com/yourorg/xaft/releases/download/v#{version}/xaft-x86_64-apple-darwin.tar.gz" }
  end

  def install
    bin.install "xaft"
    generate_completions_from_executable(bin/"xaft", "completions")
  end
end
```

## Shell Completions

```bash
# Generate shell completions
xaft completions bash   >> ~/.bash_completion
xaft completions zsh    >> ~/.zshrc
xaft completions fish   > ~/.config/fish/completions/xaft.fish
xaft completions nushell > ~/.config/nushell/completions/xaft.nu
```

## Docker Image

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y git ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/xaft /usr/local/bin/xaft
WORKDIR /workspace
VOLUME ["/workspace"]
ENTRYPOINT ["xaft"]
```

## CI/CD Release Pipeline

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags: ["v*"]
jobs:
  build:
    strategy:
      matrix:
        include:
          - os: ubuntu-latest,  target: x86_64-unknown-linux-musl
          - os: ubuntu-latest,  target: aarch64-unknown-linux-musl
          - os: macos-latest,   target: aarch64-apple-darwin
          - os: macos-latest,   target: x86_64-apple-darwin
          - os: windows-latest, target: x86_64-pc-windows-msvc
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: "${{ matrix.target }}" }
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: softprops/action-gh-release@v2
        with: { files: "target/${{ matrix.target }}/release/xaft*" }
```

## Installation

```bash
# cargo-binstall (fastest)
cargo binstall xaft

# From source
cargo install xaft

# Script installer
curl -fsSL https://get.xaft.dev | sh

# Homebrew
brew install xaft
```
EOF

echo "Config docs done"