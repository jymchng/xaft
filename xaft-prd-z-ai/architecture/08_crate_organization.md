# 08 — Crate Organization

> Rust crate layout, dependency graph, feature flags, and subsystem mapping.
> How xaft extends the agtrs workspace with new crates for CLI, TUI, indexing,
> shell, MCP, configuration, and session management.

---

## Overview

xaft is organized as a Cargo workspace extending the agtrs framework. The agtrs crates provide the core agent runtime, and xaft crates provide the application layer — CLI parsing, TUI rendering, configuration, session management, shell execution, MCP integration, and code indexing.

The workspace is designed for:

1. **Incremental compilation** — Crates are split to minimize rebuilds when changing application code.
2. **Clear boundaries** — Each crate has a well-defined responsibility and public API.
3. **Feature gating** — Optional functionality (MCP, SSE, semantic search) is behind feature flags.
4. **Testability** — Crates can be tested independently with mocked dependencies.

---

## Workspace Structure

```
xaft/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── .xaft.toml                    # Default config template
├── crates/
│   ├── agtrs-core/               # Agent trait, AgentExecutor, lifecycle
│   ├── agtrs-signal/             # SignalBus, typed events
│   ├── agtrs-llm/                # LlmProvider, CostedProvider, FallbackProvider
│   ├── agtrs-workspace/          # WorkspaceStore, FileEditor
│   ├── agtrs-git/                # GitRepo, WorktreeGuard
│   ├── agtrs-planner/            # OneShot, IterativeRefinement, TreeOfThought
│   ├── agtrs-memory/             # MemoryStore, ConversationStore, Scratchpad
│   ├── agtrs-tool/               # Tool trait, ErasedTool, SubagentTool
│   ├── agtrs-team/               # TeamMode, AgentMessageBus, Chain, Workflow
│   ├── agtrs-guardrail/          # Guardrail trait, built-in guardrails
│   ├── xaft-cli/                 # CLI entry point (clap, tracing init)
│   ├── xaft-runtime/             # XaftRuntime, boot sequence, main loop
│   ├── xaft-agent/               # XaftAgent, PlanModeAgent, lifecycle hooks
│   ├── xaft-tools/               # All built-in tool implementations
│   ├── xaft-tui/                 # Ratatui-based terminal UI
│   ├── xaft-config/              # Configuration loading and validation
│   ├── xaft-session/             # Session persistence, resume, replay
│   ├── xaft-shell/               # Shell command execution, sandboxing
│   ├── xaft-index/               # Code indexing and semantic search
│   ├── xaft-mcp/                 # Model Context Protocol client
│   ├── xaft-stream/              # Streaming engine, SSE bridge
│   └── xaft-proc-macros/         # #[tool] procedural macro
├── tests/
│   ├── integration/              # Cross-crate integration tests
│   └── fixtures/                 # Test repositories and configs
└── benches/                      # Performance benchmarks
```

---

## Crate Descriptions

### agtrs-* Crates (Framework)

| Crate | Responsibility | Key Types | Est. LOC |
|---|---|---|---|
| `agtrs-core` | Agent trait, AgentExecutor, ReAct loop | `Agent`, `AgentExecutor`, `AgentContext`, `AgentOutcome` | 5,000 |
| `agtrs-signal` | Event bus with typed signals | `SignalBus`, `Signal`, `SignalChannel` | 2,000 |
| `agtrs-llm` | LLM provider abstraction | `LlmProvider`, `CostedProvider`, `FallbackProvider`, `LlmRequest`, `LlmResponse` | 4,000 |
| `agtrs-workspace` | File state and editing | `WorkspaceStore`, `FileEditor`, `OnDiskWorkspaceStore`, `InMemoryWorkspaceStore` | 3,500 |
| `agtrs-git` | Git operations | `GitRepo`, `WorktreeGuard`, `GitStatus` | 2,500 |
| `agtrs-planner` | Task planning | `OneShotPlanner`, `IterativeRefinementPlanner`, `TreeOfThoughtPlanner`, `TaskPlan` | 3,000 |
| `agtrs-memory` | Memory and conversation | `MemoryStore`, `ConversationStore`, `Scratchpad`, `MemoryEntry` | 2,000 |
| `agtrs-tool` | Tool system | `Tool`, `ErasedTool`, `ToolContext`, `SubagentTool<T>`, `HookedTool` | 4,000 |
| `agtrs-team` | Multi-agent coordination | `TeamMode`, `AgentMessageBus`, `Chain`, `Workflow` | 3,000 |
| `agtrs-guardrail` | Safety guardrails | `Guardrail`, `GuardrailVerdict`, built-in guardrails | 1,500 |

