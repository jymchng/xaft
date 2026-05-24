# xaft Observability

## 1. Overview

xaft's observability stack is built on three pillars: **signals** (real-time event streams),
**traces** (distributed span hierarchies), and **metrics** (aggregated quantitative data).
These pillars feed into the TUI dashboard, debug logs, and cost tracking systems, providing
full visibility into every agent turn, tool call, and state transition.

```
┌───────────────────────────────────────────────────────────────────────┐
│                      xaft Observability Architecture                  │
│                                                                       │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐                │
│  │   Signals    │   │   Traces    │   │   Metrics    │                │
│  │ (Event Bus)  │   │  (Spans)    │   │ (Counters)   │                │
│  └──────┬───────┘   └──────┬──────┘   └──────┬───────┘                │
│         │                  │                  │                        │
│         ▼                  ▼                  ▼                        │
│  ┌─────────────────────────────────────────────────────┐             │
│  │                    SignalBus                         │             │
│  │          (tokio broadcast, async stream)             │             │
│  └───────┬──────────────┬──────────────┬───────────────┘             │
│          │              │              │                              │
│     ┌────▼────┐   ┌────▼────┐   ┌────▼─────┐                        │
│     │   TUI   │   │  Debug  │   │  Cost     │                        │
│     │Dashboard│   │  Log    │   │ Tracker   │                        │
│     └─────────┘   └─────────┘   └──────────┘                        │
└───────────────────────────────────────────────────────────────────────┘
```

---

## 2. SignalBus

### 2.1 Architecture

The `SignalBus` is the central nervous system of xaft's observability. It is a
tokio-broadcast-based event bus that allows any subsystem to publish signals
and any consumer to subscribe to filtered streams.

```rust
/// Core signal type — every observable event in xaft is a Signal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signal {
    // Agent lifecycle
    AgentStarted { agent_name: String, task_id: TaskId, timestamp: DateTime<Utc> },
    AgentCompleted { agent_name: String, task_id: TaskId, duration: Duration, token_count: TokenCount },
    AgentFailed { agent_name: String, task_id: TaskId, error: String, timestamp: DateTime<Utc> },
    AgentStateChanged { agent_name: String, task_id: TaskId, from: AgentState, to: AgentState },

    // Tool execution
    ToolInvoked { agent_name: String, tool: String, params: Value, timestamp: DateTime<Utc> },
    ToolCompleted { agent_name: String, tool: String, duration: Duration, result_summary: String },
    ToolFailed { agent_name: String, tool: String, error: String },

    // LLM interaction
    LlmRequestSent { model: String, prompt_tokens: usize, timestamp: DateTime<Utc> },
    LlmResponseReceived { model: String, completion_tokens: usize, latency: Duration },
    LlmStreamChunk { model: String, chunk_size: usize },
    LlmRateLimited { model: String, retry_after: Option<Duration> },

    // File system operations
    FileRead { path: PathBuf, size_bytes: usize },
    FileWritten { path: PathBuf, size_bytes: usize, diff_stats: Option<DiffStats> },
    FileDeleted { path: PathBuf },

    // Git operations
    GitCommit { hash: String, message: String, files_changed: usize },
    GitBranch { name: String, action: GitBranchAction },

    // Planning
    PlanCreated { task_id: TaskId, step_count: usize },
    PlanStepStarted { task_id: TaskId, step_index: usize, description: String },
    PlanStepCompleted { task_id: TaskId, step_index: usize, duration: Duration },
    PlanModified { task_id: TaskId, reason: String },

    // Cost tracking
    CostAccumulated { model: String, input_tokens: usize, output_tokens: usize, cost_usd: f64 },
    BudgetThresholdReached { percentage: f64, current_usd: f64, limit_usd: f64 },

    // Performance
    PerformanceSnapshot { cpu_percent: f64, memory_mb: f64, active_tasks: usize },

    // Delegation (multi-agent)
    DelegationInitiated { from: String, to: String, task: String },
    DelegationCompleted { from: String, to: String, result_summary: String },
}

/// Diff statistics for file changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
    pub unchanged: usize,
}

/// Actions for git branch signals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitBranchAction {
    Created,
    Switched,
    Merged,
    Deleted,
}
```

