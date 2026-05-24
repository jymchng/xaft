# Deployment

## 1. Overview

xaft's deployment strategy prioritizes zero-friction installation, cross-platform
compatibility, and secure credential management. As a single Rust binary, xaft
has fundamentally simpler deployment characteristics than Node.js or Python
alternatives — no runtime, no package manager, no virtual environment required.

```
┌──────────────────────────────────────────────────────────────────────┐
│                    xaft Deployment Options                            │
│                                                                       │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────┐ │
│  │ cargo install │  │   Homebrew   │  │   Docker    │  │ Static  │ │
│  │              │  │              │  │              │  │ Binary  │ │
│  │ Developers   │  │ macOS/Linux  │  │ CI/CD       │  │ Air-gapped│ │
│  │ Rust users   │  │ Homebrew     │  │ Isolated    │  │ Corporate│ │
│  │              │  │ users        │  │ environments│  │ networks │ │
│  └──────────────┘  └──────────────┘  └──────────────┘  └─────────┘ │
│                                                                       │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │
│  │ GitHub       │  │ npm wrapper  │  │  Pre-built   │              │
│  │ Releases     │  │              │  │  .deb/.rpm   │              │
│  │ All platforms│  │ npm install  │  │  Linux       │              │
│  │ Latest + prev│  │ -g xaft     │  │  distros     │              │
│  └──────────────┘  └──────────────┘  └──────────────┘              │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 2. Installation Methods

### 2.1 cargo install

The primary installation method for Rust developers. Provides the latest version
directly from crates.io.

```bash
# Install the latest release
cargo install xaft

# Install a specific version
cargo install xaft --version 0.2.0

# Install with all features (including WASM plugin support)
cargo install xaft --all-features

# Install with minimal features (no sandbox, no WASM)
cargo install xaft --no-default-features

# Install from git (bleeding edge)
cargo install --git https://github.com/xaft-dev/xaft --branch main
```

**Build Requirements:**

| Component | Minimum Version | Notes |
|-----------|----------------|-------|
| Rust | 1.75+ | Edition 2021, async fn in trait |
| C compiler | gcc 8+ / clang 10+ | For native dependencies |
| OpenSSL | 1.1.1+ | For TLS (can use rustls instead) |
| pkg-config | any | For finding system libraries |

**Feature Flags:**

```toml
# Cargo.toml feature configuration
[features]
default = ["rustls", "sandbox-docker", "tui"]

# TLS backends (mutually exclusive)
rustls = ["reqwest/rustls-tls"]         # Pure Rust TLS (default)
native-tls = ["reqwest/native-tls"]     # System TLS (OpenSSL/Schannel)

# Sandbox backends
sandbox-docker = ["bollard"]            # Docker container sandbox
sandbox-podman = ["podman-api"]         # Podman container sandbox
sandbox-native = []                     # No sandbox (direct execution)

# Plugin system
wasm-plugins = ["wasmtime"]             # WASM plugin runtime

# UI
tui = ["ratatui", "crossterm"]          # Terminal UI dashboard

# Observability
jemalloc = ["tikv-jemallocator"]        # jemalloc memory allocator

# LLM providers
anthropic = []                           # Anthropic Claude
openai = []                             # OpenAI GPT
google = []                             # Google Gemini
all-providers = ["anthropic", "openai", "google"]
```

### 2.2 Homebrew

For macOS and Linux users who prefer Homebrew:

```bash
# Add the xaft tap
brew tap xaft-dev/tap

# Install xaft
brew install xaft

# Install with Docker sandbox support
brew install xaft --with-docker

# Upgrade to latest version
brew upgrade xaft
```

**Homebrew Formula (C):**

```ruby
class Xaft < Formula
  desc "Autonomous coding CLI built on the agtrs Rust framework"
  homepage "https://github.com/xaft-dev/xaft"
  url "https://github.com/xaft-dev/xaft/archive/v0.2.0.tar.gz"
  sha256 "..." # Computed on release
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", "--path", ".", "--root", prefix
  end

  test do
    assert_match "xaft", shell_output("#{bin}/xaft --version")
  end
end
```

### 2.3 Docker

For CI/CD pipelines and isolated execution environments:

```bash
# Pull the official image
docker pull ghcr.io/xaft-dev/xaft:latest

