# Runtime Architecture

## Overview

`xaft` is built as a layered system on top of the `agtrs` workspace. The runtime architecture follows a strict dependency direction from low-level async primitives up to high-level orchestration.

```
┌─────────────────────────────────────────────────────────────────┐
│                      xaft CLI / TUI Layer                       │
│           clap argument parsing + ratatui + crossterm           │
├─────────────────────────────────────────────────────────────────┤
│                    xaft-orchestrator                             │
│        SessionManager · WorkflowEngine · PlanExecutor           │
├──────────────────────┬──────────────────────────────────────────┤
│   xaft-agents        │   xaft-tools                             │
│   CodeAgent          │   ReadFileTool · WriteFileTool           │
│   PlannerAgent       │   ShellTool · GitTool · SearchTool       │
│   ReviewAgent        │   PatchTool · IndexTool · BrowseTool     │
│   FixerAgent         │   ApprovalTool · CheckpointTool          │
├──────────────────────┴──────────────────────────────────────────┤
│                       agtrs-runtime                              │
│  AgentExecutor · AgentContext · TaskRunner · SignalBus           │
│  SubagentTool · HandoffOrchestrator · StructuredLlm              │
│  ShortTermMemory · ConversationStore · InMemoryVectorStore       │
│  PlannersA: OneShotPlanner · IterativePlanner · TreeOfThought   │
├──────────────────────────────────────────────────────────────────┤
│              agtrs-{shell, git, workspace, store}                │
│  ShellExecutor/ShellPolicy · GitRepo/WorktreeManager            │
│  WorkspaceEditor · DiffApplier · FuzzySearch · SqliteStore      │
├──────────────────────────────────────────────────────────────────┤
│              agtrs-{anthropic, gemini, local}                    │
│         AnthropicProvider · GeminiProvider · OllamaProvider     │
├──────────────────────────────────────────────────────────────────┤
│         Tokio full async runtime · injectable DI container       │
└──────────────────────────────────────────────────────────────────┘
```

## Proposed Crate Structure

```
xaft/                          ← workspace root
├── xaft/                      ← CLI binary (main.rs, clap, startup)
├── xaft-core/                 ← Core types: XaftConfig, XaftSession, XaftError
├── xaft-orchestrator/         ← SessionManager, WorkflowEngine, PlanExecutor
├── xaft-agents/               ← CodeAgent, PlannerAgent, ReviewAgent, FixerAgent
├── xaft-tools/                ← xaft-specific tool implementations
├── xaft-tui/                  ← Ratatui TUI: panes, widgets, event loop
├── xaft-index/                ← Repository semantic indexing (tree-sitter, embeddings)
├── xaft-plugin/               ← Plugin trait, registry, MCP bridge
├── xaft-server/               ← Axum HTTP/SSE remote agent server
└── xaft-test/                 ← Integration test harness with mock workspace
```

**Dependency rule:** All `xaft-*` crates may depend on `agtrs-*` crates. `agtrs-*` crates have zero dependency on `xaft-*` crates.

## XaftSession — Central State Object

`XaftSession` is the per-invocation root. Created once, passed as `Arc` everywhere.

```rust
pub struct XaftSession {
    pub session_id: Uuid,
    pub project_root: PathBuf,
    pub git_repo: Arc<GitRepo>,
    /// Active worktree for agent edits (None = no active task)
    pub active_worktree: Arc<RwLock<Option<GitWorktree>>>,
    pub workspace: Arc<WorkspaceEditor>,
    pub task_runner: Arc<TaskRunner>,
    pub signal_bus: Arc<SignalBus>,
    pub message_bus: Arc<AgentMessageBus>,
    pub config: Arc<XaftConfig>,
    pub cost_tracker: Arc<CostTracker>,
    pub session_store: Arc<dyn SessionStore>,
    pub resolve_ctx: Arc<ResolveContext>,
    pub root_cancel: CancellationToken,
    pub started_at: DateTime<Utc>,
}
```

## Startup Sequence

```
$ xaft run "migrate auth module to JWT"

1. clap parses args → RunArgs { goal, flags, overrides }
2. XaftConfig::load() — merges ~/.config/xaft, .xaft/, env, flags
3. init_tracing(config.log_format)
4. DI Container::builder()
   .register("", DynProvider::new(|| async { AnthropicProvider::from_env() }))
   .register("cheap", DynProvider::new(|| async { GeminiFlashProvider::from_env() }))
   .build().await?
5. XaftSession::new(container, config).await?
6. TUI setup: enable_raw_mode(), EnterAlternateScreen
7. Spawn TUI render loop task
8. Intent::from_goal(args.goal).constraints(...).build()
9. SessionManager::run(session, intent).await
   ↳ PlannerAgent::plan(intent, available_tools)
   ↳ TaskRunner::submit(intent, &agent_ctx)
   ↳ PlanExecutor::execute(plan, session)
      ↳ Per step: AgentExecutor::run_stream(agent, input, ctx)
      ↳ StreamEvents → mpsc::Sender<UiEvent>
10. Completion: render summary pane
11. Restore terminal, exit
```