### 2.2 SignalBus Implementation

```rust
/// The central signal bus — broadcast-based with subscriber filtering
pub struct SignalBus {
    sender: broadcast::Sender<Signal>,
    subscriber_count: Arc<AtomicUsize>,
    signal_counter: Arc<AtomicU64>,
}

impl SignalBus {
    pub fn new(buffer_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer_capacity);
        Self {
            sender,
            subscriber_count: Arc::new(AtomicUsize::new(0)),
            signal_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Publish a signal to all subscribers
    pub fn emit(&self, signal: Signal) {
        self.signal_counter.fetch_add(1, Ordering::Relaxed);
        // Lagged receivers are acceptable — signals are best-effort
        let _ = self.sender.send(signal);
    }

    /// Subscribe to all signals
    pub fn subscribe(&self) -> SignalStream {
        self.subscriber_count.fetch_add(1, Ordering::Relaxed);
        let receiver = self.sender.subscribe();
        SignalStream {
            receiver,
            filter: None,
            buffer: Vec::new(),
        }
    }

    /// Subscribe with a filter — only matching signals are delivered
    pub fn subscribe_filtered(&self, filter: SignalFilter) -> SignalStream {
        self.subscriber_count.fetch_add(1, Ordering::Relaxed);
        let receiver = self.sender.subscribe();
        SignalStream {
            receiver,
            filter: Some(filter),
            buffer: Vec::new(),
        }
    }

    /// Current subscriber count
    pub fn subscriber_count(&self) -> usize {
        self.subscriber_count.load(Ordering::Relaxed)
    }

    /// Total signals emitted since creation
    pub fn total_signals(&self) -> u64 {
        self.signal_counter.load(Ordering::Relaxed)
    }
}

/// Filter for selective signal subscription
#[derive(Clone)]
pub enum SignalFilter {
    /// Only signals matching any of the categories
    Categories(Vec<SignalCategory>),
    /// Only signals from a specific agent
    Agent(String),
    /// Only signals from specific tools
    Tools(Vec<String>),
    /// Custom predicate
    Custom(Arc<dyn Fn(&Signal) -> bool + Send + Sync>),
}

#[derive(Clone, Copy, Debug)]
pub enum SignalCategory {
    AgentLifecycle,
    ToolExecution,
    LlmInteraction,
    FileSystem,
    Git,
    Planning,
    Cost,
    Performance,
    Delegation,
}

/// Async stream of filtered signals
pub struct SignalStream {
    receiver: broadcast::Receiver<Signal>,
    filter: Option<SignalFilter>,
    buffer: Vec<Signal>,
}

impl SignalStream {
    /// Receive the next matching signal, skipping filtered ones
    pub async fn next(&mut self) -> Option<Signal> {
        loop {
            // Check buffer first
            if let Some(signal) = self.buffer.pop() {
                return Some(signal);
            }

            match self.receiver.recv().await {
                Ok(signal) => {
                    if self.matches_filter(&signal) {
                        return Some(signal);
                    }
                    // Skip non-matching signal
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    tracing::warn!("Signal stream lagged by {} signals", count);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return None;
                }
            }
        }
    }

    fn matches_filter(&self, signal: &Signal) -> bool {
        match &self.filter {
            None => true,
            Some(SignalFilter::Categories(cats)) => {
                cats.iter().any(|c| signal.category() == *c)
            }
            Some(SignalFilter::Agent(name)) => signal.agent_name() == Some(name.as_str()),
            Some(SignalFilter::Tools(tools)) => {
                matches!(signal, Signal::ToolInvoked { tool, .. } | Signal::ToolCompleted { tool, .. }
                    if tools.contains(tool))
            }
            Some(SignalFilter::Custom(pred)) => pred(signal),
        }
    }
}
```

### 2.3 Signal Subscription Patterns