# Run interactively
docker run -it --rm \
  -v $(pwd):/workspace \
  -v ~/.config/xaft:/home/xaft/.config/xaft \
  -e ANTHROPIC_API_KEY \
  ghcr.io/xaft-dev/xaft:latest \
  xaft run "Fix the bug in src/main.rs"

# Run in CI mode
docker run --rm \
  -v $(pwd):/workspace \
  -e ANTHROPIC_API_KEY \
  -e XAFT_BUDGET_LIMIT=2.0 \
  -e XAFT_MAX_TURNS=30 \
  ghcr.io/xaft-dev/xaft:latest \
  xaft ci --review-only
```

**Dockerfile:**

```dockerfile
# Multi-stage build for minimal image
FROM rust:1.82-bookworm AS builder

WORKDIR /usr/src/xaft
COPY . .

# Build with musl for static linking
RUN rustup target add x86_64-unknown-linux-musl && \
    cargo build --release --target x86_64-unknown-linux-musl \
    --no-default-features --features "rustls,tui,all-providers"

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates git && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -s /bin/bash xaft

COPY --from=builder /usr/src/xaft/target/x86_64-unknown-linux-musl/release/xaft /usr/local/bin/xaft

# Default working directory
WORKDIR /workspace
USER xaft

ENTRYPOINT ["xaft"]
CMD ["--help"]
```

**Docker Compose for Development:**

```yaml
version: '3.8'
services:
  xaft:
    image: ghcr.io/xaft-dev/xaft:latest
    build: .
    volumes:
      - ./project:/workspace
      - xaft-config:/home/xaft/.config/xaft
    environment:
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - XAFT_BUDGET_LIMIT=5.0
      - XAFT_MODEL=claude-sonnet-4-20250514
    working_dir: /workspace

volumes:
  xaft-config:
```

### 2.4 Static Binary

For air-gapped environments, older Linux distributions, and minimal containers:

```bash
# Download pre-built static binary (Linux x86_64)
curl -L https://github.com/xaft-dev/xaft/releases/latest/download/xaft-x86_64-unknown-linux-musl.tar.gz | tar xz
chmod +x xaft
sudo mv xaft /usr/local/bin/

# Download for macOS (Apple Silicon)
curl -L https://github.com/xaft-dev/xaft/releases/latest/download/xaft-aarch64-apple-darwin.tar.gz | tar xz
chmod +x xaft
sudo mv xaft /usr/local/bin/

# Download for Windows
curl -LO https://github.com/xaft-dev/xaft/releases/latest/download/xaft-x86_64-pc-windows-msvc.zip
Expand-Archive xaft-x86_64-pc-windows-msvc.zip
```

**Static Build Configuration:**

```toml
# .cargo/config.toml for musl static builds
[target.x86_64-unknown-linux-musl]
linker = "x86_64-linux-musl-gcc"
rustflags = ["-C", "target-feature=+crt-static"]

[target.aarch64-unknown-linux-musl]
linker = "aarch64-linux-musl-gcc"
rustflags = ["-C", "target-feature=+crt-static"]
```

---

## 3. Cross-Compilation

### 3.1 Supported Targets

| Target | Tier | Status | Notes |
|--------|------|--------|-------|
| `x86_64-unknown-linux-gnu` | 1 | ✅ Full | Primary Linux target |
| `x86_64-unknown-linux-musl` | 2 | ✅ Full | Static Linux binary |
| `aarch64-unknown-linux-gnu` | 2 | ✅ Full | ARM64 Linux (AWS Graviton) |
| `aarch64-unknown-linux-musl` | 2 | ✅ Full | Static ARM64 Linux |
| `x86_64-apple-darwin` | 1 | ✅ Full | macOS Intel |
| `aarch64-apple-darwin` | 1 | ✅ Full | macOS Apple Silicon |
| `x86_64-pc-windows-msvc` | 1 | ✅ Full | Windows x64 |
| `aarch64-pc-windows-msvc` | 2 | ⚠️ Partial | Windows ARM64 |
| `x86_64-unknown-freebsd` | 3 | ⚠️ Best-effort | FreeBSD |

### 3.2 Cross-Compilation Setup

```bash
# Add cross-compilation targets
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-gnu
rustup target add aarch64-apple-darwin

# Build for Linux musl (static binary)
cargo build --release --target x86_64-unknown-linux-musl

# Build for ARM64 Linux (for AWS Graviton, Raspberry Pi)
cargo build --release --target aarch64-unknown-linux-gnu

