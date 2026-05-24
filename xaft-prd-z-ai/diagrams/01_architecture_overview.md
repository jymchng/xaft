# Architecture Overview

## 1. Master Architecture Diagram

```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│                                     xaft SYSTEM ARCHITECTURE                                  │
│                                                                                                │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                                    CLI / TUI Layer                                       │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │  │
│  │  │  xaft run    │  │  xaft ci     │  │  xaft review │  │  xaft config │              │  │
│  │  │  <prompt>    │  │  --review    │  │  <diff>      │  │  --set key=v │              │  │
│  │  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘              │  │
│  │         │                  │                  │                  │                       │  │
│  │         ▼                  ▼                  ▼                  ▼                       │  │
│  │  ┌──────────────────────────────────────────────────────────────────────────────────┐  │  │
│  │  │                            Argument Parser (clap)                                 │  │  │
│  │  │  xaft run | ci | review | config | auth | history | update | plugin | agent      │  │  │
│  │  └────────────────────────────────┬─────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────┼────────────────────────────────────────────────────┘  │
│                                      │                                                         │
│                                      ▼                                                         │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                              Session Manager                                             │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │  │
│  │  │  Config      │  │  Credential  │  │  History     │  │  Workspace   │              │  │
│  │  │  Resolver    │  │  Store       │  │  Manager     │  │  Detector    │              │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘              │  │
│  └───────────────────────────────────┬────────────────────────────────────────────────────┘  │
│                                      │                                                         │
│                                      ▼                                                         │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                           Execution Engine                                               │  │
│  │                                                                                          │  │
│  │  ┌──────────────────────────────────────────────────────────────────────────────────┐  │  │
│  │  │                         AgentExecutor                                              │  │  │
│  │  │                                                                                    │  │  │
│  │  │   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐      │  │  │
│  │  │   │  Prompt   │   │  LLM     │   │  Tool    │   │  Result  │   │  Loop    │      │  │  │
│  │  │   │  Builder  │──►│  Client  │──►│  Router  │──►│  Evaluator│──►│  Control │      │  │  │
│  │  │   │           │   │          │   │          │   │          │   │          │      │  │  │
│  │  │   │ system    │   │ request  │   │ dispatch │   │ verify   │   │ continue │      │  │  │
│  │  │   │ user      │   │ response │   │ execute  │   │ validate │   │ stop     │      │  │  │
│  │  │   │ tools     │   │ stream   │   │          │   │ score    │   │ retry    │      │  │  │
│  │  │   └──────────┘   └──────────┘   └─────┬────┘   └──────────┘   └──────────┘      │  │  │
│  │  │                                                       ▲                          │  │  │
│  │  │   ┌──────────┐   ┌──────────┐                        │                          │  │  │
│  │  │   │  Plan     │   │  State   │◄───────────────────────┘                          │  │  │
│  │  │   │  Manager  │   │  Machine │                                                   │  │  │
│  │  │   │           │   │          │   ┌──────────┐   ┌──────────┐                     │  │  │
│  │  │   │ create   │   │ plan     │   │  Budget  │   │  Delegation│                     │  │  │
│  │  │   │ steps    │   │ execute  │   │  Tracker │   │  Manager  │                     │  │  │
│  │  │   │ modify   │   │ validate │   │          │   │           │                     │  │  │
│  │  │   │ complete │   │ fail     │   │ enforce  │   │ delegate  │                     │  │  │
│  │  │   └──────────┘   └──────────┘   │ rollback │   │ collect   │                     │  │  │
│  │  │                                  └──────────┘   └──────────┘                     │  │  │
│  │  └──────────────────────────────────────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────┬────────────────────────────────────────────────────┘  │
│                                      │                                                         │
│          ┌───────────────────────────┼───────────────────────────┐                            │
│          │                           │                           │                            │
│          ▼                           ▼                           ▼                            │
│  ┌───────────────┐          ┌───────────────┐          ┌───────────────┐                     │
│  │  LLM Layer    │          │  Tool Layer    │          │  Workspace    │                     │
│  │               │          │               │          │  Layer        │                     │
│  │ ┌───────────┐│          │ ┌───────────┐ │          │ ┌───────────┐ │                     │
│  │ │Transport  ││          │ │FileEditor │ │          │ │Workspace  │ │                     │
│  │ │Trait      ││          │ │           │ │          │ │Store      │ │                     │
│  │ ├───────────┤│          │ │read       │ │          │ │Trait      │ │                     │
│  │ │Anthropic  ││          │ │write      │ │          │ ├───────────┤ │                     │
│  │ │Transport  ││          │ │delete     │ │          │ │FileSystem │ │                     │
│  │ ├───────────┤│          │ │search/rep │ │          │ │Store      │ │                     │
│  │ │OpenAI     ││          │ ├───────────┤ │          │ ├───────────┤ │                     │
│  │ │Transport  ││          │ │ShellExec  │ │          │ │InMemory   │ │                     │
│  │ ├───────────┤│          │ │           │ │          │ │Store      │ │                     │
│  │ │Google     ││          │ │run cmd    │ │          │ ├───────────┤ │                     │
│  │ │Transport  ││          │ │timeout    │ │          │ │Transactional│                     │
│  │ ├───────────┤│          │ ├───────────┤ │          │ │Workspace  │ │                     │
│  │ │Mock       ││          │ │GitOps     │ │          │ │(begin/    │ │                     │
│  │ │Transport  ││          │ │           │ │          │ │ commit/   │ │                     │
│  │ ├───────────┤│          │ │commit     │ │          │ │ rollback) │ │                     │
│  │ │Recorded   ││          │ │branch     │ │          │ └───────────┘ │                     │
│  │ │Transport  ││          │ │diff       │ │          │               │                     │
│  │ └───────────┘│          │ │merge      │ │          │ ┌───────────┐ │                     │
│  │               │          │ ├───────────┤ │          │ │FileWatcher│ │                     │
│  │ ┌───────────┐│          │ │WASM Plugin│ │          │ │(notify)   │ │                     │
│  │ │LLM Client ││          │ │Tools      │ │          │ └───────────┘ │                     │
│  │ │           ││          │ │(extensible│ │          │               │                     │
│  │ │rate limit ││          │ │ via WASM) │ │          │ ┌───────────┐ │                     │
│  │ │retry      ││          │ └───────────┘ │          │ │GitRepo    │ │                     │
│  │ │streaming  ││          │               │          │ │(libgit2)  │ │                     │
│  │ │caching    ││          │ ┌───────────┐ │          │ └───────────┘ │                     │
│  │ └───────────┘│          │ │Tool       │ │          └───────────────┘                     │
│  └───────────────┘          │ │Registry   │ │                                                │
│                              │ │(dispatch,│ │                                                │
│                              │ │ validate)│ │                                                │
│                              │ └───────────┘ │                                                │
│                              └───────────────┘                                                │
│                                                                                                │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                             Observability Layer                                          │  │
│  │                                                                                          │  │
│  │  ┌──────────────────────────────────────────────────────────────────────────────────┐  │  │
│  │  │                              SignalBus (broadcast)                                 │  │  │
│  │  │                                                                                    │  │  │
│  │  │   emit() ◄── Agent ◄── Tools ◄── LLM ◄── Workspace ◄── Git ◄── Budget           │  │  │
│  │  │                                                                                    │  │  │
│  │  │   subscribe() ──► TUI Dashboard                                                   │  │  │
│  │  │   subscribe() ──► Debug Logger                                                    │  │  │
│  │  │   subscribe() ──► Cost Tracker                                                    │  │  │
│  │  │   subscribe() ──► Perf Profiler                                                   │  │  │
│  │  └──────────────────────────────────────────────────────────────────────────────────┘  │  │
│  │                                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │  │
│  │  │   Tracing     │  │  Structured  │  │  #[traced]   │  │  Perf        │              │  │
│  │  │   Spans       │  │  Logging     │  │  Macro       │  │  Profiler    │              │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘              │  │
│  └─────────────────────────────────────────────────────────────────────────────────────────┘  │
│                                                                                                │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                               Macro Layer (Compile-Time)                                 │  │
│  │                                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                                 │  │
│  │  │  #[agent]     │  │  #[tool]     │  │  #[traced]   │                                 │  │
│  │  │              │  │              │  │              │                                 │  │
│  │  │ validate:   │  │ generate:   │  │ inject:     │                                 │  │
│  │  │  - name     │  │  - JSON     │  │  - span     │                                 │  │
│  │  │  - prompt   │  │    schema   │  │    create   │                                 │  │
│  │  │  - tools    │  │  - dispatch │  │  - fields   │                                 │  │
│  │  │  - return   │  │  - metadata │  │    from args│                                 │  │
│  │  │    types    │  │              │  │              │                                 │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘                                 │  │
│  └─────────────────────────────────────────────────────────────────────────────────────────┘  │
│                                                                                                │
│  ┌─────────────────────────────────────────────────────────────────────────────────────────┐  │
│  │                              Foundation Layer (agtrs)                                    │  │
│  │                                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │  │
│  │  │  Agent Trait  │  │  Transport   │  │  Tool Trait  │  │  State      │              │  │
│  │  │  Definition   │  │  Trait       │  │  Definition  │  │  Machine    │              │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘              │  │
│  └─────────────────────────────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Subsystem Interaction Map

The following diagram shows the primary data flows between subsystems, with
arrow direction indicating the direction of function calls / data flow.

```
                              ┌─────────┐
                              │  User   │
                              └────┬────┘
                                   │ CLI input
                                   ▼