```
┌─────────────────────────────────────────────────────────────────┐
│                  Signal Subscription Topology                    │
│                                                                  │
│  ┌──────────┐  emit()  ┌────────────┐  subscribe()  ┌───────┐ │
│  │ Agent    ├─────────►│            ├──────────────►│  TUI  │ │
│  │ Executor │          │            │  filtered:     │       │ │
│  └──────────┘          │  SignalBus │  Agent,Tool,   │ Live  │ │
│  ┌──────────┐  emit()  │            │  Llm,Cost      │View   │ │
│  │ Tool     ├─────────►│            ├──────────────►│       │ │
│  │ Registry │          │            │  subscribe()  └───────┘ │
│  └──────────┘          │            │                          │
│  ┌──────────┐  emit()  │            ├──────────────►┌───────┐ │
│  │ LLM     ├─────────►│            │  subscribe()  │ Debug │ │
│  │ Client  │          │            │  filtered:     │ Log   │ │
│  └──────────┘          │            │  all           │ File  │ │
│  ┌──────────┐  emit()  │            ├──────────────►└───────┘ │
│  │ Git     ├─────────►│            │  subscribe()              │
│  │ Ops     │          │            │  filtered:     ┌───────┐ │
│  └──────────┘          │            │  Cost only    │ Cost  │ │
│                        └────────────┘──────────────►│Tracker│ │
│                                                      └───────┘ │
└─────────────────────────────────────────────────────────────────┘
```

---

## 3. Tracing Spans

### 3.1 The `#[traced]` Macro

xaft provides a `#[traced]` attribute macro that automatically creates structured tracing
spans for agent methods and tool invocations. This eliminates boilerplate and ensures
consistent span naming.

```rust
/// The #[traced] macro automatically wraps the function in a tracing span
/// with structured fields extracted from the function arguments.
///
/// Usage:
///   #[traced(level = "info", fields(task_id, agent_name))]
///   async fn execute_task(&self, task_id: TaskId, agent_name: &str) -> Result<()>
///
/// Expands to:
///   async fn execute_task(&self, task_id: TaskId, agent_name: &str) -> Result<()> {
///       let __span = tracing::info_span!(
///           "execute_task",
///           task_id = %task_id,
///           agent_name = %agent_name,
///       );
///       let __guard = __span.enter();
///       // ... original body ...
///   }
```

**Macro Implementation (simplified):**

```rust
// In the xaft-macros crate

#[proc_macro_attribute]
pub fn traced(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as TracedAttrs);
    let func = parse_macro_input!(item as ItemFn);

    let level = attrs.level.unwrap_or(Level::Info);
    let extra_fields = attrs.fields;

    let func_name = &func.sig.ident;
    let func_name_str = func_name.to_string();

    // Extract parameter names for span fields
    let param_fields: Vec<_> = func.sig.inputs.iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(PatType { pat, .. }) => {
                if let Pat::Ident(ident) = pat.as_ref() {
                    Some(ident.ident.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .filter(|name| extra_fields.contains(&name.to_string()))
        .collect();

    let span_fields = quote! {
        #(#param_fields = %#param_fields,)*
    };

    let body = &func.block;

    let expanded = quote! {
        #func {
            let __span = tracing::#level_span!(
                #func_name_str,
                #span_fields
            );
            let __guard = __span.enter();
            #body
        }
    };

    TokenStream::from(expanded)
}
```

### 3.2 Span Hierarchy

xaft's tracing spans form a structured hierarchy that maps to the agent execution model:

```
xaft_session (span: session_id, user_prompt)
├── agent_execution (span: agent_name, task_id)
│   ├── planning (span: task_id)
│   │   ├── llm_request (span: model, prompt_tokens)
│   │   │   └── llm_response (span: model, completion_tokens, latency_ms)
│   │   └── plan_created (span: step_count)
│   ├── executing (span: task_id, step_index)
│   │   ├── tool_invocation (span: tool_name, params_digest)
│   │   │   ├── file_read (span: path, size_bytes)
│   │   │   ├── file_write (span: path, diff_stats)
│   │   │   ├── shell_exec (span: command, exit_code)
│   │   │   └── git_operation (span: operation, ref_name)
│   │   └── tool_completed (span: tool_name, duration_ms)
│   └── completion (span: task_id, total_tokens, total_cost)
├── delegation (span: from_agent, to_agent)
│   └── agent_execution (span: delegated_agent_name, delegated_task_id)
│       └── ...
└── cost_summary (span: total_usd, models_used)
```