### xaft-* Crates (Application)

| Crate | Responsibility | Key Types | Est. LOC |
|---|---|---|---|
| `xaft-cli` | CLI argument parsing, entry point | `XaftCli`, `CliArgs`, `Commands` | 1,000 |
| `xaft-runtime` | Top-level orchestration | `XaftRuntime`, `RuntimeConfig`, boot sequence | 3,000 |
| `xaft-agent` | Agent implementations | `XaftAgent`, `PlanModeAgent`, lifecycle hooks | 4,000 |
| `xaft-tools` | All built-in tool implementations | `ReadFileTool`, `EditFileTool`, `BashExecTool`, `GitStatusTool`, etc. | 6,000 |
| `xaft-tui` | Terminal UI | `TuiApp`, `TuiEventConsumer`, `TokenRenderer`, panels | 5,000 |
| `xaft-config` | Configuration loading | `XaftConfig`, `ConfigLoader`, validation | 1,500 |
| `xaft-session` | Session persistence | `SessionManager`, `SqliteSessionStore`, resume/replay | 2,000 |
| `xaft-shell` | Shell execution | `ShellExecutor`, `SandboxConfig`, output streaming | 2,000 |
| `xaft-index` | Code indexing | `CodeIndex`, `SemanticSearch`, tree-sitter integration | 3,000 |
| `xaft-mcp` | MCP protocol client | `McpClient`, `McpToolRegistration`, transport | 2,000 |
| `xaft-stream` | Streaming engine | `StreamEvent`, `StreamConsumer`, `SseBridge`, backpressure | 2,500 |
| `xaft-proc-macros` | Procedural macros | `#[tool]`, `#[signal]` | 1,000 |

---

## Dependency Graph

```
                    ┌─────────┐
                    │xaft-cli │
                    └────┬────┘
                         │
                         ▼
                    ┌──────────┐
                    │xaft-     │
                    │runtime   │
                    └────┬─────┘
                         │
           ┌─────────────┼─────────────┬──────────────┐
           │             │             │              │
           ▼             ▼             ▼              ▼
     ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐
     │xaft-     │  │xaft-     │  │xaft-     │  │xaft-     │
     │agent     │  │tools     │  │tui       │  │session   │
     └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘
          │             │             │              │
          │        ┌────┼────┐       │              │
          │        │    │    │       │              │
          ▼        ▼    ▼    ▼       ▼              ▼
    ┌──────────┐ ┌────┐┌────┐┌──────┐┌──────────┐┌──────┐
    │agtrs-    │ │xaft││xaft││xaft- ││agtrs-    ││agtrs-│
    │core      │ │shell││mcp ││stream││memory    ││signal│
    └────┬─────┘ └──┬─┘└──┬─┘└──┬───┘└────┬─────┘└──┬───┘
         │          │      │      │          │         │
         ▼          ▼      │      ▼          ▼         ▼
    ┌──────────┐ ┌──────┐  │  ┌──────────┐┌──────────┐
    │agtrs-    │ │agtrs-│  │  │agtrs-    ││agtrs-    │
    │tool      │ │git  │  │  │workspace ││llm       │
    └────┬─────┘ └──┬───┘  │  └────┬─────┘└────┬─────┘
         │          │      │       │            │
         │          │      │       ▼            │
         │          │      │  ┌──────────┐     │
         │          │      │  │agtrs-    │     │
         │          │      │  │workspace │     │
         │          │      │  └──────────┘     │
         │          ▼      ▼                    │
         │     ┌──────────┐                     │
         │     │xaft-index│                     │
         │     └──────────┘                     │
         │                                      │
         └──────────────────────────────────────┘

     ┌──────────┐  ┌──────────┐  ┌───────────┐
     │agtrs-    │  │agtrs-    │  │agtrs-      │
     │planner   │  │team     │  │guardrail   │
     └──────────┘  └──────────┘  └───────────┘

     ┌──────────────┐
     │xaft-proc-    │
     │macros        │
     └──────────────┘  (used by xaft-tools at compile time)
```

