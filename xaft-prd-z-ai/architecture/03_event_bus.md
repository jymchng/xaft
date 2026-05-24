# 03 — Event Bus

> SignalBus deep dive: all 30+ event types, sync vs async delivery, event flows,
> TUI rendering, cost tracking, git lifecycle, debugging, and extensibility.

---

## Overview

The `SignalBus` is agtrs's central event dispatch system. Every significant occurrence in xaft — LLM tokens, tool calls, file edits, git operations, budget events — emits a typed signal that any number of subscribers can consume. This architecture decouples producers from consumers, enabling the TUI, cost tracker, audit logger, and debugging subsystems to all react to the same events independently.

xaft uses the SignalBus as its **spine**: all cross-cutting concerns flow through it, and no subsystem directly calls another subsystem. This makes the system observable, testable, and extensible.

---

## SignalBus Architecture

```rust
/// Central event bus for typed signal dispatch.
/// Supports both synchronous (blocking) and asynchronous subscribers.
pub struct SignalBus {
    /// Typed channel for each signal variant.
    channels: HashMap<TypeId, Box<dyn Any + Send + Sync>>,

    /// Ordered log of all emitted signals (for replay/debugging).
    signal_log: Arc<Mutex<Vec<LoggedSignal>>>,

    /// Configuration for delivery semantics.
    config: SignalBusConfig,
}

pub struct SignalBusConfig {
    /// Maximum number of signals to keep in the log.
    pub log_capacity: usize,

    /// Whether to block on synchronous subscriber delivery.
    pub blocking_sync: bool,

    /// Channel buffer size for async subscribers.
    pub async_buffer_size: usize,

    /// Whether to panic or log on subscriber errors.
    pub subscriber_error_policy: SubscriberErrorPolicy,
}

pub enum SubscriberErrorPolicy {
    /// Log the error and continue.
    Log,
    /// Panic on subscriber errors (for tests).
    Panic,
}
```

### Subscription Model

```rust
impl SignalBus {
    /// Subscribe to a specific signal type with an async receiver.
    /// Returns a bounded mpsc receiver.
    pub fn subscribe<S: Signal>(&self) -> mpsc::Receiver<S> {
        let (tx, rx) = mpsc::channel(self.config.async_buffer_size);
        self.channels
            .entry(TypeId::of::<S>())
            .or_insert_with(|| Box::new(SignalChannel::<S>::new()))
            .downcast_mut::<SignalChannel<S>>()
            .unwrap()
            .add_async_subscriber(tx);
        rx
    }

    /// Subscribe with a synchronous callback.
    /// Callbacks are invoked inline during emit — use with caution.
    pub fn subscribe_sync<S: Signal, F>(&self, callback: F)
    where
        F: Fn(&S) -> Result<(), SignalError> + Send + Sync + 'static,
    {
        self.channels
            .entry(TypeId::of::<S>())
            .or_insert_with(|| Box::new(SignalChannel::<S>::new()))
            .downcast_mut::<SignalChannel::<S>>()
            .unwrap()
            .add_sync_subscriber(Arc::new(callback));
    }

    /// Emit a signal to all subscribers.
    /// Sync subscribers are called first (in registration order).
    /// Async subscribers receive via their channels.
    pub fn emit<S: Signal>(&self, signal: S) -> Result<(), SignalError> {
        // Log the signal
        if let Ok(mut log) = self.signal_log.lock() {
            if log.len() >= self.config.log_capacity {
                log.drain(..log.len() / 4);
            }
            log.push(LoggedSignal {
                type_name: std::any::type_name::<S>().to_string(),
                timestamp: std::time::Instant::now(),
                serialized: serde_json::to_string(&signal).ok(),
            });
        }

        // Dispatch to channel
        if let Some(channel) = self.channels.get(&TypeId::of::<S>()) {
            channel
                .downcast_ref::<SignalChannel<S>>()
                .unwrap()
                .dispatch(&signal, &self.config)?;
        }

        Ok(())
    }
}

/// Trait for all signal types.
pub trait Signal: Clone + Send + Sync + 'static + Serialize {}

/// Internal channel for a specific signal type.
struct SignalChannel<S: Signal> {
    sync_subscribers: Vec<Arc<dyn Fn(&S) -> Result<(), SignalError> + Send + Sync>>,
    async_subscribers: Vec<mpsc::Sender<S>>,
}
```