### 3.3 Span Examples

```rust
#[traced(level = "info", fields(agent_name, task_id))]
async fn execute_agent_turn(
    &self,
    agent_name: &str,
    task_id: TaskId,
    prompt: &str,
) -> Result<TurnResult, AgentError> {
    // This automatically creates:
    // tracing::info_span!("execute_agent_turn", agent_name = %agent_name, task_id = %task_id);

    let response = self.llm_client.send(prompt).await?;
    // Nested span from llm_client.send() creates a child span

    for tool_call in &response.tool_calls {
        // Each tool invocation creates another child span
        let result = self.tool_registry.execute(tool_call).await?;
        self.signal_bus.emit(Signal::ToolCompleted {
            agent_name: agent_name.to_string(),
            tool: tool_call.name.clone(),
            duration: result.duration,
            result_summary: result.summary(),
        });
    }

    Ok(TurnResult::from(response))
}
```

---

## 4. Structured Logging

### 4.1 Log Format

xaft uses structured JSON logging in production and human-readable formatting in TUI mode.

**JSON format (CI/production):**
```json
{
  "timestamp": "2025-01-15T10:23:45.123Z",
  "level": "INFO",
  "span": { "name": "execute_agent_turn", "agent_name": "CodeWriter", "task_id": "t_01" },
  "message": "Tool invocation completed",
  "fields": {
    "tool": "file_write",
    "duration_ms": 12,
    "path": "/src/main.rs",
    "diff_additions": 5,
    "diff_deletions": 2
  },
  "target": "xaft::agent::executor",
  "thread_id": 3
}
```

**Human-readable format (TUI):**
```
10:23:45 INFO [CodeWriter:t_01] Tool completed: file_write /src/main.rs (+5/-2) 12ms
10:23:45 INFO [CodeWriter:t_01] Turn 3: 450 output tokens, $0.0032
10:23:46 WARN [CodeWriter:t_01] Rate limited by claude-sonnet-4-20250514, retrying in 2s
10:23:48 INFO [CodeWriter:t_01] Turn 4: 280 output tokens, $0.0020
```

### 4.2 Log Configuration

```rust
/// Configure logging based on the runtime environment
pub fn configure_logging(config: &LoggingConfig) -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.default_level));

    match config.format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(JsonStorageLayer)
                .with(JsonFormatter::new(config.output.as_path()))
                .init();
        }
        LogFormat::Human => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(HumanFormatter::new())
                .init();
        }
        LogFormat::Compact => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(CompactFormatter::new())
                .init();
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub default_level: String,
    pub format: LogFormat,
    pub output: Option<PathBuf>,
    pub module_overrides: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    Json,
    Human,
    Compact,
}
```

### 4.3 Module-Level Overrides

```rust
let config = LoggingConfig {
    default_level: "info".to_string(),
    format: LogFormat::Json,
    output: None,
    module_overrides: HashMap::from([
        ("xaft::llm".to_string(), "debug".to_string()),
        ("xaft::tool".to_string(), "debug".to_string()),
        ("xaft::agent".to_string(), "info".to_string()),
        ("hyper".to_string(), "warn".to_string()),
        ("tokio".to_string(), "warn".to_string()),
    ]),
};
```

---

## 5. TUI Signal Consumption

### 5.1 TUI Architecture

The terminal UI consumes signals from the SignalBus and renders them in real-time
using a ratatui-based interface.

