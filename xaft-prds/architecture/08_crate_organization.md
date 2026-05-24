# Crate Organization

## Workspace Structure

```toml
# xaft/Cargo.toml
[workspace]
members = [
    "xaft",
    "xaft-core",
    "xaft-orchestrator",
    "xaft-agents",
    "xaft-tools",
    "xaft-tui",
    "xaft-index",
    "xaft-plugin",
    "xaft-server",
    "xaft-test",
]
resolver = "2"

[workspace.dependencies]
# agtrs ecosystem
agtrs          = { path = "../agtrs/agtrs" }
agtrs-runtime  = { path = "../agtrs/agtrs-runtime" }
agtrs-macros   = { path = "../agtrs/agtrs-macros" }
agtrs-graph    = { path = "../agtrs/agtrs-graph" }
agtrs-shell    = { path = "../agtrs/agtrs-shell" }
agtrs-git      = { path = "../agtrs/agtrs-git" }
agtrs-workspace= { path = "../agtrs/agtrs-workspace" }
agtrs-store    = { path = "../agtrs/agtrs-store" }
agtrs-anthropic= { path = "../agtrs/agtrs-anthropic" }
agtrs-gemini   = { path = "../agtrs/agtrs-gemini" }
injectable     = { path = "../injectable/injectable" }

# Async
tokio          = { version = "1", features = ["full"] }
async-trait    = "0.1"
futures        = "0.3"

# Serialization
serde          = { version = "1", features = ["derive"] }
serde_json     = "1"
toml           = "0.8"

# TUI
ratatui        = "0.29"
crossterm      = { version = "0.28", features = ["event-stream"] }

# CLI
clap           = { version = "4", features = ["derive", "env"] }

# Error handling
thiserror      = "1"
anyhow         = "1"

# Observability
tracing        = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# Utilities
uuid           = { version = "1", features = ["v4"] }
chrono         = { version = "0.4", features = ["serde"] }
```

## Crate Responsibilities

### `xaft` (binary crate)

Entry point only. Minimal code.

```rust
// xaft/src/main.rs
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = xaft_core::dispatch(cli).await {
        eprintln!("error: {e}");
        std::process::exit(e.exit_code());
    }
}
```

**Depends on**: `xaft-core`, `clap`, `tokio`

### `xaft-core`

Core types, error definitions, configuration loading, and top-level dispatch.

```
xaft-core/src/
├── lib.rs           — dispatch(), init functions
├── config.rs        — XaftConfig, config loading + merging
├── error.rs         — XaftError enum
├── session.rs       — XaftSession construction
└── cost.rs          — CostTracker (wraps PricingTable)
```

**Depends on**: `agtrs-runtime`, `agtrs-git`, `agtrs-workspace`, `agtrs-store`, `injectable`

### `xaft-orchestrator`

SessionManager, WorkflowEngine, PlanExecutor. The "brain" that coordinates agents.

```
xaft-orchestrator/src/
├── lib.rs
├── session_manager.rs  — XaftSession lifecycle
├── plan_executor.rs    — PlanExecutor: execute plan steps
├── workflow_engine.rs  — WorkflowEngine: compose multi-agent workflows
├── conflict.rs         — File-level conflict detection for parallel execution
└── recovery.rs         — Failure recovery: retry, replan, checkpoint restore
```

**Depends on**: `xaft-core`, `xaft-agents`, `agtrs-runtime`

### `xaft-agents`

All agent implementations.

```
xaft-agents/src/
├── lib.rs
├── code_agent.rs     — CodeAgent: implements plan steps
├── planner_agent.rs  — PlannerAgent: decomposes intent
├── review_agent.rs   — ReviewAgent: reviews diffs
├── fixer_agent.rs    — FixerAgent: fixes test/compile failures
├── index_agent.rs    — IndexAgent: builds/queries semantic index
└── summary_agent.rs  — SummaryAgent: condenses conversation context
```

**Depends on**: `xaft-core`, `xaft-tools`, `agtrs-runtime`, `agtrs-macros`

### `xaft-tools`

All tool implementations.

```
xaft-tools/src/
├── lib.rs
├── fs/
│   ├── read_file.rs
│   ├── write_file.rs
│   ├── list_files.rs
│   ├── search_files.rs
│   └── apply_patch.rs
├── shell/
│   ├── run_cargo.rs
│   ├── run_command.rs
│   └── run_tests.rs
├── git/
│   ├── git_status.rs
│   ├── git_diff.rs
│   ├── git_commit.rs
│   └── git_log.rs
├── index/
│   ├── search_code.rs
│   ├── find_symbol.rs
│   └── get_dependencies.rs
└── meta/
    ├── checkpoint_tool.rs
    ├── approval_tool.rs
    └── replan_tool.rs
```