### Dependency Rules

1. **agtrs-* crates never depend on xaft-* crates.** The framework is application-agnostic.
2. **xaft-runtime is the integration point.** It depends on all other xaft crates and wires them together.
3. **xaft-cli depends only on xaft-runtime and xaft-config.** It is the thin entry point.
4. **Circular dependencies are forbidden.** If A depends on B, B must not depend on A (directly or transitively).
5. **agtrs-* crates may depend on other agtrs-* crates** following the graph above.
6. **xaft-proc-macros is a proc-macro crate** — it has no runtime dependencies.

---

## Detailed Crate Specifications

### xaft-cli

```toml
# crates/xaft-cli/Cargo.toml
[package]
name = "xaft-cli"
version = "0.1.0"
edition = "2021"

[dependencies]
xaft-runtime = { path = "../xaft-runtime" }
xaft-config = { path = "../xaft-config" }
clap = { version = "4", features = ["derive", "env"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tokio = { version = "1", features = ["full"] }
anyhow = "1"

[[bin]]
name = "xaft"
path = "src/main.rs"
```

```rust
// crates/xaft-cli/src/main.rs

use clap::Parser;
use xaft_cli::XaftCli;
use xaft_runtime::XaftRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = XaftCli::parse();

    // Initialize tracing
    xaft_cli::init_tracing(&cli)?;

    // Bootstrap runtime
    let runtime = XaftRuntime::bootstrap(cli).await?;

    // Run
    let exit_code = runtime.run().await?;

    std::process::exit(exit_code.as_i32())
}
```

```rust
// crates/xaft-cli/src/args.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xaft", about = "Autonomous coding CLI")]
pub struct XaftCli {
    /// The task prompt (shorthand for `xaft run "prompt"`)
    pub prompt: Option<String>,

    /// Configuration file path
    #[arg(long, env = "XAFT_CONFIG", global = true)]
    pub config: Option<String>,

    /// Model to use
    #[arg(long, env = "XAFT_MODEL", global = true)]
    pub model: Option<String>,

    /// Session budget in USD
    #[arg(long, env = "XAFT_BUDGET", global = true)]
    pub budget: Option<f64>,

    /// Auto-approve all tool calls (dangerous!)
    #[arg(long, global = true)]
    pub auto_approve: bool,

    /// Run in headless mode (no TUI, JSON output)
    #[arg(long, global = true)]
    pub headless: bool,

    /// Planner mode
    #[arg(long, default_value = "auto")]
    pub planner: PlannerChoice,

    /// Maximum turns
    #[arg(long, default_value = "50")]
    pub max_turns: u32,

    /// Log level
    #[arg(long, default_value = "info")]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run a task
    Run {
        /// The task prompt
        prompt: String,
    },
    /// Resume a previous session
    Resume {
        /// Session ID to resume
        session_id: String,
    },
    /// List sessions
    Sessions,
    /// Show session details
    Show {
        session_id: String,
    },
    /// Initialize xaft configuration
    Init,
    /// Check configuration and provider connectivity
    Doctor,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum PlannerChoice {
    Auto,
    OneShot,
    Iterative,
    TreeOfThought,
}
```