```
┌─────────────────────────────────────────────────────────────────────┐
│ xaft v0.1.0 │ Task: "Refactor error handling" │ Cost: $0.045      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  Agent: CodeWriter (turn 5/20)                                       │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │ 🤖 I'll now update the error module to use anyhow.           │ │
│  │    Reading /src/error.rs first...                              │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                      │
│  Tools:                                                              │
│  ├─ file_read  /src/error.rs       ✅  2ms    1.2KB                │
│  ├─ file_write /src/error.rs       ✅  8ms    +12/-8               │
│  ├─ shell_exec "cargo check"       ✅  3.2s  exit 0                │
│  └─ file_write /src/main.rs       ✅  5ms    +3/-1                │
│                                                                      │
│  Stats:                                                              │
│  ├─ LLM:   5 turns, 3,240 tokens in, 1,890 tokens out              │
│  ├─ Cost:  $0.045 / $1.00 budget (4.5%)                             │
│  ├─ Files: 3 modified, 0 created, 0 deleted                         │
│  └─ Time:  45s elapsed                                              │
│                                                                      │
├─────────────────────────────────────────────────────────────────────┤
│ [q]uit [p]ause [d]ebug [s]tats [c]ost breakdown  │  ▊▊▊▊░░░ 4.5% │
└─────────────────────────────────────────────────────────────────────┘
```

### 5.2 TUI Signal Handler

```rust
pub struct TuiSignalHandler {
    signal_stream: SignalStream,
    state: Arc<Mutex<TuiState>>,
    render_tx: mpsc::UnboundedSender<TuiEvent>,
}

impl TuiSignalHandler {
    pub fn new(bus: &SignalBus, state: Arc<Mutex<TuiState>>, render_tx: mpsc::UnboundedSender<TuiEvent>) -> Self {
        let signal_stream = bus.subscribe_filtered(SignalFilter::Categories(vec![
            SignalCategory::AgentLifecycle,
            SignalCategory::ToolExecution,
            SignalCategory::LlmInteraction,
            SignalCategory::Cost,
            SignalCategory::Planning,
        ]));

        Self { signal_stream, state, render_tx }
    }

    pub async fn run(mut self) {
        while let Some(signal) = self.signal_stream.next().await {
            let event = match signal {
                Signal::AgentStarted { agent_name, task_id, .. } => {
                    TuiEvent::AgentStarted { agent_name, task_id }
                }
                Signal::ToolInvoked { tool, params, .. } => {
                    TuiEvent::ToolStarted { name: tool, params }
                }
                Signal::ToolCompleted { tool, duration, result_summary, .. } => {
                    TuiEvent::ToolCompleted { name: tool, duration, summary: result_summary }
                }
                Signal::ToolFailed { tool, error, .. } => {
                    TuiEvent::ToolFailed { name: tool, error }
                }
                Signal::LlmResponseReceived { completion_tokens, latency, .. } => {
                    TuiEvent::LlmTurnCompleted { tokens: completion_tokens, latency }
                }
                Signal::CostAccumulated { cost_usd, .. } => {
                    let mut state = self.state.lock().await;
                    state.total_cost += cost_usd;
                    TuiEvent::CostUpdated { total: state.total_cost }
                }
                Signal::BudgetThresholdReached { percentage, .. } => {
                    TuiEvent::BudgetWarning { percentage }
                }
                Signal::FileWritten { path, diff_stats, .. } => {
                    TuiEvent::FileModified { path, stats: diff_stats }
                }
                _ => continue,
            };

            let _ = self.render_tx.send(event);
        }
    }
}

#[derive(Debug, Clone)]
pub struct TuiState {
    pub agent_name: String,
    pub task_id: Option<TaskId>,
    pub current_turn: usize,
    pub max_turns: usize,
    pub total_cost: f64,
    pub budget_limit: f64,
    pub tool_history: Vec<ToolRecord>,
    pub files_modified: HashSet<PathBuf>,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub start_time: Instant,
    pub agent_thinking: Option<String>,
}
```

---

## 6. Debug Mode

### 6.1 Debug Mode Activation

Debug mode provides verbose output including full LLM request/response payloads,
internal state dumps, and step-by-step execution traces.

