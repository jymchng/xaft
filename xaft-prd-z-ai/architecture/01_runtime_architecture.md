# 01 — Runtime Architecture

> How xaft boots, initializes, and runs the main event loop.
> This document is implementation-oriented: a Rust engineer should be able to write code from it.

---

## Overview

The `XaftRuntime` is the top-level orchestrator that composes every agtrs primitive into a cohesive autonomous coding system. It is constructed once at CLI invocation, owns all shared state, and drives the agent execution loop until task completion or user cancellation.

The runtime does not implement business logic itself — it wires together the components that do. Its responsibilities are:

1. **Bootstrapping** from CLI arguments and configuration files
2. **Constructing** the provider chain, workspace, git context, and agent
3. **Driving** the `AgentExecutor::run_stream` loop
4. **Bridging** `StreamEvent`s to the appropriate output sink (TUI, headless JSON, SSE)
5. **Managing** session lifecycle, cost budgets, and cancellation

---

## Boot Sequence

The boot sequence transforms raw CLI arguments into a fully initialized runtime. Every step is fallible; errors produce user-friendly diagnostics and exit code 1.

```
CLI Invocation
     │
     ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 1: Parse CLI Args (xaft-cli)                          │
│   clap::Parser → XaftCli                                    │
│   Determine: command, prompt, flags, config path             │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 2: Load Configuration (xaft-config)                   │
│   Load order: CLI flags > env vars > .xaft.toml > defaults   │
│   Validate: provider keys, budget limits, tool permissions   │
│   Produce: XaftConfig (fully resolved)                       │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 3: Initialize Tracing (tracing-subscriber)            │
│   Configure: fmt layer, file layer, filter directives        │
│   Install: global subscriber, panic hook                     │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 4: Construct Provider Chain (agtrs-llm)               │
│   Primary: CostedProvider::new(primary_provider, tracker)    │
│   Fallbacks: FallbackProvider::chain([primary, ...fallbacks])│
│   Validate: API key reachability with lightweight ping       │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 5: Initialize Workspace (agtrs-workspace)             │
│   Detect: git root, .xaftignore, .gitignore                  │
│   Construct: OnDiskWorkspaceStore::new(root, ignore_patterns)│
│   Validate: write permissions, disk space threshold           │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 6: Initialize Git Context (agtrs-git)                 │
│   Open: GitRepo::open(workspace_root)                        │
│   Create: WorktreeGuard for branch isolation                 │
│   Branch: xaft/task-{timestamp}-{slug}                       │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 7: Initialize SignalBus (agtrs-signal)                │
│   Construct: SignalBus::new()                                │
│   Register: TUI subscriber, cost tracker, audit logger       │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 8: Register Tools (xaft-tools)                        │
│   Workspace: ReadFile, WriteFile, EditFile, ListFiles, Grep  │
│   Git: GitStatus, GitDiff, GitCommit, GitBranch              │
│   Shell: BashExec (with sandbox config)                      │
│   Search: SemanticSearch, SymbolSearch                       │
│   MCP: Load from config, register dynamically                │
│   Wrap: HookedTool::wrap(tool, hooks) for each               │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 9: Construct Agent (xaft-agent)                       │
│   Select: XaftAgent or PlanModeAgent based on config         │
│   Inject: system prompt, tools, memory, conversation store   │
│   Configure: max turns, guardrails, approval policy          │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 10: Construct XaftRuntime                             │
│   Assemble: all components into XaftRuntime struct           │
│   Validate: all invariants hold (budget > 0, tools > 0, ...) │
└─────────────────────────┬───────────────────────────────────┘
                          │
                          ▼
                   XaftRuntime::run()
```

---

## XaftRuntime Struct