---

## Complete Event Catalog

All signal types used by xaft, organized by subsystem:

### Agent Lifecycle Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 1 | `AgentStarted` | `{ agent_id, plan? }` | Async | TUI, Audit |
| 2 | `AgentFinished` | `{ agent_id, outcome, total_turns, total_cost }` | Async | TUI, Cost Tracker |
| 3 | `BeforeLlmCall` | `{ turn, model }` | Sync+Async | Cost Tracker, TUI |
| 4 | `AfterLlmCall` | `{ turn, tokens_used, cost_usd, tool_calls }` | Sync+Async | Cost Tracker, TUI |
| 5 | `BeforeTool` | `{ tool, turn }` | Sync+Async | Audit, TUI |
| 6 | `ToolExecuting` | `{ tool, args }` | Async | TUI |
| 7 | `ToolResult` | `{ call_id, success }` | Async | TUI, Audit |
| 8 | `ToolBlocked` | `{ tool, reason }` | Async | TUI, Audit |
| 9 | `ToolRejected` | `{ call_id }` | Async | Agent (via context) |
| 10 | `ToolResultsCollected` | `{ turn, count }` | Async | TUI |

### Streaming Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 11 | `StreamToken` | `{ token, model }` | Async | TUI, SSE |
| 12 | `StreamThinking` | `{ content }` | Async | TUI (thinking panel) |
| 13 | `StreamToolProgress` | `{ tool, progress }` | Async | TUI, SSE |
| 14 | `StreamError` | `{ error, recoverable }` | Async | TUI, Logging |

### Turn & Plan Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 15 | `TurnComplete` | `{ turn, cumulative_cost, files_modified }` | Async | TUI, Cost Tracker |
| 16 | `PlanningStarted` | `{ planner }` | Async | TUI |
| 17 | `PlanCreated` | `{ plan: TaskPlan }` | Async | TUI, Audit |
| 18 | `PlanStepComplete` | `{ step, total, description }` | Async | TUI (progress bar) |
| 19 | `PlanComplete` | `{ }` | Async | TUI |
| 20 | `PlanRejected` | `{ }` | Async | TUI, Audit |
| 21 | `PlanningFailed` | `{ error }` | Async | TUI, Logging |
| 22 | `MaxTurnsReached` | `{ max }` | Async | TUI, Audit |

### Cost & Budget Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 23 | `CostIncrement` | `{ amount, cumulative, provider }` | Sync | Cost Tracker |
| 24 | `BudgetExhausted` | `{ }` | Sync+Async | Runtime (cancel), TUI |
| 25 | `DailyBudgetExhausted` | `{ }` | Sync+Async | Runtime (cancel), TUI |
| 26 | `BudgetWarning` | `{ percent_used }` | Async | TUI |

### Git & Workspace Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 27 | `BranchCreated` | `{ name }` | Async | TUI, Audit |
| 28 | `AutoCommit` | `{ message }` | Async | TUI, Audit |
| 29 | `FileEditStarted` | `{ path, operation }` | Async | TUI |
| 30 | `FileEditCommitted` | `{ path, lines_changed }` | Async | TUI, Git Hook |
| 31 | `FileEditRolledBack` | `{ path, reason }` | Async | TUI, Audit |
| 32 | `WorkspaceDirty` | `{ files: Vec<PathBuf> }` | Async | TUI (status bar) |
| 33 | `WorkspaceClean` | `{ }` | Async | TUI (status bar) |

### Sub-Agent Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 34 | `SubagentStarted` | `{ agent_id, parent_id }` | Async | TUI |
| 35 | `SubagentComplete` | `{ agent_id, result_summary }` | Async | TUI, Parent Agent |

### Session Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 36 | `SessionStarted` | `{ session_id, workspace_root }` | Async | TUI, Logging |
| 37 | `SessionComplete` | `{ session_id, exit_code }` | Async | TUI, Audit |
| 38 | `SessionSuspended` | `{ session_id, reason }` | Async | TUI |
| 39 | `SessionResumed` | `{ session_id }` | Async | TUI |

### Provider Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 40 | `ProviderError` | `{ provider, error }` | Async | TUI, FallbackProvider |
| 41 | `ProviderFallback` | `{ from, to, reason }` | Async | TUI, Cost Tracker |

### Debug/Internal Events