```rust
pub struct DebugConfig {
    /// Enable verbose LLM I/O logging
    pub log_llm_payloads: bool,
    /// Dump agent state at each transition
    pub dump_state_transitions: bool,
    /// Log tool call parameters in full (may contain sensitive data)
    pub log_full_tool_params: bool,
    /// Enable span enter/exit logging
    pub trace_span_lifecycle: bool,
    /// Write debug output to file instead of stderr
    pub output_file: Option<PathBuf>,
    /// Include full prompt text in logs
    pub include_prompts: bool,
    /// Include full response text in logs
    pub include_responses: bool,
    /// Redact patterns from logged output
    pub redact_patterns: Vec<String>,
}

impl DebugConfig {
    pub fn from_env() -> Self {
        Self {
            log_llm_payloads: std::env::var("XAFT_DEBUG_LLM").is_ok(),
            dump_state_transitions: std::env::var("XAFT_DEBUG_STATE").is_ok(),
            log_full_tool_params: std::env::var("XAFT_DEBUG_TOOLS").is_ok(),
            trace_span_lifecycle: std::env::var("XAFT_DEBUG_SPANS").is_ok(),
            output_file: std::env::var("XAFT_DEBUG_FILE").ok().map(PathBuf::from),
            include_prompts: std::env::var("XAFT_DEBUG_PROMPTS").is_ok(),
            include_responses: std::env::var("XAFT_DEBUG_RESPONSES").is_ok(),
            redact_patterns: std::env::var("XAFT_DEBUG_REDACT")
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default(),
        }
    }
}
```

### 6.2 Debug Output Example

```
[DEBUG 10:23:45.001] span.enter: execute_agent_turn(agent_name="CodeWriter", task_id="t_01")
[DEBUG 10:23:45.002] llm.request:
  model: claude-sonnet-4-20250514
  messages:
    [system] You are an expert Rust developer...
    [user] Refactor the error handling to use anyhow
  tools: [file_read, file_write, file_delete, shell_exec, git_commit]
  max_tokens: 4096

[DEBUG 10:23:46.893] llm.response:
  stop_reason: tool_use
  content: "I'll start by reading the current error module..."
  tool_calls:
    [0] file_read({"path":"/src/error.rs"})
  usage: {input: 1240, output: 89, cache_read: 800}

[DEBUG 10:23:46.894] state.transition: Planning -> Executing (task=t_01)
[DEBUG 10:23:46.895] tool.invoke: file_read({"path":"/src/error.rs"})
[DEBUG 10:23:46.897] tool.complete: file_read -> Ok(1.2KB, 2ms)

[DEBUG 10:23:46.898] llm.request:
  model: claude-sonnet-4-20250514
  messages:
    [system] You are an expert Rust developer...
    [user] Refactor the error handling to use anyhow
    [assistant] I'll start by reading the current error module...
    [tool_result] [file_read] contents of /src/error.rs (1.2KB)
  tools: [file_read, file_write, file_delete, shell_exec, git_commit]

[DEBUG 10:23:49.123] llm.response:
  stop_reason: tool_use
  content: "Now I'll rewrite the error module using anyhow."
  tool_calls:
    [0] file_write({"path":"/src/error.rs","content":"use anyhow::{Result, Context, ...}"})
  usage: {input: 2040, output: 234, cache_read: 1240}
```

---

## 7. Cost Tracking Events

### 7.1 Cost Model

xaft tracks costs in real-time using model-specific pricing tables and emits
cost signals that feed into both the TUI and the budget enforcement system.