```rust
/// Top-level runtime orchestrator. Owns all shared state and drives execution.
pub struct XaftRuntime {
    // ── Identity ──────────────────────────────────────────────
    /// Unique runtime identifier for tracing and logging.
    runtime_id: RuntimeId,

    // ── Configuration ─────────────────────────────────────────
    /// Fully resolved configuration (CLI > env > file > defaults).
    config: XaftConfig,

    // ── LLM Provider Chain ────────────────────────────────────
    /// Provider chain with cost tracking and fallback routing.
    /// Chain: CostedProvider(primary) → FallbackProvider([primary, fallback1, fallback2])
    provider: FallbackProvider<CostedProvider<OpenAiProvider>, AnthropicProvider>,

    // ── Agent Execution ───────────────────────────────────────
    /// The ReAct loop executor. Drives the agent turn-by-turn.
    executor: AgentExecutor,

    /// The agent instance (XaftAgent or PlanModeAgent).
    agent: Box<dyn Agent>,

    /// Planner for task decomposition before execution.
    planner: Box<dyn Planner>,

    // ── Workspace ─────────────────────────────────────────────
    /// File state management with transactional editing.
    workspace: Arc<dyn WorkspaceStore>,

    /// Git repository context with branch isolation.
    git_repo: GitRepo,

    /// Active worktree guard; dropped on shutdown to restore original branch.
    worktree_guard: Option<WorktreeGuard>,

    // ── Event System ──────────────────────────────────────────
    /// Central event bus for all runtime events.
    signal_bus: Arc<SignalBus>,

    // ── Session ───────────────────────────────────────────────
    /// Conversation history persistence.
    conversation_store: Arc<dyn ConversationStore>,

    /// Long-term memory across sessions.
    memory_store: Arc<dyn MemoryStore>,

    /// Current session state.
    session: AgentSession,

    // ── Cost Control ──────────────────────────────────────────
    /// Real-time cost tracker integrated with CostedProvider.
    cost_tracker: Arc<CostTracker>,

    /// Session budget limit in USD.
    session_budget: Budget,

    /// Daily budget limit in USD.
    daily_budget: Budget,

    // ── Cancellation ──────────────────────────────────────────
    /// Token for graceful cancellation (Ctrl+C, timeout, budget exhaustion).
    cancellation_token: CancellationToken,

    // ── Tool Registry ─────────────────────────────────────────
    /// All registered tools, type-erased for heterogeneous dispatch.
    tools: Vec<ErasedTool>,

    /// Approval gate policy.
    approval_policy: ApprovalPolicy,
}
```

### Key Methods

```rust
impl XaftRuntime {
    /// Bootstrap the runtime from CLI arguments.
    /// Executes Phases 1-10 of the boot sequence.
    pub async fn bootstrap(cli: XaftCli) -> Result<Self, XaftError> { ... }

    /// Run the main execution loop.
    /// Consumes self; runtime cannot be reused after run().
    pub async fn run(mut self) -> Result<ExitCode, XaftError> { ... }

    /// Run in headless mode (no TUI, structured JSON output).
    pub async fn run_headless(mut self) -> Result<ExitCode, XaftError> { ... }

    /// Run with SSE output for remote access.
    pub async fn run_sse(mut self, addr: SocketAddr) -> Result<ExitCode, XaftError> { ... }

    /// Resume a previously interrupted session.
    pub async fn resume(session_id: SessionId, cli: XaftCli) -> Result<Self, XaftError> { ... }

    /// Graceful shutdown: persist state, commit/rollback pending edits, restore git.
    pub async fn shutdown(mut self) -> Result<(), XaftError> { ... }
}
```

---

## Session Lifecycle

Sessions represent a single logical interaction with xaft, from initial prompt to final result. They persist across crashes via `ConversationStore`.

```
                    ┌──────────┐
                    │  Created │  XaftRuntime::bootstrap()
                    └────┬─────┘
                         │ load_or_create conversation
                         ▼
                    ┌──────────┐
          ┌────────│  Active   │────────┐
          │        └────┬─────┘        │
          │             │              │
     user pauses   agent running   budget exhausted
          │             │              │
          ▼             ▼              ▼
    ┌──────────┐  ┌──────────┐  ┌──────────┐
    │ Suspended│  │ Running  │  │ BudgetHit│
    └────┬─────┘  └────┬─────┘  └────┬─────┘
         │              │              │
    resume()       task complete   budget increased
         │              │              │
         └──────┬───────┘──────────────┘
                │
                ▼
           ┌──────────┐
           │Completed │  Final result delivered
           └──────────┘

     At any point, CancellationToken fires:
         Any State ──→ Cancelled ──→ shutdown()
```

### Session Persistence

```rust
pub struct AgentSession {
    id: SessionId,
    created_at: chrono::DateTime<chrono::Utc>,
    workspace_root: PathBuf,
    git_branch: Option<String>,
    total_cost_usd: f64,
    total_tokens: TokenCount,
    turn_count: u32,
    status: SessionStatus,
    /// Serialized conversation history for resume.
    conversation_snapshot: Option<ConversationSnapshot>,
}

pub enum SessionStatus {
    Active,
    Suspended,
    Completed { exit_code: ExitCode },
    Failed { error: String },
    Cancelled,
}
```

