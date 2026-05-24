# Event Bus Architecture

## Two Event Systems

`xaft` uses two complementary event systems from `agtrs-runtime`:

| System | Scope | Direction | Use |
|---|---|---|---|
| `SignalBus` | Process-wide | Broadcast (emit → all subscribers) | Observability, TUI, metrics |
| `AgentMessageBus` | Agent-to-agent | P2P + broadcast | Inter-agent coordination |

## SignalBus — Observability Backbone

The `SignalBus` is the single source of truth for all agent activity events. Every significant action in the `xaft` runtime emits a typed signal.

### Signal Types (agtrs built-in)

```rust
// Model interactions
ModelCallStarted { model, agent_id, agent_name, messages_count, input_tokens_estimate }
ModelCallComplete { model, agent_id, agent_name, usage, duration_ms, cost_usd, stop_reason }

// Tool interactions
ToolCallStarted { tool_name, tool_use_id, agent_id, input, cache_hit }
ToolCallComplete { tool_name, tool_use_id, agent_id, duration_ms, success, error }
ToolPendingApproval { agent_id, agent_run_id, tool_name, tool_use_id, input, risk_level }

// Agent lifecycle
AgentTurnComplete { agent_id, agent_name, turn, usage }
AgentRunComplete { agent_id, agent_name, turns, total_usage, total_cost_usd }
AgentCancelled { agent_id, agent_name, reason, turns_completed }

// Planning
PlanCreated { task_id, intent_goal, step_count }
PlanStepStarted { task_id, step_id, step_description, agent_name }
PlanStepCompleted { task_id, step_id, duration_ms }
PlanStepFailed { task_id, step_id, reason, will_replan }

// Task
TaskStateChanged { task_id, from, to, reason }
```

### xaft-specific Signals (additional)

```rust
// Workspace events
pub struct FileWritten {
    pub path: PathBuf,
    pub bytes_written: usize,
    pub agent_name: String,
}

pub struct PatchApplied {
    pub path: PathBuf,
    pub hunks_applied: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

pub struct WorktreeCreated {
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

pub struct WorktreeRemoved {
    pub worktree_path: PathBuf,
    pub committed: bool,
}

// Shell events
pub struct ShellCommandStarted {
    pub command: String,
    pub working_dir: PathBuf,
    pub agent_name: String,
}

pub struct ShellCommandComplete {
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: f64,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
}

// Index events
pub struct IndexingStarted { pub file_count: usize }
pub struct IndexingComplete { pub file_count: usize, pub duration_ms: f64 }
pub struct IndexQueryComplete { pub query: String, pub results: usize, pub duration_ms: f64 }

// Session events
pub struct SessionStarted { pub session_id: Uuid, pub project_root: PathBuf }
pub struct SessionComplete { pub session_id: Uuid, pub total_cost_usd: f64, pub duration_secs: f64 }
pub struct CheckpointSaved { pub task_id: Uuid, pub step: usize }
pub struct ApprovalRequested { pub tool_name: String, pub risk_level: RiskLevel }
pub struct ApprovalDecision { pub tool_name: String, pub approved: bool, pub reason: Option<String> }
```

### Signal Subscription Patterns

```rust
// TUI subscription (async broadcast — receives all events for rendering)
let mut rx = bus.subscribe::<ModelCallComplete>();
tokio::spawn(async move {
    while let Ok(sig) = rx.recv().await {
        ui_tx.send(UiEvent::ModelCallComplete(sig)).await.ok();
    }
});

// Metrics subscription (sync handler — lightweight counter increment)
bus.on::<ToolCallComplete>(move |s| {
    metrics::counter!("xaft_tool_calls_total",
        "tool" => s.tool_name.clone(),
        "success" => s.success.to_string()
    ).increment(1);
    metrics::histogram!("xaft_tool_duration_ms", s.duration_ms);
});

// Audit log subscription (async broadcast — write JSON lines)
let mut audit_rx = bus.subscribe::<ToolCallStarted>();
tokio::spawn(async move {
    while let Ok(sig) = audit_rx.recv().await {
        let line = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "event": "tool_call_started",
            "tool": sig.tool_name,
            "input": sig.input,
        });
        audit_writer.write_line(&line.to_string()).await.ok();
    }
});

// Cost tracker (sync handler — atomic accumulation)
let session_cost = Arc::new(AtomicF64::new(0.0));
let sc = Arc::clone(&session_cost);
bus.on::<ModelCallComplete>(move |s| {
    sc.fetch_add(s.cost_usd, Ordering::Relaxed);
});
```