**Depends on**: `xaft-core`, `agtrs-runtime`, `agtrs-shell`, `agtrs-git`, `agtrs-workspace`

### `xaft-tui`

Ratatui TUI application.

```
xaft-tui/src/
├── lib.rs
├── app.rs           — TuiApp: root application struct + event loop
├── state.rs         — AppState definition
├── events.rs        — UiEvent enum + mapping from StreamEvent/signals
├── render.rs        — Top-level render() dispatch
├── panes/
│   ├── agent_output.rs    — Scrollable streaming text pane
│   ├── plan_tree.rs       — Plan step tree with status indicators
│   ├── diff_viewer.rs     — Syntax-highlighted unified diff
│   ├── shell_console.rs   — Shell command output stream
│   ├── cost_dashboard.rs  — Token/cost gauges and sparklines
│   └── log_console.rs     — Timestamped log line viewer
├── widgets/
│   ├── approval_dialog.rs — Modal approval dialog
│   ├── status_bar.rs      — Bottom status bar
│   └── progress_bar.rs    — Animated progress indicator
└── keyboard.rs            — Keyboard binding dispatch
```

**Depends on**: `xaft-core`, `ratatui`, `crossterm`, `agtrs-runtime`

### `xaft-index`

Repository semantic indexing using tree-sitter.

```
xaft-index/src/
├── lib.rs
├── builder.rs       — Index build orchestration
├── watcher.rs       — Incremental reindex on file change
├── symbols.rs       — Tree-sitter symbol extraction
├── import_graph.rs  — Dependency graph construction
├── search.rs        — Fuzzy + semantic search
└── languages/
    ├── rust.rs
    ├── typescript.rs
    └── python.rs
```

**Depends on**: `xaft-core`, `agtrs-workspace`, `agtrs-runtime`, `tree-sitter`, `tree-sitter-rust`

### `xaft-plugin`

Plugin trait and registry. Enables third-party tools via native Rust or MCP.

```
xaft-plugin/src/
├── lib.rs
├── plugin.rs        — XaftPlugin trait
├── registry.rs      — PluginRegistry: load, register, list
├── mcp_bridge.rs    — MCP → Tool adapter
└── native.rs        — Native Rust plugin loader (dylib)
```

**Depends on**: `xaft-core`, `agtrs-runtime`

### `xaft-server`

Optional Axum HTTP server for remote agent access.

```
xaft-server/src/
├── lib.rs
├── server.rs        — Axum router, startup
├── routes/
│   ├── run.rs       — POST /run → SSE stream
│   ├── status.rs    — GET /sessions/{id}
│   ├── approve.rs   — POST /sessions/{id}/approve
│   └── cancel.rs    — POST /sessions/{id}/cancel
├── auth.rs          — API key validation middleware
└── models.rs        — Request/response types
```

**Depends on**: `xaft-core`, `xaft-orchestrator`, `axum`, `agtrs-runtime`

### `xaft-test`

Integration test harness.

```
xaft-test/src/
├── lib.rs
├── harness.rs       — TestHarness: temp dir, mock git, mock providers
├── fixtures/        — Pre-built test repository fixtures
└── scenarios/
    ├── simple_edit.rs
    ├── multi_file_refactor.rs
    └── fixer_loop.rs
```

**Depends on**: all `xaft-*` crates, `tempfile`, `agtrs-runtime::testing`

## Dependency Graph

```
xaft (binary)
    └── xaft-core
            ├── xaft-orchestrator
            │       ├── xaft-agents
            │       │       └── xaft-tools
            │       └── xaft-index
            ├── xaft-tui
            ├── xaft-plugin
            └── xaft-server (optional feature)

All xaft-* depend on:
    agtrs / agtrs-runtime / agtrs-macros
    agtrs-shell / agtrs-git / agtrs-workspace / agtrs-store
    agtrs-anthropic / agtrs-gemini
    injectable / injectable-runtime
```

## Feature Flags

```toml
# xaft/Cargo.toml
[features]
default = ["anthropic", "tui"]
anthropic = ["agtrs-anthropic"]
gemini = ["agtrs-gemini"]
ollama = ["xaft-providers/ollama"]
tui = ["xaft-tui"]
server = ["xaft-server"]
mcp = ["xaft-plugin/mcp"]
distributed = ["xaft-server/distributed"]
```

## References

- agtrs: `Cargo.toml` (workspace structure reference)
- Next: see [TUI Architecture →](../tui/01_tui_architecture.md)