---

## Provider Routing

xaft uses a layered provider architecture for cost control and resilience:

```rust
/// Construction of the provider chain.
fn build_provider_chain(config: &XaftConfig) -> Result<impl LlmProvider, XaftError> {
    // Layer 1: Primary provider (e.g., OpenAI GPT-4o)
    let primary = OpenAiProvider::new(config.openai_api_key.clone(), config.primary_model.clone());

    // Layer 2: Wrap with cost tracking
    let costed_primary = CostedProvider::new(
        primary,
        config.pricing_for(&config.primary_model),  // per-token pricing table
        config.session_budget.clone(),
    );

    // Layer 3: Add fallback providers for resilience
    let fallback1 = AnthropicProvider::new(config.anthropic_api_key.clone(), "claude-3.5-sonnet");
    let fallback2 = OpenAiProvider::new(config.openai_api_key.clone(), "gpt-4o-mini");

    let chain = FallbackProvider::new(costed_primary)
        .with_fallback(fallback1, FallbackTrigger::RateLimit | FallbackTrigger::Timeout(30_000))
        .with_fallback(fallback2, FallbackTrigger::RateLimit | FallbackTrigger::CostThreshold(0.8));

    Ok(chain)
}
```

### Fallback Triggers

| Trigger | Condition | Action |
|---|---|---|
| `RateLimit` | HTTP 429 from provider | Switch to next provider in chain |
| `Timeout(ms)` | No response within ms | Switch to next provider in chain |
| `CostThreshold(ratio)` | Session cost exceeds `ratio * budget` | Switch to cheaper provider |
| `ModelError` | Provider returns structured error | Switch or retry depending on error class |
| `Manual` | User explicitly requests switch | Immediate switch, no retry |

### Cost Tracking Flow

```
LLM Request
    │
    ▼
CostedProvider::complete(request)
    │
    ├── Pre-check: session_budget.remaining() > estimated_cost?
    │   ├── Yes → proceed
    │   └── No  → emit(BudgetExhausted), return Err(BudgetExceeded)
    │
    ├── Execute: inner_provider.complete(request)
    │   ├── Ok(response) →
    │   │   ├── Calculate: input_tokens * input_price + output_tokens * output_price
    │   │   ├── Emit: SignalBus::emit(CostIncrement { amount, cumulative })
    │   │   └── Return response
    │   └── Err(e) →
    │       └── Emit: SignalBus::emit(ProviderError { provider, error })
    │
    └── Post-check: cumulative_cost > daily_budget?
        └── Yes → emit(DailyBudgetExhausted), cancel token
```

---

## Planner Selection

xaft selects a planner based on task complexity heuristics. The planner runs before the main agent loop to decompose the task into an execution plan.

```rust
fn select_planner(task: &str, config: &XaftConfig) -> Box<dyn Planner> {
    match config.planner {
        PlannerConfig::Auto => {
            // Heuristic: estimate complexity from prompt characteristics
            let complexity = estimate_complexity(task);
            match complexity {
                Complexity::Simple => Box::new(OneShotPlanner::new()),
                Complexity::Moderate => Box::new(IterativeRefinementPlanner::new(
                    IterativeRefinementConfig {
                        max_iterations: 3,
                        refinement_prompt: include_str!("../prompts/refine_plan.md"),
                    },
                )),
                Complexity::Complex => Box::new(TreeOfThoughtPlanner::new(
                    TreeOfThoughtConfig {
                        branching_factor: 3,
                        max_depth: 4,
                        evaluation_model: config.cheap_model.clone(),
                    },
                )),
            }
        }
        PlannerConfig::OneShot => Box::new(OneShotPlanner::new()),
        PlannerConfig::Iterative { max_iterations } => Box::new(
            IterativeRefinementPlanner::new(IterativeRefinementConfig {
                max_iterations,
                ..Default::default()
            })
        ),
        PlannerConfig::TreeOfThought { branching, depth } => Box::new(
            TreeOfThoughtPlanner::new(TreeOfThoughtConfig {
                branching_factor: branching,
                max_depth: depth,
                ..Default::default()
            })
        ),
    }
}

fn estimate_complexity(task: &str) -> Complexity {
    let indicators = [
        task.contains("refactor"),   // +2 complexity
        task.contains("migrate"),    // +3 complexity
        task.contains("multiple"),   // +1 complexity
        task.split_whitespace().count() > 50,  // +2 complexity
        task.contains("and then"),   // +1 complexity (multi-step)
        task.matches('\n').count() > 5,  // +1 complexity (structured prompt)
    ];

    let score: u32 = indicators.iter().map(|&b| if b { 1 } else { 0 }).sum();
    match score {
        0..=1 => Complexity::Simple,
        2..=3 => Complexity::Moderate,
        _ => Complexity::Complex,
    }
}
```