# Using cross for easier cross-compilation
cross build --release --target x86_64-unknown-linux-musl
cross build --release --target aarch64-unknown-linux-gnu
```

### 3.3 CI Cross-Compilation Matrix

```yaml
# .github/workflows/release.yml
name: Release

on:
  push:
    tags: ['v*']

jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            name: xaft-linux-amd64
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
            name: xaft-linux-arm64
          - target: x86_64-apple-darwin
            os: macos-latest
            name: xaft-macos-amd64
          - target: aarch64-apple-darwin
            os: macos-latest
            name: xaft-macos-arm64
          - target: x86_64-pc-windows-msvc
            os: windows-latest
            name: xaft-windows-amd64.exe

    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - name: Build
        run: cargo build --release --target ${{ matrix.target }}
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.name }}
          path: target/${{ matrix.target }}/release/xaft*
```

---

## 4. API Key Management

### 4.1 Configuration Hierarchy

xaft resolves API keys from multiple sources in priority order:

```
┌──────────────────────────────────────────────────────────────────────┐
│                  API Key Resolution Order                             │
│                                                                       │
│  1. Command-line flag:     --api-key <KEY>                           │
│  2. Environment variable:  ANTHROPIC_API_KEY / OPENAI_API_KEY        │
│  3. Project config:        .xaft/config.toml                         │
│  4. User config:           ~/.config/xaft/config.toml                │
│  5. Credential store:      OS keychain (keyring)                     │
│  6. .env file:             .env in project root                      │
│                                                                       │
│  Higher priority overrides lower.                                     │
└──────────────────────────────────────────────────────────────────────┘
```

### 4.2 Configuration File Format

```toml
# ~/.config/xaft/config.toml

[default]
model = "claude-sonnet-4-20250514"
max_turns = 50
budget_limit = 10.0

[providers.anthropic]
api_key = "sk-ant-..."  # Or use keychain storage
base_url = "https://api.anthropic.com"
models = ["claude-sonnet-4-20250514", "claude-opus-4-20250514", "claude-haiku-3-5-20241022"]

[providers.openai]
api_key = "sk-..."  # Or use keychain storage
base_url = "https://api.openai.com/v1"
models = ["gpt-4.1", "gpt-4.1-mini", "gpt-4.1-nano"]

[providers.google]
api_key = "AIza..."  # Or use keychain storage
models = ["gemini-2.5-pro"]

[agent.coder]
model = "claude-sonnet-4-20250514"
system_prompt_additions = ["Focus on Rust best practices"]

[agent.reviewer]
model = "claude-opus-4-20250514"
system_prompt_additions = ["Be thorough, check for security issues"]

[sandbox]
backend = "docker"
memory_mb = 1024
cpu_cores = 2.0
network = "outbound-only"

[logging]
level = "info"
format = "human"
output = "/tmp/xaft.log"

[history]
enabled = true
path = "~/.config/xaft/history"
max_entries = 10000
```

### 4.3 Credential Storage

```rust
/// Secure credential storage using the OS keychain
pub struct CredentialStore {
    backend: Box<dyn KeyringBackend>,
}

impl CredentialStore {
    /// Store an API key in the OS keychain
    pub async fn store(&self, provider: &str, key: &str) -> Result<(), CredentialError> {
        self.backend.set_password(
            &format!("xaft.{}", provider),
            key,
        ).await
    }

    /// Retrieve an API key from the OS keychain
    pub async fn retrieve(&self, provider: &str) -> Result<String, CredentialError> {
        self.backend.get_password(
            &format!("xaft.{}", provider),
        ).await
    }

    /// Delete a stored API key
    pub async fn delete(&self, provider: &str) -> Result<(), CredentialError> {
        self.backend.delete_password(
            &format!("xaft.{}", provider),
        ).await
    }

    /// List all providers with stored credentials
    pub async fn list_providers(&self) -> Result<Vec<String>, CredentialError> {
        self.backend.list_entries("xaft.").await
    }
}

/// Keyring backends
pub enum KeyringBackend {
    /// macOS Keychain
    MacOs,
    /// Linux Secret Service (GNOME Keyring / KDE Wallet)
    SecretService,
    /// Windows Credential Manager
    WindowsCredentialManager,
    /// Encrypted file-based fallback
    FileBased { path: PathBuf, key: [u8; 32] },
}