┌──────────┐    config     ┌──────────────┐    prompt     ┌──────────────┐
│ Config   │◄──────────────│  Session     │──────────────►│  Prompt      │
│ Store    │               │  Manager     │               │  Builder     │
└──────────┘               └──────┬───────┘               └──────┬───────┘
                                  │                              │
                           create session                formatted messages
                                  │                              │
                                  ▼                              ▼
                          ┌──────────────────────────────────────────┐
                          │            AgentExecutor                  │
                          │                                           │
                          │  ┌─────────┐  ┌──────────┐  ┌────────┐ │
                          │  │ Loop    │  │  State   │  │ Plan   │ │
                          │  │ Control │──►│  Machine │  │ Manager│ │
                          │  └────┬────┘  └──────────┘  └────────┘ │
                          │       │                                  │
                          └───────┼──────────────────────────────────┘
                                  │
                    ┌─────────────┼──────────────┐
                    │             │              │
                    ▼             ▼              ▼
            ┌──────────┐  ┌──────────┐  ┌──────────────┐
            │   LLM    │  │  Tools   │  │  Delegation  │
            │  Client  │  │  Router  │  │  Manager     │
            └─────┬────┘  └─────┬────┘  └──────┬───────┘
                  │             │               │
                  ▼             ▼               ▼
            ┌──────────┐  ┌──────────┐  ┌──────────────┐
            │Transport │  │FileEditor│  │  Sub-Agent   │
            │ (HTTP)   │  │ShellExec │  │  Executor    │
            │          │  │GitOps    │  │  (recursive) │
            └──────────┘  │WASM      │  └──────────────┘
                          └─────┬────┘
                                │
                    ┌───────────┼───────────┐
                    │           │           │
                    ▼           ▼           ▼
            ┌──────────┐ ┌──────────┐ ┌──────────┐
            │Workspace │ │  Shell   │ │  GitRepo │
            │  Store   │ │ Process  │ │ (libgit2)│
            │(transact)│ │ (tokio)  │ │          │
            └──────────┘ └──────────┘ └──────────┘

    ┌──────────────────────────────────────────────────────┐
    │                 SignalBus (broadcast)                 │
    │  ◄── emit() from: Agent, LLM, Tools, Workspace,     │
    │                      Git, Budget, Delegation         │
    │  ──► subscribe(): TUI, DebugLog, CostTracker,       │
    │                      PerfProfiler, HistoryManager    │
    └──────────────────────────────────────────────────────┘