---

### xaft-runtime

```toml
[package]
name = "xaft-runtime"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-core = { path = "../agtrs-core" }
agtrs-signal = { path = "../agtrs-signal" }
agtrs-llm = { path = "../agtrs-llm" }
agtrs-workspace = { path = "../agtrs-workspace" }
agtrs-git = { path = "../agtrs-git" }
agtrs-planner = { path = "../agtrs-planner" }
agtrs-memory = { path = "../agtrs-memory" }
agtrs-tool = { path = "../agtrs-tool" }
agtrs-team = { path = "../agtrs-team" }
agtrs-guardrail = { path = "../agtrs-guardrail" }
xaft-agent = { path = "../xaft-agent" }
xaft-tools = { path = "../xaft-tools" }
xaft-config = { path = "../xaft-config" }
xaft-session = { path = "../xaft-session" }
xaft-stream = { path = "../xaft-stream" }

[features]
default = ["tui", "mcp", "index"]
tui = ["xaft-tui"]
mcp = ["xaft-mcp"]
index = ["xaft-index"]
sse = ["xaft-stream/sse"]
```

---

### xaft-tui

```toml
[package]
name = "xaft-tui"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-signal = { path = "../agtrs-signal" }
xaft-stream = { path = "../xaft-stream" }
xaft-config = { path = "../xaft-config" }
ratatui = "0.27"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
syntect = "5"          # Syntax highlighting
unicode-width = "0.1"  # Proper character width calculation
```

**TUI module structure:**

```
xaft-tui/src/
├── lib.rs              # Public API
├── app.rs              # TuiApp main loop
├── consumer.rs         # StreamEvent consumer
├── renderer/
│   ├── mod.rs          # Renderer trait
│   ├── output.rs       # Agent output panel
│   ├── plan.rs         # Plan progress panel
│   ├── cost.rs         # Cost tracker panel
│   ├── tools.rs        # Tool execution panel
│   ├── diff.rs         # Diff rendering
│   └── status.rs       # Status bar
├── components/
│   ├── mod.rs
│   ├── scrollable.rs   # Scrollable text area
│   ├── progress.rs     # Progress bar
│   ├── gauge.rs        # Cost gauge
│   └── input.rs        # User input handling
└── theme.rs            # Color scheme and styling
```

---

### xaft-config

```toml
[package]
name = "xaft-config"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
directories = "5"       # XDG config dirs
thiserror = "1"
tracing = "0.1"

[features]
default = []
```

**Configuration module structure:**

```
xaft-config/src/
├── lib.rs              # Public API
├── types.rs            # XaftConfig, all config types
├── loader.rs           # Config loading (CLI > env > file > defaults)
├── validation.rs       # Config validation rules
└── defaults.rs         # Default values
```

---

### xaft-session

```toml
[package]
name = "xaft-session"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-memory = { path = "../agtrs-memory" }
agtrs-signal = { path = "../agtrs-signal" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.7", features = ["sqlite", "runtime-tokio"] }
chrono = { version = "0.4", features = ["serde"] }
thiserror = "1"
```

---

### xaft-shell

```toml
[package]
name = "xaft-shell"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-workspace = { path = "../agtrs-workspace" }
agtrs-signal = { path = "../agtrs-signal" }
tokio = { version = "1", features = ["process", "io-util"] }
which = "6"             # Find executables in PATH
thiserror = "1"
tracing = "0.1"
```

**Shell execution architecture:**