/// CLI commands for credential management
impl XaftCli {
    /// xaft auth login --provider anthropic
    pub async fn cmd_auth_login(&self, provider: &str) -> Result<(), CliError> {
        let key = dialoguer::Password::new()
            .with_prompt(format!("Enter {} API key", provider))
            .interact()?;

        let store = CredentialStore::new()?;
        store.store(provider, &key).await?;

        // Validate the key
        let client = LlmClient::new(provider, &key);
        match client.validate_key().await {
            Ok(_) => println!("✅ {} API key validated and stored", provider),
            Err(e) => {
                store.delete(provider).await?;
                return Err(CliError::AuthenticationFailed(provider.to_string(), e.to_string()));
            }
        }
        Ok(())
    }

    /// xaft auth logout --provider anthropic
    pub async fn cmd_auth_logout(&self, provider: &str) -> Result<(), CliError> {
        let store = CredentialStore::new()?;
        store.delete(provider).await?;
        println!("✅ {} API key removed", provider);
        Ok(())
    }

    /// xaft auth status
    pub async fn cmd_auth_status(&self) -> Result<(), CliError> {
        let store = CredentialStore::new()?;
        let providers = store.list_providers().await?;

        if providers.is_empty() {
            println!("No API keys stored.");
        } else {
            println!("Stored API keys:");
            for provider in &providers {
                let key = store.retrieve(provider).await?;
                let masked = format!("{}...{}", &key[..4.min(key.len())], &key[key.len()-4..]);
                println!("  {}: {}", provider, masked);
            }
        }
        Ok(())
    }
}
```

---

## 5. CI Environment Configuration

### 5.1 GitHub Actions

```yaml
# .github/workflows/xaft-review.yml
name: AI Code Review

on:
  pull_request:
    types: [opened, synchronize]

jobs:
  ai-review:
    runs-on: ubuntu-latest
    container:
      image: ghcr.io/xaft-dev/xaft:latest

    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0  # Full history for git diff

      - name: Run xaft review
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
        run: |
          xaft ci \
            --review-only \
            --budget-limit 1.0 \
            --max-turns 15 \
            --report-format github-actions \
            --base-branch origin/main

      - name: Upload review report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: xaft-review-report
          path: .xaft/review-report.json
```

### 5.2 GitLab CI

```yaml
# .gitlab-ci.yml
stages:
  - ai-review

xaft-review:
  stage: ai-review
  image: ghcr.io/xaft-dev/xaft:latest
  variables:
    XAFT_PROVIDER: "anthropic"
    XAFT_MODEL: "claude-sonnet-4-20250514"
    XAFT_BUDGET_LIMIT: "1.0"
  script:
    - xaft ci --review-only --report-format gitlab > xaft-review.json
  artifacts:
    paths:
      - xaft-review.json
    expire_in: 7 days
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
```

### 5.3 Generic CI Configuration

```bash
#!/bin/bash
# ci/xaft-review.sh — Generic CI wrapper

set -euo pipefail

# Configuration via environment variables
XAFT_PROVIDER="${XAFT_PROVIDER:-anthropic}"
XAFT_MODEL="${XAFT_MODEL:-claude-sonnet-4-20250514}"
XAFT_BUDGET_LIMIT="${XAFT_BUDGET_LIMIT:-2.0}"
XAFT_MAX_TURNS="${XAFT_MAX_TURNS:-30}"
XAFT_REPORT_FORMAT="${XAFT_REPORT_FORMAT:-json}"

# Ensure API key is set
if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -z "${OPENAI_API_KEY:-}" ]; then
  echo "ERROR: No API key configured. Set ANTHROPIC_API_KEY or OPENAI_API_KEY."
  exit 1
fi

# Run xaft
xaft ci \
  --provider "$XAFT_PROVIDER" \
  --model "$XAFT_MODEL" \
  --budget-limit "$XAFT_BUDGET_LIMIT" \
  --max-turns "$XAFT_MAX_TURNS" \
  --report-format "$XAFT_REPORT_FORMAT" \
  2>&1 | tee xaft-output.log

# Check exit code
XAFT_EXIT=${PIPESTATUS[0]}

if [ $XAFT_EXIT -eq 0 ]; then
  echo "✅ xaft review completed successfully"
elif [ $XAFT_EXIT -eq 2 ]; then
  echo "⚠️ xaft found issues that need attention"
else
  echo "❌ xaft encountered an error"
fi