### Plan Output Format

Regardless of planner type, the output is a `TaskPlan`:

```rust
pub struct TaskPlan {
    /// Human-readable plan description.
    summary: String,

    /// Ordered list of execution steps.
    steps: Vec<PlanStep>,

    /// Estimated cost range for the plan.
    estimated_cost: Range<f64>,

    /// Estimated number of agent turns.
    estimated_turns: Range<u32>,

    /// Risk assessment: files that may be modified, commands that may be run.
    risk_assessment: RiskAssessment,
}

pub struct PlanStep {
    /// Step description shown to user.
    description: String,

    /// Tools expected to be used.
    expected_tools: Vec<String>,

    /// Files expected to be modified.
    expected_file_changes: Vec<PathBuf>,

    /// Whether this step requires user approval.
    requires_approval: bool,
}
```

---

## Team Mode Initialization

When multi-agent coordination is needed, xaft initializes `TeamMode` structures:

```rust
/// Initialize team mode if configured.
fn init_team_mode(
    config: &TeamConfig,
    provider: impl LlmProvider,
    tools: Vec<ErasedTool>,
    bus: Arc<SignalBus>,
) -> Result<Option<TeamMode>, XaftError> {
    match config.mode {
        TeamModeConfig::Single => Ok(None),

        TeamModeConfig::Coordinator { subagent_count } => {
            let coordinator = TeamMode::Coordinator {
                coordinator_agent: XaftAgent::new(
                    "coordinator",
                    COORDINATOR_SYSTEM_PROMPT,
                    tools.clone(),
                    provider.clone(),
                ),
                subagents: (0..subagent_count)
                    .map(|i| SubagentTool::new(
                        format!("subagent-{i}"),
                        XaftAgent::new(
                            &format!("subagent-{i}"),
                            SUBAGENT_SYSTEM_PROMPT,
                            tools.clone(),
                            provider.clone(),
                        ),
                        bus.clone(),
                    ))
                    .collect(),
                message_bus: AgentMessageBus::new(subagent_count),
            };
            Ok(Some(coordinator))
        }

        TeamModeConfig::Collaborate { agent_count } => {
            let collaborate = TeamMode::Collaborate {
                agents: (0..agent_count)
                    .map(|i| XaftAgent::new(
                        &format!("collaborator-{i}"),
                        COLLABORATOR_SYSTEM_PROMPT,
                        tools.clone(),
                        provider.clone(),
                    ))
                    .collect(),
                message_bus: AgentMessageBus::new(agent_count),
                shared_scratchpad: Scratchpad::new(),
            };
            Ok(Some(collaborate))
        },
    }
}
```

---

## Main Event Loop

The core execution loop consumes `StreamEvent`s from `AgentExecutor::run_stream` and dispatches them to the output sink.