```rust
/// Shell command executor with sandboxing and streaming.
pub struct ShellExecutor {
    config: SandboxConfig,
}

pub struct SandboxConfig {
    /// Working directory for command execution.
    pub working_dir: PathBuf,

    /// Environment variables to set.
    pub env: HashMap<String, String>,

    /// Environment variables to remove.
    pub env_remove: Vec<String>,

    /// Maximum execution time.
    pub timeout: Duration,

    /// Maximum output size (bytes).
    pub max_output_size: usize,

    /// Whether to allow network access.
    pub allow_network: bool,

    /// Allowed executables (empty = all allowed).
    pub allowed_commands: Vec<String>,

    /// Blocked command patterns.
    pub blocked_patterns: Vec<regex::Regex>,
}

impl ShellExecutor {
    /// Execute a command with streaming output.
    pub async fn execute_streaming(
        &self,
        command: &str,
        ctx: &ToolContext,
    ) -> Result<ShellResult, ShellError> {
        // Validate command
        self.validate_command(command)?;

        // Build process
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.config.working_dir)
            .env_clear();

        // Set allowed environment variables
        for (key, value) in &self.config.env {
            cmd.env(key, value);
        }

        // Configure stdio
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Spawn process
        let mut child = cmd.spawn()
            .map_err(|e| ShellError::SpawnFailed(e.to_string()))?;

        // Stream stdout and stderr
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();

        let mut output = ShellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        };

        loop {
            tokio::select! {
                // Read stdout line
                line = stdout_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            output.stdout.push_str(&line);
                            output.stdout.push('\n');
                            ctx.progress(ToolProgressUpdate::StdoutLine(line));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            output.stderr.push_str(&format!("stdout read error: {}\n", e));
                        }
                    }
                }

                // Read stderr line
                line = stderr_lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            output.stderr.push_str(&line);
                            output.stderr.push('\n');
                            ctx.progress(ToolProgressUpdate::StderrLine(line));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            output.stderr.push_str(&format!("stderr read error: {}\n", e));
                        }
                    }
                }

                // Wait for process exit
                status = child.wait() => {
                    output.exit_code = status.ok().map(|s| s.code().unwrap_or(-1));
                    break;
                }

                // Check cancellation
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if ctx.is_cancelled() {
                        // Try SIGTERM first
                        let _ = child.kill().await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        // Force SIGKILL if still running
                        let _ = child.kill().await;
                        return Err(ShellError::Cancelled);
                    }

                    // Check timeout
                    // (timeout logic here)
                }
            }
        }

        Ok(ShellResult {
            output,
            duration: Duration::from_millis(0), // calculated
        })
    }
}
```

---

### xaft-index

```toml
[package]
name = "xaft-index"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-workspace = { path = "../agtrs-workspace" }
agtrs-signal = { path = "../agtrs-signal" }
agtrs-llm = { path = "../agtrs-llm" }
tree-sitter = "0.22"
tree-sitter-rust = "0.21"
tree-sitter-python = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-typescript = "0.21"
tantivy = "0.22"        # Full-text search
serde = { version = "1", features = ["derive"] }
rayon = "1"             # Parallel indexing
thiserror = "1"

[features]
default = ["embeddings"]
embeddings = []          # Requires embedding API access
```

**Index architecture:**

```
xaft-index/src/
├── lib.rs              # Public API
├── index.rs            # CodeIndex main struct
├── parser.rs           # Tree-sitter based code parsing
├── symbols.rs          # Symbol extraction (functions, types, etc.)
├── searcher.rs         # Search interface (keyword + semantic)
├── embeddings.rs       # Embedding generation and similarity search
├── watcher.rs          # File watcher for incremental re-indexing
└── languages/
    ├── mod.rs
    ├── rust.rs
    ├── python.rs
    ├── javascript.rs
    └── typescript.rs
```

---

### xaft-mcp

```toml
[package]
name = "xaft-mcp"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-tool = { path = "../agtrs-tool" }
agtrs-signal = { path = "../agtrs-signal" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
thiserror = "1"
tracing = "0.1"

[features]
default = ["stdio", "sse"]
stdio = []              # Standard I/O transport
sse = []                # Server-Sent Events transport
```

---

### xaft-stream