| # | Signal Type | Payload | Delivery | Primary Consumer |
|---|---|---|---|---|
| 42 | `ContextReduced` | `{ attempt }` | Async | Logging |
| 43 | `LifecycleError` | `{ phase, error }` | Async | Logging, TUI |
| 44 | `GuardrailTriggered` | `{ guardrail, verdict }` | Async | Audit, TUI |
| 45 | `CacheHit` | `{ tool, key }` | Async | Metrics |
| 46 | `CacheMiss` | `{ tool, key }` | Async | Metrics |

---

## Sync vs Async Delivery

The delivery mode of each signal is critical for correctness and performance:

### Synchronous (Blocking)

Sync subscribers are called **inline** during `emit()`. This means the emitter blocks until all sync subscribers have processed the signal. Use sync delivery when:

- The signal must be processed **before** the next step can proceed
- The subscriber needs to **modify** the signal or **veto** an action
- Ordering guarantees are critical

**Rules for sync subscribers:**
1. Never block on I/O (network, disk) in a sync subscriber
2. Never call `emit()` from a sync subscriber (prevents reentrancy)
3. Keep processing time under 100μs

### Asynchronous (Buffered)

Async subscribers receive signals via an mpsc channel. The emitter does not wait for processing. Use async delivery when:

- Processing may take significant time (TUI rendering, network calls)
- The signal is informational only (no modification or veto needed)
- Multiple subscribers should process independently

**Backpressure handling for async:**

```
Producer (emit) ──▶ Channel (buffer: N) ──▶ Consumer (subscriber)

If channel is full:
  ├── DropOldest: drop the oldest signal in the buffer
  ├── DropCurrent: drop the signal being emitted
  ├── Block: block the emitter until space is available
  └── Warn: emit a warning and drop (default for TUI)
```

### Delivery Semantics by Event Category

| Category | Delivery | Rationale |
|---|---|---|
| Agent Lifecycle (1-10) | Mixed: BeforeLlmCall is sync, rest async | Before may veto; After is informational |
| Streaming (11-14) | Async | Must not block the LLM stream |
| Turn & Plan (15-22) | Async | Informational only |
| Cost & Budget (23-26) | CostIncrement sync; rest async | Cost must be accurate before proceeding |
| Git & Workspace (27-33) | Async | Informational; git ops are separate |
| Session (36-39) | Async | Informational |
| Provider (40-41) | ProviderError sync; rest async | Fallback must be immediate |
| Debug (42-46) | Async | Never block on diagnostics |

---

## Event Flow Diagrams

### LLM Call Flow

```
AgentExecutor
    │
    ├── agent.before_llm_call()
    │       │
    │       ▼
    │   ┌────────────────────────────────────────────┐
    │   │ SignalBus::emit(BeforeLlmCall { turn, model })   │
    │   │                                            │
    │   │  Sync: CostTracker.check_budget()          │
    │   │        └── If budget exhausted:             │
    │   │            emit(BudgetExhausted) ← SYNC     │
    │   │            return Err(BudgetExceeded)       │
    │   │                                            │
    │   │  Async: TUI.render_thinking_indicator()    │
    │   │         Logger.log("LLM call starting")    │
    │   └────────────────────────────────────────────┘
    │
    ├── provider.stream(request)
    │       │
    │       │  For each token:
    │       ▼
    │   ┌────────────────────────────────────────────┐
    │   │ SignalBus::emit(StreamToken { token })      │
    │   │                                            │
    │   │  Async: TUI.render_token(token)            │
    │   │         SSE.send(token)                    │
    │   └────────────────────────────────────────────┘
    │
    ├── agent.after_llm_call()
    │       │
    │       ▼
    │   ┌────────────────────────────────────────────┐
    │   │ SignalBus::emit(AfterLlmCall { turn,       │
    │   │   tokens, cost, tool_calls })               │
    │   │                                            │
    │   │  Sync: CostTracker.record(cost)             │
    │   │        └── emit(CostIncrement { amount })   │
    │   │                                            │
    │   │  Async: TUI.update_cost_display(cost)      │
    │   │         Audit.log("LLM response received") │
    │   └────────────────────────────────────────────┘
    │
    ▼
  Continue to tool dispatch
```

### File Edit Flow