```rust
impl XaftRuntime {
    pub async fn run(mut self) -> Result<ExitCode, XaftError> {
        // ── Pre-flight checks ──────────────────────────────
        self.preflight_checks()?;

        // ── Execute plan if planner is configured ──────────
        let plan = if self.config.planning_enabled {
            let plan = self.planner.plan(&self.session.prompt, &*self.provider).await?;
            self.signal_bus.emit(Signal::PlanCreated(plan.clone()))?;

            // If plan requires approval, gate here
            if plan.requires_approval() && !self.config.auto_approve_plans {
                let approved = self.output_sink.request_plan_approval(&plan).await?;
                if !approved {
                    self.signal_bus.emit(Signal::PlanRejected)?;
                    return Ok(ExitCode::PLAN_REJECTED);
                }
            }
            Some(plan)
        } else {
            None
        };

        // ── Inject plan into agent context ─────────────────
        if let Some(ref plan) = plan {
            self.agent.inject_context(serde_json::to_string(plan)?)?;
        }

        // ── Main streaming loop ────────────────────────────
        let mut stream = self.executor.run_stream(
            &*self.agent,
            &self.session.prompt,
            &*self.provider,
            self.cancellation_token.clone(),
        );

        // ── Consume stream events ──────────────────────────
        let mut final_result = None;
        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::LlmToken { token, .. } => {
                    self.output_sink.render_token(&token)?;
                    self.signal_bus.emit(Signal::StreamToken(token))?;
                }

                StreamEvent::ToolCall { tool_name, args, call_id } => {
                    self.signal_bus.emit(Signal::ToolExecuting {
                        tool: tool_name.clone(),
                        args: args.clone(),
                    })?;

                    // Check approval gate
                    if self.approval_policy.requires_approval(&tool_name) {
                        let approved = self.output_sink
                            .request_tool_approval(&tool_name, &args).await?;
                        if !approved {
                            self.signal_bus.emit(Signal::ToolRejected { call_id })?;
                            // Inject rejection into agent context
                            self.agent.inject_tool_result(call_id, "User rejected this tool call")?;
                            continue;
                        }
                    }

                    self.output_sink.render_tool_start(&tool_name, &args)?;
                }

                StreamEvent::ToolResult { call_id, result } => {
                    self.signal_bus.emit(Signal::ToolResult {
                        call_id: call_id.clone(),
                        success: result.is_ok(),
                    })?;
                    self.output_sink.render_tool_result(&call_id, &result)?;
                }

                StreamEvent::TurnComplete { turn, cost } => {
                    self.cost_tracker.record(cost)?;
                    self.session.turn_count += 1;
                    self.signal_bus.emit(Signal::TurnComplete {
                        turn,
                        cumulative_cost: self.cost_tracker.total(),
                    })?;

                    // Budget check after every turn
                    if self.cost_tracker.total() > self.session_budget.remaining() {
                        self.signal_bus.emit(Signal::BudgetExhausted)?;
                        self.cancellation_token.cancel();
                    }
                }

                StreamEvent::AgentComplete { result } => {
                    final_result = Some(result);
                    break;
                }

                StreamEvent::Error { error } => {
                    self.signal_bus.emit(Signal::AgentError { error: error.clone() })?;
                    return Err(XaftError::AgentExecution(error));
                }
            }
        }

        // ── Post-execution ─────────────────────────────────
        if let Some(result) = final_result {
            self.signal_bus.emit(Signal::TaskComplete { result: result.clone() })?;
        }

        // ── Git commit if workspace has changes ────────────
        if self.workspace.has_uncommitted_changes()? {
            let msg = format!("xaft: {}", self.session.prompt.chars().take(72).collect::<String>());
            self.worktree_guard.as_mut().unwrap().commit_all(&msg)?;
            self.signal_bus.emit(Signal::AutoCommit { message: msg })?;
        }

        // ── Persist session ────────────────────────────────
        self.session.status = SessionStatus::Completed { exit_code: ExitCode::SUCCESS };
        self.conversation_store.persist(&self.session).await?;

        // ── Shutdown ───────────────────────────────────────
        self.shutdown().await?;

        Ok(ExitCode::SUCCESS)
    }
}
```

---

## Full Runtime Pipeline Diagram

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                              xaft run pipeline                               │
│                                                                              │
│  ┌────────┐    ┌─────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐  │
│  │  CLI   │───▶│ Config  │───▶│ Provider │───▶│Workspace │───▶│  Signal  │  │
│  │  Args  │    │ Loader  │    │  Chain   │    │  Store   │    │   Bus    │  │
│  └────────┘    └─────────┘    └──────────┘    └──────────┘    └────┬─────┘  │
│                                                                     │       │
│  ┌────────┐    ┌─────────┐    ┌──────────┐    ┌──────────┐         │       │
│  │  Git   │───▶│  Tool   │───▶│  Agent   │───▶│ Executor │◀────────┤       │
│  │  Repo  │    │Registry │    │ Instance │    │ (ReAct)  │         │       │
│  └────────┘    └─────────┘    └──────────┘    └────┬─────┘         │       │
│                                                    │               │       │
│                                                    ▼               ▼       │
│                                             ┌─────────────────────────┐   │
│                                             │    StreamEvent Channel  │   │
│                                             └───────────┬─────────────┘   │
│                                                         │                 │
│                              ┌──────────────────────────┼──────────────┐  │
│                              │                          │              │  │
│                              ▼                          ▼              ▼  │
│                       ┌──────────┐             ┌──────────────┐ ┌───────┐ │
│                       │   TUI    │             │  Headless    │ │  SSE  │ │
│                       │ (ratatui)│             │  JSON output │ │ (Axum)│ │
│                       └──────────┘             └──────────────┘ └───────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Error Handling Strategy