```toml
[package]
name = "xaft-stream"
version = "0.1.0"
edition = "2021"

[dependencies]
agtrs-core = { path = "../agtrs-core" }
agtrs-signal = { path = "../agtrs-signal" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
futures = "0.3"
thiserror = "1"
tracing = "0.1"

[features]
default = []
sse = ["axum", "tower-http"]
```

---

### xaft-proc-macros

```toml
[package]
name = "xaft-proc-macros"
version = "0.1.0"
edition = "2021"

[lib]
proc-macro = true

[dependencies]
syn = "2"
quote = "1"
proc-macro2 = "1"
schemars = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## Feature Flags

Feature flags control optional functionality across the workspace:

```toml
# Root Cargo.toml workspace features

[workspace.features]
# Default: full feature set
default = ["tui", "mcp", "index", "sse"]

# Terminal UI
tui = ["xaft-runtime/tui", "xaft-tui"]

# Model Context Protocol
mcp = ["xaft-runtime/mcp", "xaft-mcp"]

# Semantic code search
index = ["xaft-runtime/index", "xaft-index"]

# SSE remote access
sse = ["xaft-runtime/sse", "xaft-stream/sse"]

# Embedding-based search (requires API key)
embeddings = ["xaft-index/embeddings"]

# Extra language support in tree-sitter
lang-go = []
lang-java = []
lang-cpp = []

# Debug features (increase logging, expose internals)
debug-signals = ["agtrs-signal/debug"]
debug-state = ["xaft-session/debug"]
```

### Feature Flag Matrix

| Feature | Enabled by Default | Depends On | Affects |
|---|---|---|---|
| `tui` | Yes | ratatui, crossterm | xaft-runtime, xaft-cli |
| `mcp` | Yes | reqwest | xaft-runtime, xaft-tools |
| `index` | Yes | tree-sitter, tantivy | xaft-runtime, xaft-tools |
| `sse` | No | axum, tower-http | xaft-runtime, xaft-stream |
| `embeddings` | Yes (if index) | agtrs-llm | xaft-index |
| `debug-signals` | No | — | agtrs-signal |
| `debug-state` | No | — | xaft-session |

### Minimal Build

For CI/CD headless mode with minimal dependencies:

```bash
cargo build --no-default-features --features "mcp"
# Excludes: tui, index, sse
# Includes: mcp (for external tool access)
# Result: ~50% smaller binary, faster compile
```

---

## Build Profiles

```toml
# Root Cargo.toml

[profile.dev]
opt-level = 0
debug = 2
incremental = true

[profile.release]
opt-level = 3
debug = 1           # Keep debug info for panic backtraces
lto = "thin"        # Thin LTO for faster builds
codegen-units = 1   # Better optimization, slower compile
strip = true        # Strip debug symbols from binary

[profile.release-fast]
inherits = "release"
lto = false         # No LTO for faster compile
codegen-units = 16  # Parallel codegen