```
Agent calls EditFile tool
    │
    ├── tool.before_edit()
    │       │
    │       ▼
    │   SignalBus::emit(FileEditStarted { path, operation })
    │       │
    │       │  Async: TUI.render_edit_start(path)
    │       │
    ├── FileEditor.replace_block(path, old, new)
    │       │
    │       │  (file is now dirty but not committed)
    │       │
    │       ▼
    │   SignalBus::emit(WorkspaceDirty { files: [path] })
    │       │
    │       │  Async: TUI.status_bar.show_dirty()
    │       │
    ├── FileEditor.commit()
    │       │
    │       │  (file is now committed to disk)
    │       │
    │       ▼
    │   SignalBus::emit(FileEditCommitted { path, lines_changed })
    │       │
    │       │  Async: TUI.render_diff(path, diff)
    │       │         GitAutoCommitHook.check_threshold()
    │       │
    │       │  If GitAutoCommitHook triggers:
    │       │     ├── GitRepo.commit_all(msg)
    │       │     └── SignalBus::emit(AutoCommit { message })
    │       │            │
    │       │            │  Async: TUI.render_commit(msg)
    │       │            │         Audit.log("auto-commit")
    │       │
    ├── OR FileEditor.rollback()
    │       │
    │       ▼
    │   SignalBus::emit(FileEditRolledBack { path, reason })
    │       │
    │       │  Async: TUI.render_rollback(path)
    │       │         Audit.log("rollback")
    │
    ▼
  Tool result returned to agent
```

### Git Operation Flow

```
Agent calls GitStatus / GitCommit / GitBranch
    │
    ├── GitRepo operation
    │       │
    │       ▼
    │   ┌──────────────────────────────────────────────┐
    │   │ SignalBus::emit(BranchCreated { name })       │
    │   │                                              │
    │   │  Async: TUI.render_branch_created(name)      │
    │   │         Audit.log("branch created")          │
    │   └──────────────────────────────────────────────┘
    │
    │   ── OR ──
    │
    │   ┌──────────────────────────────────────────────┐
    │   │ SignalBus::emit(AutoCommit { message })       │
    │   │                                              │
    │   │  Async: TUI.render_commit(message)           │
    │   │         Audit.log("auto-commit")             │
    │   └──────────────────────────────────────────────┘
    │
    ▼
  Tool result returned to agent
```

---

## TUI Event Consumption

The TUI subscribes to all async signals and renders them in the appropriate panel:

```rust
/// TUI event consumer — subscribes to SignalBus and renders events.
pub struct TuiEventConsumer {
    /// Channel receivers for all signal types.
    token_rx: mpsc::Receiver<StreamToken>,
    tool_rx: mpsc::Receiver<ToolExecuting>,
    result_rx: mpsc::Receiver<ToolResult>,
    cost_rx: mpsc::Receiver<CostIncrement>,
    turn_rx: mpsc::Receiver<TurnComplete>,
    file_rx: mpsc::Receiver<FileEditCommitted>,
    git_rx: mpsc<AutoCommit>,
    // ... other receivers
}

impl TuiEventConsumer {
    /// Process all pending events without blocking.
    /// Called from the TUI's 60fps render loop.
    pub fn process_events(&mut self) -> Vec<TuiUpdate> {
        let mut updates = Vec::new();

        // Process tokens (highest priority — streaming)
        while let Ok(token) = self.token_rx.try_recv() {
            updates.push(TuiUpdate::AppendToken(token.token));
        }

        // Process tool events
        while let Ok(tool) = self.tool_rx.try_recv() {
            updates.push(TuiUpdate::ToolStarted {
                name: tool.tool,
                args: tool.args,
            });
        }

        while let Ok(result) = self.result_rx.try_recv() {
            updates.push(TuiUpdate::ToolResult {
                call_id: result.call_id,
                success: result.success,
            });
        }

        // Process cost updates
        while let Ok(cost) = self.cost_rx.try_recv() {
            updates.push(TuiUpdate::CostUpdate {
                incremental: cost.amount,
                cumulative: cost.cumulative,
            });
        }

        // Process file edits
        while let Ok(file) = self.file_rx.try_recv() {
            updates.push(TuiUpdate::FileChanged {
                path: file.path,
                lines_changed: file.lines_changed,
            });
        }

        // Process git events
        while let Ok(git) = self.git_rx.try_recv() {
            updates.push(TuiUpdate::GitCommit {
                message: git.message,
            });
        }

        // ... process other events

        updates
    }
}
```

### TUI Panel Layout

