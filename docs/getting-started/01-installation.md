# Installation

xaft distributes as a single statically-linked binary with no runtime dependencies. This page covers every installation method, the system prerequisites you need before running it, and the full build-from-source procedure for contributors who want to work on the codebase itself.

## Prerequisites

Before installing xaft, verify that your environment meets these requirements. xaft is built and tested on Linux (x86_64 and aarch64) and macOS (Apple Silicon and Intel). Windows is not currently supported due to the sandboxed shell executor's reliance on POSIX process isolation primitives.

| Requirement | Minimum Version | Why |
|-------------|----------------|-----|
| **Rust toolchain** | 1.75.0+ | Required only for building from source. xaft uses `impl Trait` in associated types and stabilized async closures that landed in 1.75. |
| **Git** | 2.30+ | xaft's worktree management uses `git worktree add` with the `--lock` flag introduced in 2.30. Older versions will fail during workspace initialization. |
| **SQLite** | 3.35.0+ | The session store relies on `RETURNING` clauses and WAL mode. Most modern distributions ship 3.38+, which is well above the floor. xaft statically links SQLite via `libsqlite3-sys` with the `bundled` feature, so a system library is not strictly required—but if you disable `bundled`, the system version must meet this minimum. |
| **C compiler** (build only) | GCC or Clang | Needed by `libsqlite3-sys` and `openssl-sys` when building from source without pre-built crates. On Debian/Ubuntu, install `build-essential` and `pkg-config`. |

No Python, Node.js, or other language runtime is needed. The binary is self-contained.

### API Keys

xaft requires at least one LLM provider API key to function. Set the key as an environment variable before running any task:

```bash
# Anthropic Claude (recommended for coding tasks)
export ANTHROPIC_API_KEY="sk-ant-..."

# OpenAI GPT-4o (alternative)
export OPENAI_API_KEY="sk-..."
```

If both keys are present, xaft uses the provider specified in your configuration, defaulting to Anthropic. The `CostedProvider` wrapper can route between them based on token cost thresholds—see the architecture documentation for details.

## Binary Download

The fastest way to get started is to download a pre-built binary from the [GitHub Releases](https://github.com/nicholasgasior/xaft/releases) page. Each release includes statically-linked binaries for supported platforms:

```bash
# Linux x86_64
curl -LO https://github.com/nicholasgasior/xaft/releases/latest/download/xaft-x86_64-unknown-linux-gnu.tar.gz
tar xzf xaft-x86_64-unknown-linux-gnu.tar.gz
sudo mv xaft /usr/local/bin/

# macOS Apple Silicon
curl -LO https://github.com/nicholasgasior/xaft/releases/latest/download/xaft-aarch64-apple-darwin.tar.gz
tar xzf xaft-aarch64-apple-darwin.tar.gz
sudo mv xaft /usr/local/bin/
```

Verify the installation:

```bash
xaft --version
# xaft 0.8.1
```

The binary includes shell completion scripts embedded at compile time. Generate them on demand:

```bash
# Bash
xaft completions bash > ~/.local/share/bash-completion/completions/xaft

# Zsh
xaft completions zsh > "${fpath[1]}/_xaft"

# Fish
xaft completions fish > ~/.config/fish/completions/xaft.fish
```

## Installing with Cargo

If you have a Rust toolchain installed, you can install xaft directly from the registry:

```bash
cargo install xaft
```

This compiles xaft from source with default features. The binary is placed in `~/.cargo/bin/`, which must be on your `$PATH`. Compilation typically takes 3–5 minutes on a modern machine due to the SQLite and TLS static linking.

To install a specific version:

```bash
cargo install xaft --version 0.8.1
```

### Feature Flags

xaft exposes optional feature flags that control which provider crates are compiled in. By default, both Anthropic and OpenAI support are enabled. If you only use one provider, you can reduce compile time and binary size by disabling the other:

```bash
# Anthropic only (smaller binary)
cargo install xaft --no-default-features --features anthropic

# OpenAI only
cargo install xaft --no-default-features --features openai

# Both (default)
cargo install xaft --features anthropic,openai
```

The `tui` feature is enabled by default and includes the Ratatui-based interactive dashboard. Disable it for headless/CI usage:

```bash
cargo install xaft --no-default-features --features anthropic,openai
```

## Building from Source

Building from source is necessary when you want to contribute patches, run the test suite, or enable experimental features that are not yet in a released version. The xaft workspace uses Cargo's workspace feature extensively, so the build procedure is a single `cargo build` invocation at the workspace root.

### Clone the Repository

```bash
git clone https://github.com/nicholasgasior/xaft.git
cd xaft
```

The repository uses a standard Cargo workspace layout. The top-level `Cargo.toml` defines all seven crates as workspace members:

```
xaft/
├── Cargo.toml          # workspace root
├── crates/
│   ├── xaft/           # binary entry point
│   ├── xaft-cli/       # argument parsing
│   ├── xaft-config/    # config loading
│   ├── xaft-runtime/   # orchestration
│   ├── xaft-agent/     # agent implementations
│   ├── xaft-tools/     # tool registry
│   ├── xaft-tui/       # terminal UI
│   └── xaft-session/   # SQLite persistence
└── agtrs/              # framework crates (submodule)
```

The `agtrs` directory is a Git submodule containing the framework crates. Initialize it during clone:

```bash
git clone --recurse-submodules https://github.com/nicholasgasior/xaft.git
```

If you already cloned without `--recurse-submodules`:

```bash
git submodule update --init --recursive
```

### Build

```bash
# Debug build (fast compilation, slow runtime, full debug info)
cargo build

# Release build (slow compilation, fast runtime, optimized)
cargo build --release
```

The debug binary is at `target/debug/xaft`. The release binary is at `target/release/xaft`. For day-to-day development, the debug build is sufficient. For benchmarking or long-running sessions, always use the release build—LLM API call overhead dominates, but the event loop and SQLite write path benefit measurably from optimization.

### Run the Test Suite

```bash
# All tests across all workspace members
cargo test --workspace

# Tests for a specific crate
cargo test -p xaft-runtime

# Integration tests only (requires ANTHROPIC_API_KEY or OPENAI_API_KEY)
cargo test --workspace --test '*'
```

Unit tests do not require API keys. They mock the LLM provider using a stub that returns canned responses. Integration tests that hit real APIs are gated behind the `live-api` feature flag and will be skipped if no key is present.

### Lint and Format

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI enforces both `clippy` and `rustfmt` with zero warnings. Contributions that do not pass these checks will not be merged.

## Verifying the Installation

Regardless of how you installed xaft, run these commands to confirm everything is working:

```bash
# Print version and build metadata
xaft --version

# Show the resolved configuration (merges all 6 config layers)
xaft config show

# Run a dry-run task (no API calls, validates config and tool loading)
xaft run --dry-run "Create a hello world Rust project"
```

If `xaft config show` prints a valid configuration with your API key redacted (shown as `sk-ant-****`), and `--dry-run` exits 0, your installation is complete. Proceed to the [Quick Start](02-quick-start.md) guide to run your first real task.