```rust
/// Model pricing configuration
pub struct ModelPricing {
    pub input_per_million: f64,    // USD per 1M input tokens
    pub output_per_million: f64,   // USD per 1M output tokens
    pub cache_read_per_million: Option<f64>,  // Cached input discount
}

impl ModelPricing {
    pub fn for_model(model: &str) -> Self {
        match model {
            "claude-sonnet-4-20250514" => Self {
                input_per_million: 3.0,
                output_per_million: 15.0,
                cache_read_per_million: Some(0.30),
            },
            "claude-opus-4-20250514" => Self {
                input_per_million: 15.0,
                output_per_million: 75.0,
                cache_read_per_million: Some(1.50),
            },
            "gpt-4.1" => Self {
                input_per_million: 2.0,
                output_per_million: 8.0,
                cache_read_per_million: Some(0.50),
            },
            _ => Self {
                input_per_million: 5.0,
                output_per_million: 15.0,
                cache_read_per_million: None,
            },
        }
    }

    /// Calculate cost for a single LLM interaction
    pub fn calculate(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        let cache_discount = match (self.cache_read_per_million, usage.cache_read_tokens) {
            (Some(price), Some(tokens)) if tokens > 0 => {
                // Subtract full input cost for cached tokens, add discounted cost
                let full_cost = (tokens as f64 / 1_000_000.0) * self.input_per_million;
                let cached_cost = (tokens as f64 / 1_000_000.0) * price;
                -(full_cost - cached_cost)
            }
            _ => 0.0,
        };
        input_cost + output_cost + cache_discount
    }
}

#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cache_read_tokens: Option<usize>,
}
```

### 7.2 Budget Enforcement

```rust
/// Budget tracker that halts execution when limits are reached
pub struct BudgetTracker {
    total_cost: Arc<AtomicF64>,
    limit: f64,
    warning_thresholds: Vec<f64>,
    signal_bus: SignalBus,
    per_model_costs: Arc<Mutex<HashMap<String, f64>>>,
}

impl BudgetTracker {
    pub fn new(limit: f64, signal_bus: SignalBus) -> Self {
        Self {
            total_cost: Arc::new(AtomicF64::new(0.0)),
            limit,
            warning_thresholds: vec![0.25, 0.50, 0.75, 0.90, 0.95],
            signal_bus,
            per_model_costs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a cost event. Returns Err if budget exceeded.
    pub fn record(&self, model: &str, usage: &TokenUsage) -> Result<(), BudgetExceeded> {
        let pricing = ModelPricing::for_model(model);
        let cost = pricing.calculate(usage);

        self.total_cost.fetch_add(cost, Ordering::SeqCst);
        self.per_model_costs.lock().unwrap()
            .entry(model.to_string())
            .and_modify(|c| *c += cost)
            .or_insert(cost);

        let total = self.total_cost.load(Ordering::SeqCst);
        let percentage = total / self.limit * 100.0;

        // Emit cost signal
        self.signal_bus.emit(Signal::CostAccumulated {
            model: model.to_string(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_usd: cost,
        });

        // Check warning thresholds
        for threshold in &self.warning_thresholds {
            if total / self.limit >= *threshold {
                self.signal_bus.emit(Signal::BudgetThresholdReached {
                    percentage: total / self.limit * 100.0,
                    current_usd: total,
                    limit_usd: self.limit,
                });
            }
        }

        // Hard limit check
        if total >= self.limit {
            return Err(BudgetExceeded {
                total_cost: total,
                limit: self.limit,
            });
        }

        Ok(())
    }

    /// Check remaining budget
    pub fn remaining(&self) -> f64 {
        (self.limit - self.total_cost.load(Ordering::SeqCst)).max(0.0)
    }

    /// Generate cost breakdown
    pub fn breakdown(&self) -> CostBreakdown {
        let total = self.total_cost.load(Ordering::SeqCst);
        let per_model = self.per_model_costs.lock().unwrap().clone();
        CostBreakdown {
            total_usd: total,
            limit_usd: self.limit,
            percentage_used: total / self.limit * 100.0,
            per_model,
            remaining_usd: (self.limit - total).max(0.0),
        }
    }
}
```

---

## 8. Performance Profiling

### 8.1 Performance Signal Emitter