```
┌─────────────────────────────────────────────────────────────┐
│ xaft — Session abc123  │  Turn 5/50  │  $0.34/$5.00  │ git │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ Agent Output (StreamToken events)                      │ │
│  │                                                         │ │
│  │ I'll fix the bug by updating the error handling in     │ │
│  │ src/lib.rs. The issue is that the function doesn't     │ │
│  │ check for None before unwrapping...                     │ │
│  │                                                         │ │
│  │ [Tool: edit_file] src/lib.rs:42-48                      │ │
│  │ ─── old ───                                             │ │
│  │ +     let result = container.unwrap();                  │ │
│  │ +++ new +++                                             │ │
│  │ +     let result = container.ok_or(Error::Empty)?;     │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  ┌─────────────────────┐  ┌──────────────────────────────┐ │
│  │ Plan Progress       │  │ Cost Tracker                  │ │
│  │                     │  │                               │ │
│  │ ✓ 1. Analyze bug    │  │ Turn 5: $0.08                │ │
│  │ ✓ 2. Fix code       │  │ Session: $0.34 / $5.00       │ │
│  │ → 3. Run tests      │  │ Daily: $2.10 / $25.00        │ │
│  │ ○ 4. Verify fix     │  │ ██████░░░░ 6.8%              │ │
│  └─────────────────────┘  └──────────────────────────────┘ │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ Tool Execution Log                                     │ │
│  │  ✓ edit_file: src/lib.rs (3 lines changed)            │ │
│  │  ✓ read_file: src/lib.rs                              │ │
│  │  → bash_exec: cargo test                              │ │
│  └────────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────┤
│ [a]pprove [r]eject [s]uspend [q]uit  │  Branch: xaft/task-fix-unwrap │
└─────────────────────────────────────────────────────────────┘
```

---

## Cost Tracking via Events

The cost tracking subsystem is entirely event-driven:

```rust
/// Cost tracker that subscribes to cost-related signals.
pub struct CostTracker {
    session_spent: AtomicF64,
    daily_spent: AtomicF64,
    session_budget: f64,
    daily_budget: f64,
}

impl CostTracker {
    /// Register as a sync subscriber on the SignalBus.
    pub fn register(bus: &SignalBus, budgets: (f64, f64)) -> Arc<Self> {
        let tracker = Arc::new(Self {
            session_spent: AtomicF64::new(0.0),
            daily_spent: AtomicF64::new(0.0),
            session_budget: budgets.0,
            daily_budget: budgets.1,
        });

        // Sync subscription — must process before emit returns
        let tracker_clone = tracker.clone();
        bus.subscribe_sync::<CostIncrement>(move |signal| {
            tracker_clone.session_spent.fetch_add(signal.amount);
            tracker_clone.daily_spent.fetch_add(signal.amount);

            let session_pct = tracker_clone.session_spent.load()
                / tracker_clone.session_budget * 100.0;
            let daily_pct = tracker_clone.daily_spent.load()
                / tracker_clone.daily_budget * 100.0;

            if daily_pct >= 100.0 {
                bus.emit(DailyBudgetExhausted)?;
            } else if session_pct >= 100.0 {
                bus.emit(BudgetExhausted)?;
            } else if session_pct >= 80.0 || daily_pct >= 80.0 {
                bus.emit(BudgetWarning {
                    percent_used: session_pct.max(daily_pct),
                })?;
            }

            Ok(())
        });

        tracker
    }
}
```

---

## Event Ordering Guarantees

xaft provides the following ordering guarantees:

1. **Within a turn**: Events are emitted in lifecycle order (BeforeLlmCall → StreamToken* → AfterLlmCall → BeforeTool → ToolResult → TurnComplete). No reordering is possible.

2. **Across turns**: Turn N's TurnComplete always arrives before Turn N+1's BeforeLlmCall.

3. **Sync before async**: For any single emit(), all sync subscribers process the signal before any async subscriber receives it.

4. **No ordering across signal types**: A StreamToken from Turn N+1 may arrive before a ToolResult from Turn N in async channels. Consumers must handle out-of-order delivery or correlate by turn number.

5. **Causal ordering for git events**: AutoCommit always follows FileEditCommitted for the files included in the commit.

---

## Adding Custom Events

To add a new signal type:

### Step 1: Define the Signal

```rust
// In xaft-signals crate

/// Custom signal for MCP tool discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDiscovered {
    pub server_name: String,
    pub tool_name: String,
    pub tool_schema: serde_json::Value,
}

impl Signal for McpToolDiscovered {}
```