exit $XAFT_EXIT
```

---

## 6. Environment Variables Reference

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `ANTHROPIC_API_KEY` | Anthropic API key | — | If using Anthropic |
| `OPENAI_API_KEY` | OpenAI API key | — | If using OpenAI |
| `GOOGLE_API_KEY` | Google AI API key | — | If using Gemini |
| `XAFT_PROVIDER` | Default LLM provider | `anthropic` | No |
| `XAFT_MODEL` | Default model name | `claude-sonnet-4-20250514` | No |
| `XAFT_BUDGET_LIMIT` | Maximum spend per session (USD) | `10.0` | No |
| `XAFT_MAX_TURNS` | Maximum agent turns | `50` | No |
| `XAFT_CONFIG_DIR` | Configuration directory | `~/.config/xaft` | No |
| `XAFT_DATA_DIR` | Data directory | `~/.local/share/xaft` | No |
| `XAFT_LOG_LEVEL` | Log level | `info` | No |
| `XAFT_LOG_FORMAT` | Log format (json/human/compact) | `human` | No |
| `XAFT_LOG_FILE` | Log output file | stderr | No |
| `XAFT_SANDBOX` | Sandbox backend (docker/podman/native) | `native` | No |
| `XAFT_NO_COLOR` | Disable colored output | `false` | No |
| `XAFT_NO_TUI` | Disable TUI, use plain output | `false` | No |
| `XAFT_DEBUG` | Enable debug mode | `false` | No |
| `XAFT_DEBUG_LLM` | Log full LLM payloads | `false` | No |
| `XAFT_DEBUG_STATE` | Dump state transitions | `false` | No |
| `XAFT_HTTP_PROXY` | HTTP proxy URL | — | No |
| `XAFT_HTTPS_PROXY` | HTTPS proxy URL | — | No |
| `XAFT_VERIFY_SSL` | Verify TLS certificates | `true` | No |

---

## 7. Project Configuration (.xaft/config.toml)

### 7.1 Project-Level Configuration

Project-level config overrides user-level config and is checked into version control,
ensuring team consistency.

```toml
# .xaft/config.toml — Project-level configuration

[project]
name = "my-rust-app"
language = "rust"

[agent.default]
model = "claude-sonnet-4-20250514"
system_prompt_file = ".xaft/prompts/default.md"
max_turns = 30

[agent.reviewer]
model = "claude-opus-4-20250514"
system_prompt_file = ".xaft/prompts/reviewer.md"

[tools]
# Disable specific tools for this project
disabled = ["shell_exec"]

# Tool-specific configuration
[tools.shell_exec]
allowed_commands = [
    "cargo *",
    "rustfmt *",
    "clippy *",
    "git *",
]
blocked_commands = [
    "rm -rf /",
    "curl * | sh",
    "sudo *",
]
timeout_seconds = 60

[tools.file_write]
# Require approval for files outside src/
require_approval_pattern = "^(?!src/).*$"

[sandbox]
enabled = true
backend = "docker"
memory_mb = 2048
cpu_cores = 2.0

[git]
# Auto-commit after each agent turn
auto_commit = false
# Branch naming for xaft-created branches
branch_prefix = "xaft/"
# Commit message prefix
commit_prefix = "xaft: "