## AgentMessageBus — Inter-Agent Coordination

When multiple specialized agents collaborate on a task, the `AgentMessageBus` enables direct communication without routing through the orchestrator.

### Coordination Pattern: CodeAgent → ReviewAgent

```
CodeAgent produces diff
    │ send TaskRequest("review_diff") → ReviewAgent
    ▼
ReviewAgent examines diff
    │ send QueryResponse(review_result) → CodeAgent
    ▼
CodeAgent incorporates review feedback
    │ (if issues found: edit files, stage again)
    ▼
CodeAgent sends Notification("diff_approved") → orchestrator
```

### Implementation

```rust
// CodeAgent tool: after writing files, request review
pub async fn request_review(&self, diff: &str, ctx: &ToolContext) -> Result<ReviewResult, AgtrsError> {
    let bus = ctx.message_bus().ok_or_else(|| AgtrsError::msg("no message bus"))?;

    let correlation_id = Uuid::new_v4().to_string();
    let mut rx = bus.subscribe("code_agent").await;

    // Send review request
    bus.send(AgentMessage::builder("code_agent", AgentMessageType::TaskRequest)
        .to("review_agent")
        .correlation_id(correlation_id.clone())
        .payload(serde_json::json!({
            "diff": diff,
            "context": "auth module migration",
        }))
        .build()).await?;

    // Wait for review response (with timeout)
    let response = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Ok(msg) = rx.recv().await {
                if msg.correlation_id.as_deref() == Some(&correlation_id) {
                    return msg;
                }
            }
        }
    }).await.map_err(|_| AgtrsError::msg("review timed out"))?;

    let result: ReviewResult = serde_json::from_value(response.payload.unwrap_or_default())?;
    Ok(result)
}
```

## Event Sourcing Considerations

While `xaft` v1 does not implement full event sourcing, the `SignalBus` architecture is designed to support it:

1. All signals are `Serialize + Clone` — can be persisted to a structured event log.
2. Signal types include sufficient context to replay events (agent_id, timestamps, inputs/outputs).
3. `CheckpointSaved` events mark safe replay points.
4. Future: `EventLog` store that writes all signals to SQLite → enables session replay and audit.

### Future EventLog interface

```rust
pub trait EventLog: Send + Sync {
    async fn append(&self, event: &dyn EraseableSignal) -> Result<u64, XaftError>;
    async fn replay_since(&self, sequence: u64) -> Result<Vec<BoxedSignal>, XaftError>;
    async fn snapshot_at(&self, task_id: Uuid) -> Result<SessionSnapshot, XaftError>;
}
```

## Signal Bus Architecture Constraints

- **Broadcast capacity**: 256 items per signal type. TUI subscriber must process faster than emission rate.
- **No persistence**: Signals are in-memory only. Audit logs require explicit subscription + write.
- **No ordering guarantees across types**: `ModelCallComplete` and `ToolCallStarted` may arrive in different order than execution if async subscriber is slow.
- **Single process**: `SignalBus` does not cross process boundaries. Remote agents use the HTTP API.

## TUI Event Pipeline

```
SignalBus (broadcast)
    │
    ├── rx<ModelCallComplete>  ──► UiEvent::ModelDone
    ├── rx<ToolCallStarted>    ──► UiEvent::ToolStart
    ├── rx<ToolCallComplete>   ──► UiEvent::ToolDone
    ├── rx<PlanStepStarted>    ──► UiEvent::StepStart
    ├── rx<FileWritten>        ──► UiEvent::FileChanged
    ├── rx<ApprovalRequested>  ──► UiEvent::ShowApprovalDialog
    └── rx<CheckpointSaved>    ──► UiEvent::CheckpointUpdate
         │
         ▼
    mpsc::Sender<UiEvent>
         │
         ▼
    TUI render loop (consumes UiEvent, mutates AppState, draws frame)
```

## References

- agtrs: `agtrs-runtime/src/signals.rs`
- agtrs: `agtrs-runtime/src/messaging.rs`
- Next: [Workspace Model →](04_workspace_model.md)