### Step 2: Register Consumers

```rust
// In xaft-tui crate — subscribe to the new signal
let mcp_rx: mpsc::Receiver<McpToolDiscovered> = signal_bus.subscribe();

// In TuiEventConsumer
while let Ok(mcp) = self.mcp_rx.try_recv() {
    updates.push(TuiUpdate::McpToolAvailable {
        server: mcp.server_name,
        tool: mcp.tool_name,
    });
}
```

### Step 3: Emit from Producer

```rust
// In xaft-mcp crate — emit when a new MCP tool is discovered
signal_bus.emit(McpToolDiscovered {
    server_name: server.name.clone(),
    tool_name: tool.name.clone(),
    tool_schema: tool.schema.clone(),
})?;
```

### Step 4: Test

```rust
#[tokio::test]
async fn test_mcp_tool_discovered_signal() {
    let bus = SignalBus::new(SignalBusConfig::default());
    let rx = bus.subscribe::<McpToolDiscovered>();

    bus.emit(McpToolDiscovered {
        server_name: "test".to_string(),
        tool_name: "query".to_string(),
        tool_schema: serde_json::json!({}),
    }).unwrap();

    let signal = rx.recv().await.unwrap();
    assert_eq!(signal.server_name, "test");
}
```

---

## Signal Log and Replay

The SignalBus maintains an ordered log of all emitted signals. This enables:

1. **Debugging**: Inspect the exact sequence of events that led to an issue.
2. **Replay**: Reconstruct the TUI state from the signal log.
3. **Audit**: Full trace of all agent actions for compliance.

```rust
/// Logged signal for replay and debugging.
pub struct LoggedSignal {
    /// Type name of the signal.
    pub type_name: String,

    /// Timestamp relative to runtime start.
    pub timestamp: std::time::Instant,

    /// Serialized signal payload (JSON).
    pub serialized: Option<String>,
}

impl SignalBus {
    /// Dump the signal log to a file for post-mortem analysis.
    pub fn dump_log(&self, path: &Path) -> Result<(), std::io::Error> {
        let log = self.signal_log.lock().unwrap();
        let json = serde_json::to_string_pretty(&*log)?;
        std::fs::write(path, json)
    }

    /// Replay signals from a log file (for testing and debugging).
    pub fn replay_log(path: &Path) -> Result<Vec<LoggedSignal>, std::io::Error> {
        let json = std::fs::read_to_string(path)?;
        let log: Vec<LoggedSignal> = serde_json::from_str(&json)?;
        Ok(log)
    }
}
```

---

## Performance Considerations

| Metric | Target | Strategy |
|---|---|---|
| Sync subscriber latency | <100μs per subscriber | Keep sync callbacks minimal |
| Async channel throughput | >100K events/sec | Bounded channels with backpressure |
| Signal log overhead | <5% of total runtime | Lazy serialization, capacity limits |
| Memory per signal | <256 bytes average | Use references where possible, clone only when needed |
| TUI event processing | <1ms per frame | Non-blocking try_recv, batch processing |

### Optimization: Batching StreamTokens

For high-throughput streaming (e.g., GPT-4o generating long responses), individual `StreamToken` signals can overwhelm async channels. xaft batches tokens:

```rust
/// Batching wrapper for high-frequency signals.
pub struct SignalBatcher<S: Signal> {
    buffer: Vec<S>,
    buffer_size: usize,
    flush_interval: Duration,
    last_flush: Instant,
}

impl<S: Signal> SignalBatcher<S> {
    /// Add a signal to the batch. Flushes automatically when buffer is full
    /// or the flush interval has elapsed.
    pub fn add(&mut self, signal: S, bus: &SignalBus) -> Result<(), SignalError> {
        self.buffer.push(signal);

        if self.buffer.len() >= self.buffer_size
            || self.last_flush.elapsed() >= self.flush_interval
        {
            self.flush(bus)?;
        }

        Ok(())
    }

    fn flush(&mut self, bus: &SignalBus) -> Result<(), SignalError> {
        for signal in self.buffer.drain(..) {
            bus.emit(signal)?;
        }
        self.last_flush = Instant::now();
        Ok(())
    }
}
```

This ensures that token-by-token rendering still works smoothly while preventing the signal bus from becoming a bottleneck.