```

---

## 3. Data Model

### 3.1 Core Types

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Core Data Model                                     │
│                                                                              │
│  TaskId ──────► Task ◄─────── AgentState                                    │
│                   │              │                                           │
│                   │              ├── Initialized                             │
│                   │              ├── Planning                                │
│                   ├── id         ├── Executing                               │
│                   ├── prompt     ├── Validating                              │
│                   ├── state      ├── AwaitingApproval                        │
│                   ├── plan ──────├── RollingBack                             │
│                   ├── turns      ├── Delegating                              │
│                   ├── result     ├── Completed                               │
│                   └── cost       └── Failed                                  │
│                                                                              │
│  Turn ◄─────────────────────────────┐                                       │
│    ├── index                        │                                       │
│    ├── llm_request  ──► LlmRequest │                                       │
│    ├── llm_response ──► LlmResponse│                                       │
│    ├── tool_calls ──► Vec<ToolCall> │                                       │
│    ├── duration                    │                                       │
│    └── token_usage ──► TokenUsage  │                                       │
│                                     │                                       │
│  LlmRequest                         │                                       │
│    ├── model                        │                                       │
│    ├── messages ──► Vec<Message>    │                                       │
│    ├── tools ──► Vec<ToolDef>       │                                       │
│    └── max_tokens                   │                                       │
│                                                                              │
│  LlmResponse                          ToolCall                              │
│    ├── content                         ├── name                              │
│    ├── tool_calls ──┐                  ├── parameters (JSON Value)          │
│    ├── stop_reason  │                  └── id                                │
│    └── usage ───────┤                                                       │
│                      │                  ToolResult                           │
│  TokenUsage          │                  ├── output (String)                  │
│    ├── input_tokens  │                  ├── error (Option<String>)           │
│    ├── output_tokens │                  ├── duration                         │
│    └── cache_read    │                  └── is_error (bool)                  │
│                      │                                                       │
│  Plan ◄──────────────┤                                                       │
│    ├── steps ──► Vec<PlanStep>                                               │
│    ├── current_step                                                          │
│    └── modifications                                                          │
│                                                                              │
│  PlanStep                                                                    │
│    ├── index                                                                 │
│    ├── description                                                           │
│    ├── status (Pending/InProgress/Completed/Skipped/Failed)                  │
│    ├── tool_calls (predicted)                                                │
│    └── actual_tool_calls (executed)                                          │
│                                                                              │
│  FileChange                                                                  │
│    ├── path (PathBuf)                                                        │
│    ├── change_type (Create/Modify/Delete/Rename)                             │
│    ├── diff_stats (Option<DiffStats>)                                        │
│    └── transaction_id                                                        │
│                                                                              │
│  Delegation                                                                  │
│    ├── from_agent (String)                                                   │
│    ├── to_agent (String)                                                     │
│    ├── task (String)                                                         │
│    ├── result (Option<DelegationResult>)                                     │
│    └── cost (f64)                                                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 3.2 Configuration Hierarchy

```
┌──────────────────────────────────────────────────────────────────┐
│                 Configuration Resolution                          │
│                                                                   │
│  CLI Flags ──────────┐                                           │
│  (--model, --budget) │   Highest priority                        │
│                       ▼                                           │
│  Environment Variables ──► XAFT_MODEL, XAFT_BUDGET_LIMIT         │
│                            .env file                              │
│                       ▼                                           │
│  Project Config ───────► .xaft/config.toml                       │
│                          (checked into VCS)                       │
│                       ▼                                           │
│  User Config ──────────► ~/.config/xaft/config.toml              │
│                          (personal preferences)                   │
│                       ▼                                           │
│  Built-in Defaults ───► model: claude-sonnet-4-20250514          │
│                          budget: $10.00                           │
│                          max_turns: 50                            │
│                          Lowest priority                          │
└──────────────────────────────────────────────────────────────────┘
```

---

## 4. Layer Responsibilities

### 4.1 Layer Descriptions

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Layer              │ Responsibility             │ Key Types / Traits        │
├────────────────────┼────────────────────────────┼───────────────────────────┤
│ CLI / TUI          │ User interaction, display   │ XaftCli, TuiRenderer      │
│ Session Manager    │ Config, credentials, setup  │ Session, Config, CredStore│
│ Execution Engine   │ Agent loop, planning, state │ AgentExecutor, PlanMgr    │
│ LLM Layer          │ API communication, retry    │ Transport, LlmClient      │
│ Tool Layer         │ Action execution, dispatch  │ ToolRegistry, FileEditor  │
│ Workspace Layer    │ File ops, transactions, git │ WorkspaceStore, GitRepo   │
│ Observability      │ Signals, traces, metrics    │ SignalBus, #[traced]      │
│ Macro Layer        │ Compile-time validation     │ #[agent], #[tool]         │
│ Foundation (agtrs) │ Core trait definitions      │ Agent, Transport, Tool    │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 4.2 Dependency Direction

Dependencies flow downward — higher layers depend on lower layers, never the reverse.
The SignalBus is the only cross-cutting concern that is injected into all layers.

```
  CLI/TUI ──► Session ──► Execution ──► LLM
                                  ──► Tools ──► Workspace
                                  ──► Delegation ──► (recursive)

  All layers ──► SignalBus (injected, not imported upward)
  All layers ──► agtrs Foundation Traits
  Macro Layer ──► rustc (compile-time only, no runtime dependency)