[profile.bench]
inherits = "release"
debug = 2           # Full debug info for profiling
```

---

## Testing Strategy

### Test Categories

| Category | Scope | Location | Runs on CI |
|---|---|---|---|
| Unit tests | Single crate | `crates/*/src/` | Every push |
| Integration tests | Cross-crate | `tests/integration/` | Every push |
| TUI tests | Terminal rendering | `crates/xaft-tui/tests/` | Every push |
| E2E tests | Full xaft binary | `tests/e2e/` | Nightly |
| Benchmark | Performance | `benches/` | Nightly |
| Provider tests | LLM API | `tests/providers/` | Manual (needs API keys) |

### Test Infrastructure

```
tests/
├── integration/
│   ├── test_runtime_bootstrap.rs     # Boot sequence integration
│   ├── test_agent_lifecycle.rs       # Agent lifecycle integration
│   ├── test_file_editor.rs           # FileEditor + WorkspaceStore
│   ├── test_git_integration.rs       # GitRepo + FileEditor + WorktreeGuard
│   ├── test_streaming.rs             # Streaming + TUI + SSE
│   ├── test_tool_hooks.rs            # Tool hooks + SignalBus
│   └── test_session_persistence.rs   # Session save/resume
├── e2e/
│   ├── test_simple_edit.rs           # "Fix this bug" end-to-end
│   ├── test_multi_file_edit.rs       # Multi-file refactoring
│   ├── test_cancel_and_resume.rs     # Cancel + resume workflow
│   └── test_budget_enforcement.rs    # Budget limit enforcement
├── fixtures/
│   ├── rust_project/                 # Sample Rust project
│   ├── python_project/               # Sample Python project
│   ├── js_project/                   # Sample JavaScript project
│   └── configs/
│       ├── default.xaft.toml
│       ├── strict.xaft.toml
│       └── headless.xaft.toml
└── providers/
    └── test_openai.rs                # (requires OPENAI_API_KEY)
```

### Test Utilities

```rust
/// Test utilities shared across integration tests.
pub mod test_utils {
    use agtrs_workspace::InMemoryWorkspaceStore;
    use agtrs_signal::SignalBus;

    /// Create a test workspace with pre-populated files.
    pub fn test_workspace(files: &[(&str, &str)]) -> Arc<dyn WorkspaceStore> {
        let store = InMemoryWorkspaceStore::new(PathBuf::from("/test"));
        for (path, content) in files {
            store.write_file_internal(Path::new(path), content).unwrap();
        }
        Arc::new(store)
    }

    /// Create a test signal bus with a collector.
    pub fn test_signal_bus() -> (Arc<SignalBus>, SignalCollector) {
        let bus = Arc::new(SignalBus::new(SignalBusConfig::default()));
        let collector = SignalCollector::new(&bus);
        (bus, collector)
    }

    /// Create a mock LLM provider that returns predefined responses.
    pub fn mock_provider(responses: Vec<&str>) -> MockLlmProvider {
        MockLlmProvider::new(responses)
    }

    /// Create a test XaftRuntime with all mocks.
    pub fn test_runtime() -> TestRuntime {
        TestRuntimeBuilder::new()
            .with_workspace(test_workspace(&[
                ("src/main.rs", "fn main() {}"),
                ("Cargo.toml", "[package]\nname = \"test\""),
            ]))
            .with_provider(mock_provider(vec!["I'll fix the bug"]))
            .with_budget(1.0)
            .build()
    }
}
```

---

## Binary Size Optimization

| Technique | Estimated Savings | Impact |
|---|---|---|
| LTO (thin) | ~15% | Slower compile, no runtime impact |
| Strip symbols | ~30% | No debug symbols in release |
| `opt-level = "z"` | ~5% | Slower runtime, smaller binary |
| Feature gating | ~20-40% | Depends on features excluded |
| `panic = "abort"` | ~5% | No unwinding, smaller binary |
| `strip = true` | ~10% | No symbol table |

Target binary sizes:

| Configuration | Est. Size |
|---|---|
| Full (all features) | ~25MB |
| No TUI | ~18MB |
| No TUI, no index | ~14MB |
| Minimal (no tui, mcp, index, sse) | ~10MB |

---

## CI/CD Integration

### Build Matrix

```yaml
# .github/workflows/ci.yml (sketch)
matrix:
  os: [ubuntu-latest, macos-latest, windows-latest]
  rust: [stable, nightly]
  features:
    - default
    - --no-default-features --features mcp
    - --no-default-features

steps:
  - cargo check ${{ features }}
  - cargo test ${{ features }}
  - cargo clippy ${{ features }}
  - cargo fmt --check
```

### Release Process

```
1. Tag release: git tag v0.1.0
2. CI builds binaries for all platforms
3. Strip and compress binaries
4. Upload to GitHub Releases
5. Publish crates to crates.io (optional)
6. Update Homebrew tap (optional)
```