```rust
/// Periodically emits performance snapshots to the SignalBus
pub struct PerformanceProfiler {
    signal_bus: SignalBus,
    sample_interval: Duration,
    collector: Arc<PerformanceCollector>,
}

struct PerformanceCollector {
    cpu_usage: AtomicF64,
    memory_usage_mb: AtomicF64,
    active_tasks: AtomicUsize,
    pending_tool_calls: AtomicUsize,
    llm_requests_in_flight: AtomicUsize,
}

impl PerformanceProfiler {
    pub fn new(signal_bus: SignalBus, sample_interval: Duration) -> Self {
        Self {
            signal_bus,
            sample_interval,
            collector: Arc::new(PerformanceCollector {
                cpu_usage: AtomicF64::new(0.0),
                memory_usage_mb: AtomicF64::new(0.0),
                active_tasks: AtomicUsize::new(0),
                pending_tool_calls: AtomicUsize::new(0),
                llm_requests_in_flight: AtomicUsize::new(0),
            }),
        }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(self.sample_interval);

        loop {
            interval.tick().await;

            let cpu = self.collect_cpu_usage();
            let memory = self.collect_memory_usage();
            let active = self.collector.active_tasks.load(Ordering::Relaxed);

            self.signal_bus.emit(Signal::PerformanceSnapshot {
                cpu_percent: cpu,
                memory_mb: memory,
                active_tasks: active,
            });
        }
    }

    fn collect_cpu_usage(&self) -> f64 {
        // Use jemalloc stats or /proc/self/stat on Linux
        #[cfg(target_os = "linux")]
        {
            // Read from /proc/self/stat
            let stat = std::fs::read_to_string("/proc/self/stat").ok();
            // Parse utime + stime
            stat.and_then(|s| {
                let fields: Vec<&str> = s.split_whitespace().collect();
                let utime: f64 = fields.get(13)?.parse().ok()?;
                let stime: f64 = fields.get(14)?.parse().ok()?;
                Some((utime + stime) / 100.0) // Convert from ticks to percentage
            })
            .unwrap_or(0.0)
        }
        #[cfg(not(target_os = "linux"))]
        {
            0.0 // Placeholder for macOS/Windows
        }
    }

    fn collect_memory_usage(&self) -> f64 {
        // Use jemalloc stats when available
        #[cfg(feature = "jemalloc")]
        {
            let epoch = tikv_jemalloc_ctl::epoch::mib().unwrap();
            let allocated = tikv_jemalloc_ctl::stats::allocated::mib().unwrap();
            epoch.advance().unwrap();
            allocated.read().unwrap() as f64 / (1024.0 * 1024.0)
        }
        #[cfg(not(feature = "jemalloc"))]
        {
            0.0
        }
    }
}
```

### 8.2 Performance Report Generation

```rust
/// Generate a performance report from collected signals
pub async fn generate_performance_report(
    signal_bus: &SignalBus,
    session_duration: Duration,
) -> PerformanceReport {
    let mut stream = signal_bus.subscribe_filtered(SignalFilter::Categories(vec![
        SignalCategory::Performance,
        SignalCategory::LlmInteraction,
        SignalCategory::ToolExecution,
    ]));

    let mut report = PerformanceReport::default();

    while let Ok(signal) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await {
        match signal {
            Some(Signal::PerformanceSnapshot { cpu_percent, memory_mb, .. }) => {
                report.cpu_samples.push(cpu_percent);
                report.memory_samples.push(memory_mb);
            }
            Some(Signal::LlmResponseReceived { latency, .. }) => {
                report.llm_latencies.push(latency);
            }
            Some(Signal::ToolCompleted { duration, tool, .. }) => {
                report.tool_durations.entry(tool).or_default().push(duration);
            }
            _ => {}
        }
    }

    report.session_duration = session_duration;
    report
}

#[derive(Debug, Default)]
pub struct PerformanceReport {
    pub session_duration: Duration,
    pub cpu_samples: Vec<f64>,
    pub memory_samples: Vec<f64>,
    pub llm_latencies: Vec<Duration>,
    pub tool_durations: HashMap<String, Vec<Duration>>,
}
```

---

## 9. Summary

xaft's observability is designed as a first-class subsystem, not an afterthought. The
SignalBus provides a unified event stream that powers the TUI, debug logs, cost tracking,
and performance profiling. The `#[traced]` macro eliminates boilerplate for span creation
while ensuring consistent instrumentation across the codebase. Structured logging adapts
its format to the runtime environment, and the budget enforcement system uses cost signals
to prevent runaway spending. Together, these systems provide complete visibility into every
aspect of agent execution, from individual token costs to cross-agent delegation chains.