```

---

## 5. Process Architecture

### 5.1 Runtime Process Model

```
┌──────────────────────────────────────────────────────────────────────┐
│                     xaft Runtime Process                             │
│                                                                      │
│  Main Thread (tokio runtime)                                         │
│  ├── CLI argument parsing                                            │
│  ├── Session initialization                                          │
│  └── tokio::spawn(agent_loop)                                       │
│                                                                      │
│  tokio Tasks:                                                        │
│  ├── agent_loop (main execution loop)                                │
│  │   ├── llm_request (HTTP client, async)                           │
│  │   ├── tool_execution (file/shell/git, async)                     │
│  │   └── state_transitions (sync, fast)                              │
│  ├── signal_handler (SignalBus consumer → TUI)                      │
│  ├── budget_tracker (cost accumulation)                              │
│  ├── file_watcher (notify, async)                                    │
│  └── perf_profiler (periodic sampling)                               │
│                                                                      │
│  Blocking Pool (for CPU-bound / blocking operations):                │
│  ├── git operations (libgit2)                                        │
│  ├── file I/O (large file reads)                                     │
│  ├── diff computation                                                │
│  └── WASM plugin execution (wasmtime)                                │
│                                                                      │
│  Thread Count:                                                       │
│  ├── 1 main thread (tokio RT)                                        │
│  ├── N worker threads (tokio RT, default = CPU cores)               │
│  ├── M blocking threads (tokio blocking pool)                        │
│  └── 1 TUI thread (crossterm event loop)                            │
└──────────────────────────────────────────────────────────────────────┘
```

### 5.2 Memory Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                     xaft Memory Layout                               │
│                                                                      │
│  Stack (per task, ~8MB default):                                     │
│  ├── Agent state machine (small, enum-based)                         │
│  ├── Turn context (references to heap data)                          │
│  └── Local variables for current turn                                │
│                                                                      │
│  Heap (shared via Arc):                                              │
│  ├── Arc<WorkspaceStore> — file contents cache                       │
│  ├── Arc<SignalBus> — broadcast sender                               │
│  ├── Arc<ToolRegistry> — tool dispatch table                         │
│  ├── Arc<BudgetTracker> — atomic cost counters                       │
│  ├── Arc<TaskStore> — execution history                              │
│  └── Arc<LlmClient> — HTTP connection pool                           │
│                                                                      │
│  Shared State Protection:                                            │
│  ├── RwLock for read-heavy (WorkspaceStore files map)                │
│  ├── Mutex for write-heavy (TaskStore execution log)                 │
│  ├── AtomicU64 for counters (BudgetTracker, SignalBus)               │
│  └── broadcast::Sender for SignalBus (lock-free reads)              │
│                                                                      │
│  Typical Memory Usage:                                               │
│  ├── Binary + runtime:        ~3 MB                                  │
│  ├── LLM client + TLS:        ~5 MB                                  │
│  ├── Workspace cache:          ~2 MB (depends on project size)       │
│  ├── TUI buffers:             ~1 MB                                  │
│  ├── Agent execution state:   ~2 MB                                  │
│  └── Total:                   ~15 MB (small project)                 │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 6. Error Handling Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                    Error Propagation Hierarchy                       │
│                                                                      │
│  ToolError                                                           │
│  ├── ExecutionFailed(String)    → Tool returns error to agent       │
│  ├── PermissionDenied           → Agent must request approval       │
│  ├── Timeout(Duration)          → Agent retries or modifies cmd     │
│  └── InvalidParameters(String)  → Agent fixes parameters            │
│                                                                      │
│  TransportError                                                      │
│  ├── RateLimited { retry_after } → LlmClient auto-retries           │
│  ├── ConnectionFailed            → LlmClient retries with backoff   │
│  ├── InvalidResponse(String)     → Agent retries with new prompt    │
│  └── AuthenticationFailed        → Session halts, user action needed│
│                                                                      │
│  WorkspaceError                                                      │
│  ├── FileNotFound(PathBuf)       → Agent adjusts path               │
│  ├── PermissionDenied(PathBuf)   → Agent requests approval          │
│  ├── TransactionConflict         → TransactionManager resolves      │
│  └── DiskFull                    → Session halts, user action needed│
│                                                                      │
│  AgentError                                                          │
│  ├── MaxTurnsExceeded            → Session ends with partial result │
│  ├── BudgetExceeded              → Session ends with partial result │
│  ├── CompilationFailed(String)   → Agent rolls back + retries       │
│  ├── PlanningFailed(String)      → Agent retries with simpler plan  │
│  └── CriticalError(String)       → Session halts, rollback all     │
│                                                                      │
│  Recovery Strategy per Layer:                                        │
│  ├── Tool layer:     retry 2x, then return error to agent           │
│  ├── LLM layer:      retry with exponential backoff (max 5x)        │
│  ├── Workspace layer: transactional rollback, then retry             │
│  └── Agent layer:    plan modification, re-plan, or fail gracefully │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 7. Security Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                    Security Boundaries                                │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Trust Level 0: User Input                                     │ │
│  │  - CLI arguments, prompts                                      │ │
│  │  - Validated by clap, no shell injection possible              │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                              │                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Trust Level 1: LLM Output                                     │ │
│  │  - Tool call parameters, content                               │ │
│  │  - Validated by #[tool] macro-generated schema                 │ │
│  │  - Path traversal protection                                   │ │
│  │  - Command injection protection (shell_exec)                   │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                              │                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Trust Level 2: Tool Execution                                 │ │
│  │  - File operations restricted to project directory             │ │
│  │  - Shell commands filtered by allowlist/denylist               │ │
│  │  - Git operations restricted to project repo                   │ │
│  │  - Optional: sandboxed in Docker/Podman container              │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                              │                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Trust Level 3: Credential Storage                             │ │
│  │  - API keys stored in OS keychain (never plaintext)            │ │
│  │  - Keys redacted from logs and signals                         │ │
│  │  - .xaft/ added to .gitignore automatically                   │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                      │
│  Approval Policy (configurable):                                     │
│  ├── None:        Auto-approve all operations                       │
│  ├── SafeOnly:    Require approval for delete, shell_exec           │
│  ├── Destructive: Require approval for any write/delete             │
│  └── All:         Require approval for every operation              │
└──────────────────────────────────────────────────────────────────────┘
```

This architecture provides a complete map of xaft's subsystems, their interactions,
data models, and cross-cutting concerns. Each subsystem has clear responsibilities
and well-defined boundaries, enabling independent development and testing.