## Cancellation Architecture

```rust
// Root cancellation token — fires on Ctrl-C or fatal error
let root_token = CancellationToken::new();

// Graceful shutdown on Ctrl-C
tokio::spawn({
    let rt = root_token.clone();
    let session = Arc::clone(&session);
    async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Ctrl-C received — initiating graceful shutdown");
        rt.cancel();
        // Cleanup: suspend task, remove worktree, save checkpoint
        if let Some(wt) = session.active_worktree.read().await.as_ref() {
            session.task_runner.suspend_current("user cancelled").await.ok();
            session.git_repo.remove_worktree(wt).await.ok();
        }
    }
});

// All subtasks receive child tokens
let task_token = root_token.child_token();
AgentExecutor::run(agent, input, &mut ctx_with_token(task_token)).await?;
```

## Error Propagation

```rust
#[derive(Debug, thiserror::Error)]
pub enum XaftError {
    #[error("agent runtime error: {0}")]
    Agtrs(#[from] AgtrsError),

    #[error("git error: {0}")]
    Git(#[from] agtrs_git::GitError),

    #[error("shell error: {0}")]
    Shell(#[from] agtrs_shell::ShellError),

    #[error("workspace error: {0}")]
    Workspace(#[from] agtrs_workspace::WorkspaceError),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("plugin error: {0}")]
    Plugin(String),

    #[error("cancelled: {reason}")]
    Cancelled { reason: String },
}
```

The top-level `main()` maps `XaftError` to exit codes:
- `Ok` → 0
- `Cancelled` → 130 (conventional Ctrl-C exit)
- `Agtrs(BudgetExceeded)` → 1
- All others → 1 + TUI error pane display

## Agent Configuration Resolution Order

```
Priority (highest wins):
5. CLI flags:    --model claude-3-5-sonnet --budget 2.00 --max-turns 30
4. Env vars:     XAFT_MODEL, XAFT_BUDGET_USD, XAFT_MAX_TURNS
3. Project:      .xaft/config.toml [agents.code]
2. User:         ~/.config/xaft/config.toml [agent_defaults]
1. Framework:    AgentConfig::default()
```

## Process Architecture

`xaft` is a single process with multiple Tokio tasks:

```
main process
├── task: TUI render loop (30fps)
├── task: Crossterm keyboard event reader
├── task: SessionManager / PlanExecutor
│   ├── task: Agent execution (ReAct loop)
│   │   └── task: Individual tool calls (parallel if enabled)
│   └── task: Subagent instances (isolated contexts)
├── task: SignalBus broadcast consumers
│   ├── task: TUI event forwarder
│   ├── task: Metrics emitter
│   └── task: Audit log writer
└── task: Ctrl-C handler
```

All tasks share the root `CancellationToken`. Tokio's `JoinSet` tracks all tasks and ensures clean shutdown.

## Subsystem Interaction Diagram

```
User Input (CLI args or TUI keybind)
        │
        ▼
┌──────────────┐   intent    ┌───────────────┐   plan    ┌─────────────┐
│ SessionMgr   │────────────►│ PlannerAgent  │──────────►│ TaskRunner  │
└──────┬───────┘             └───────────────┘           └──────┬──────┘
       │                                                         │ step N
       │ StreamEvent (mpsc)                                      ▼
       │                                             ┌───────────────────┐
       ▼                                             │  Agent Executor   │
┌──────────────┐                                    │  ReAct loop       │
│  TUI Render  │◄────────────────────────────────── │  tool calls       │
│  Loop        │   UiEvent::AgentStream              └────────┬──────────┘
└──────────────┘                                              │
                                                    ┌─────────▼──────────┐
                                                    │   Tool Registry    │
                                                    │   read_file        │
                                                    │   write_file       │
                                                    │   shell_exec       │
                                                    │   git_commit       │
                                                    └────────────────────┘
```

## References

- agtrs: `agtrs-runtime/src/executor.rs`
- agtrs: `agtrs-runtime/src/task.rs`
- agtrs: `agtrs-runtime/src/signals.rs`
- Next: [Agent Lifecycle →](02_agent_lifecycle.md)