[formatting]
# Run formatter after file edits
format_on_write = true
formatter_command = "rustfmt --edition 2021"
```

### 7.2 .xaftignore

```gitignore
# .xaftignore — Files and directories xaft should not access
.env
.env.*
*.pem
*.key
secrets/
credentials/
node_modules/
target/
.git/
```

---

## 8. First-Run Experience

### 8.1 Onboarding Flow

```
┌──────────────────────────────────────────────────────────────────────┐
│                    xaft First-Run Onboarding                          │
│                                                                       │
│  $ xaft                                                               │
│                                                                       │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  Welcome to xaft! 🦀                                        ║    │
│  ║                                                              ║    │
│  ║  The autonomous coding CLI built on the agtrs framework.     ║    │
│  ║                                                              ║    │
│  ║  Let's get you set up:                                       ║    │
│  ║                                                              ║    │
│  ║  [1/3] Configure LLM provider                               ║    │
│  ║    Which provider would you like to use?                     ║    │
│  ║    > Anthropic (Claude)                                      ║    │
│  ║      OpenAI (GPT)                                           ║    │
│  ║      Google (Gemini)                                        ║    │
│  ║                                                              ║    │
│  ║  [2/3] Enter API key                                        ║    │
│  ║    Enter your Anthropic API key: ****-****-****-****         ║    │
│  ║    ✅ Key validated successfully                             ║    │
│  ║                                                              ║    │
│  ║  [3/3] Configure workspace                                  ║    │
│  ║    Detected project: Rust (Cargo.toml)                      ║    │
│  ║    Default model: claude-sonnet-4-20250514                        ║    │
│  ║    Budget limit: $10.00/session                             ║    │
│  ║    > Accept defaults                                        ║    │
│  ║      Customize                                              ║    │
│  ║                                                              ║    │
│  ║  ✅ Setup complete!                                          ║    │
│  ║                                                              ║    │
│  ║  Try your first command:                                    ║    │
│  ║    xaft run "Explain the project structure"                  ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
└──────────────────────────────────────────────────────────────────────┘
```

### 8.2 Onboarding Implementation

```rust
pub async fn run_onboarding() -> Result<(), CliError> {
    // Step 1: Check if already configured
    let config_path = dirs::config_dir()
        .unwrap_or_default()
        .join("xaft/config.toml");

    if config_path.exists() {
        println!("xaft is already configured. Run `xaft config` to modify settings.");
        return Ok(());
    }

    // Step 2: Select provider
    let provider = Select::new()
        .with_prompt("Which LLM provider would you like to use?")
        .items(&["Anthropic (Claude)", "OpenAI (GPT)", "Google (Gemini)"])
        .default(0)
        .interact()?;

    let provider_name = match provider {
        0 => "anthropic",
        1 => "openai",
        2 => "google",
        _ => unreachable!(),
    };

    // Step 3: Enter API key
    let api_key = Password::new()
        .with_prompt(format!("Enter your {} API key", provider_name))
        .interact()?;

    // Validate the key
    let client = LlmClient::new(provider_name, &api_key);
    client.validate_key().await.map_err(|e| {
        CliError::AuthenticationFailed(provider_name.to_string(), e.to_string())
    })?;

    // Store in keychain
    let store = CredentialStore::new()?;
    store.store(provider_name, &api_key).await?;

    println!("✅ Key validated and stored securely");

    // Step 4: Detect project and create config
    let project = detect_project_type(".")?;
    let default_model = match provider_name {
        "anthropic" => "claude-sonnet-4-20250514",
        "openai" => "gpt-4.1",
        "google" => "gemini-2.5-pro",
        _ => "claude-sonnet-4-20250514",
    };

    let config = XaftConfig {
        default_provider: provider_name.to_string(),
        default_model: default_model.to_string(),
        budget_limit: 10.0,
        max_turns: 50,
        project_type: Some(project),
    };

    config.save_to(&config_path)?;
    println!("✅ Configuration saved to {}", config_path.display());

    println!("\nTry your first command:");
    println!("  xaft run \"Explain the project structure\"");

    Ok(())
}
```

---

## 9. Update & Version Management

```bash
# Check current version
xaft --version

# Check for updates
xaft update --check

# Update to latest version
xaft update

# Update to a specific version
xaft update --version 0.2.0

# Rollback to previous version
xaft update --rollback
```

```rust
/// Self-update implementation
pub async fn self_update(target_version: Option<&str>) -> Result<(), UpdateError> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = fetch_latest_version().await?;

    let target = target_version.unwrap_or(&latest);

    if current == target {
        println!("xaft is already up to date (v{})", current);
        return Ok(());
    }

    println!("Updating xaft: v{} → v{}", current, target);

    // Download the new binary
    let platform = detect_platform();
    let url = format!(
        "https://github.com/xaft-dev/xaft/releases/download/v{}/xaft-{}",
        target, platform
    );

    let temp = download_binary(&url).await?;

    // Verify checksum
    let expected_hash = fetch_checksum(target, &platform).await?;
    verify_checksum(&temp, &expected_hash)?;

    // Replace current binary
    let current_exe = std::env::current_exe()?;
    replace_binary(&current_exe, &temp)?;

    println!("✅ Updated to v{}", target);
    Ok(())
}
```

---

## 10. Summary

xaft's deployment strategy leverages Rust's single-binary output model to provide
the simplest possible installation experience. With six installation methods
(cargo install, Homebrew, Docker, static binary, GitHub Releases, npm wrapper),
xaft is accessible to every developer workflow. The credential management system
uses OS keychain integration for secure storage, and the CI configuration supports
GitHub Actions, GitLab CI, and generic environments out of the box. The first-run
onboarding experience guides new users through provider selection, API key
configuration, and workspace detection in under 60 seconds.