xaft uses a layered error handling approach:

```rust
/// Top-level error type for the runtime.
#[derive(Debug, thiserror::Error)]
pub enum XaftError {
    // ── Boot errors ──
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Provider initialization failed: {0}")]
    ProviderInit(String),

    #[error("Workspace initialization failed: {0}")]
    WorkspaceInit(#[from] WorkspaceError),

    #[error("Git initialization failed: {0}")]
    GitInit(#[from] GitError),

    // ── Runtime errors ──
    #[error("Agent execution error: {0}")]
    AgentExecution(String),

    #[error("Budget exceeded: spent ${spent:.4} of ${budget:.4}")]
    BudgetExceeded { spent: f64, budget: f64 },

    #[error("Tool execution error: {tool} - {error}")]
    ToolExecution { tool: String, error: String },

    #[error("Approval rejected for tool: {0}")]
    ApprovalRejected(String),

    // ── Cancellation ──
    #[error("Operation cancelled")]
    Cancelled,

    // ── Persistence errors ──
    #[error("Session persistence error: {0}")]
    SessionPersistence(String),
}
```

### Panic Boundaries

Each subsystem is isolated behind a panic boundary using `std::panic::catch_unwind`:

```rust
fn execute_tool_safely(tool: &ErasedTool, args: &str, ctx: &ToolContext) -> Result<ToolOutput, XaftError> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        tool.execute(args, ctx)
    }))
    .map_err(|_| XaftError::ToolExecution {
        tool: tool.name().to_string(),
        error: "tool panicked".to_string(),
    })?
}
```

---

## Shutdown Sequence

```
Shutdown triggered by:
  ├── CancellationToken (Ctrl+C, budget, timeout)
  ├── Agent completion (natural)
  └── Fatal error

                    ┌──────────────┐
                    │Shutdown Init │
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │Cancel Token  │  Propagates to all subsystems
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │Drain     │ │Rollback  │ │Abort     │
        │In-flight │ │Uncommited│ │Git       │
        │LLM Calls │ │FileEdits │ │Operations│
        └────┬─────┘ └────┬─────┘ └────┬─────┘
             │             │            │
             └──────┬──────┘────────────┘
                    │
             ┌──────▼──────┐
             │Persist      │  Save session, conversation, memory
             │Session      │
             └──────┬──────┘
                    │
             ┌──────▼──────┐
             │Emit         │  Signal::SessionComplete
             │Final Signal │
             └──────┬──────┘
                    │
             ┌──────▼──────┐
             │Drop         │  Drop WorktreeGuard (restores branch if needed)
             │Resources    │  Close file handles, HTTP connections
             └─────────────┘
```

---

## Runtime Configuration Reference

```toml
# .xaft.toml — default runtime configuration

[provider]
primary = "openai"
primary_model = "gpt-4o"
fallback_models = ["claude-3.5-sonnet", "gpt-4o-mini"]

[budget]
session_limit_usd = 5.00
daily_limit_usd = 25.00
warn_at_percent = 80

[workspace]
store = "ondisk"           # "ondisk" | "inmemory"
auto_commit = true
commit_message_prefix = "xaft:"

[git]
branch_per_task = true
branch_prefix = "xaft/task-"
auto_push = false
worktree_isolation = true

[planner]
mode = "auto"               # "auto" | "oneshot" | "iterative" | "tree-of-thought"
iterative_max_refinements = 3
tot_branching_factor = 3
tot_max_depth = 4

[approval]
default_policy = "confirm"  # "confirm" | "auto-approve" | "deny"
shell = "confirm"
file_write = "confirm"
file_edit = "confirm"
git_commit = "auto-approve"
git_push = "confirm"
search = "auto-approve"

[streaming]
token_by_token = true
tool_progress = true
backpressure_buffer_size = 1024

[team]
mode = "single"             # "single" | "coordinator" | "collaborate"
subagent_count = 3          # for coordinator mode

[memory]
conversation_store = "sqlite"
memory_store = "sqlite"
max_conversation_turns = 100

[logging]
level = "info"
file = ".xaft/logs/session.log"
```
