# State Machines

## Three Core State Machines

`xaft` manages three primary state machines, each with well-defined transitions and invariants.

## 1. Session State Machine

A session is the top-level container for a user's interaction with `xaft`.

```
Initial
    │ xaft run <goal>
    ▼
Initializing
    │ Config loaded, DI container built, TUI started
    ▼
Planning
    │ PlannerAgent decomposes intent
    ▼
Executing ─────────────────────── ► Suspended
    │ PlanExecutor runs steps                │
    │                         user: xaft suspend
    │                                        │
    │◄────────────────────── xaft resume ────┘
    │
    ├── step fails → Recovering (FixerAgent or replan)
    │       │
    │       ├── recovery succeeds → Executing
    │       └── recovery fails (max iterations) → Failed
    │
    ├── approval rejected → Cancelled
    │
    ├── Ctrl-C → Cancelling
    │       │ save checkpoint, remove worktree
    │       ▼
    │   Cancelled
    │
    └── all steps complete → Completing
            │ stage, commit, cleanup worktree
            ▼
        Complete
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionState {
    Initial,
    Initializing,
    Planning { intent: Intent },
    Executing { task_id: Uuid, current_step: usize, total_steps: usize },
    Recovering { task_id: Uuid, failed_step: String, attempt: u32 },
    Suspended { task_id: Uuid, checkpoint: CheckpointId, reason: String },
    Cancelling { reason: String },
    Completing { worktree: PathBuf, commit_sha: Option<String> },
    Complete { summary: SessionSummary },
    Failed { reason: String, last_checkpoint: Option<CheckpointId> },
    Cancelled { reason: String },
}

impl SessionState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. } | Self::Cancelled { .. })
    }

    pub fn can_suspend(&self) -> bool {
        matches!(self, Self::Executing { .. } | Self::Recovering { .. })
    }

    pub fn can_resume(&self) -> bool {
        matches!(self, Self::Suspended { .. })
    }
}
```

## 2. Task State Machine (from agtrs-runtime)

Individual tasks managed by `TaskRunner`:

```
Received
    │ TaskRunner::submit(intent)
    ▼
Planned
    │ PlannerAgent produces Plan
    ▼
Running ──────────────────────── ► Suspended
    │ PlanExecutor executing steps         │
    │                     runner.suspend()  │
    │◄──────────────────── runner.resume() ─┘
    │
    ├── step fails → AwaitingApproval (if risk=High)
    │       │
    │       ├── approved → Running
    │       └── rejected → Failed
    │
    ├── step fails → Failed (if not approval-gated)
    │       │ runner.retry()
    │       └── Running (from last checkpoint)
    │
    ├── runner.cancel() → Cancelled
    │
    └── all steps done → Completed
```

```rust
// From agtrs-runtime/src/task.rs
pub enum TaskState {
    Received,
    Planned { plan: Plan },
    Running { current_step: usize },
    Suspended { reason: String, checkpoint: Checkpoint },
    AwaitingApproval { step: PlanStep, approval_id: Uuid },
    Failed { step_id: String, reason: String, checkpoint: Option<Checkpoint> },
    Completed { result: AgentResponse },
    Cancelled { reason: String },
}
```

## 3. Tool Approval State Machine

For high-risk tools that require human approval:

```
ToolCallInitiated (risk=High)
    │ executor detects risk level from tool metadata
    ▼
ApprovalPending
    │ ToolPendingApproval signal emitted
    │ TUI shows approval dialog
    │
    ├── timeout (30s default) → ApprovalTimedOut → tool rejected
    │
    ├── user approves → Approved
    │       │ executor continues with tool call
    │       ▼
    │   ToolExecuting → ToolComplete
    │
    └── user rejects → Rejected
            │ executor returns ToolCallRejected error
            ▼
        ToolRejected → agent receives error message
```

```rust
pub enum ApprovalState {
    Pending {
        tool_name: String,
        input: serde_json::Value,
        risk_level: RiskLevel,
        deadline: Instant,
        tx: oneshot::Sender<bool>,
    },
    Approved,
    Rejected { reason: Option<String> },
    TimedOut,
}

pub struct ApprovalGate {
    pending: Arc<Mutex<Option<ApprovalState>>>,
    approval_timeout: Duration,
}

impl ApprovalGate {
    pub async fn request(&self, tool_name: &str, input: &serde_json::Value, risk: RiskLevel)
        -> Result<bool, XaftError>
    {
        let (tx, rx) = oneshot::channel();
        *self.pending.lock().await = Some(ApprovalState::Pending {
            tool_name: tool_name.to_string(),
            input: input.clone(),
            risk_level: risk,
            deadline: Instant::now() + self.approval_timeout,
            tx,
        });

        tokio::time::timeout(self.approval_timeout, rx)
            .await
            .map(|r| r.unwrap_or(false))
            .map_err(|_| {
                *self.pending.try_lock().map(|mut g| { g.take(); g }).ok();
                XaftError::Cancelled { reason: "approval timed out".into() }
            })
    }

    pub async fn respond(&self, approved: bool, reason: Option<String>) {
        if let Some(ApprovalState::Pending { tx, .. }) = self.pending.lock().await.take() {
            tx.send(approved).ok();
        }
    }
}
```

## Checkpoint State

Checkpoints enable session recovery after crash or suspension:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub checkpoint_id: Uuid,
    pub task_id: Uuid,
    pub session_id: Uuid,
    pub step_index: usize,
    pub completed_steps: Vec<CompletedStep>,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
    pub conversation_snapshot: Vec<Message>,
    pub context_state: HashMap<String, serde_json::Value>,
    pub saved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedStep {
    pub step_id: String,
    pub step_description: String,
    pub agent_name: String,
    pub modified_files: Vec<PathBuf>,
    pub commit_sha: Option<String>,
    pub duration_ms: f64,
    pub cost_usd: f64,
    pub completed_at: DateTime<Utc>,
}
```

## State Transition Validation

```rust
impl SessionState {
    /// Returns Err if the transition is invalid.
    pub fn transition(&self, next: &SessionState) -> Result<(), XaftError> {
        match (self, next) {
            (Self::Planning { .. }, Self::Executing { .. }) => Ok(()),
            (Self::Executing { .. }, Self::Recovering { .. }) => Ok(()),
            (Self::Executing { .. }, Self::Suspended { .. }) => Ok(()),
            (Self::Executing { .. }, Self::Completing { .. }) => Ok(()),
            (Self::Executing { .. }, Self::Cancelling { .. }) => Ok(()),
            (Self::Recovering { .. }, Self::Executing { .. }) => Ok(()),
            (Self::Recovering { .. }, Self::Failed { .. }) => Ok(()),
            (Self::Suspended { .. }, Self::Executing { .. }) => Ok(()),
            (Self::Cancelling { .. }, Self::Cancelled { .. }) => Ok(()),
            (Self::Completing { .. }, Self::Complete { .. }) => Ok(()),
            _ => Err(XaftError::Session(format!(
                "invalid transition: {:?} → {:?}", self, next
            ))),
        }
    }
}
```

## TUI State Synchronization

The TUI's `AppState` is a projection of the `SessionState` + real-time streaming events:

```rust
pub struct AppState {
    // Derived from SessionState
    pub session_state: SessionState,
    pub task_id: Option<Uuid>,
    pub plan_steps: Vec<PlanStepUi>,
    pub current_step_idx: Option<usize>,

    // Live streaming data
    pub agent_outputs: Vec<AgentPaneState>,
    pub selected_agent: usize,
    pub active_tool: Option<ActiveToolState>,

    // Metrics
    pub session_cost: f64,
    pub task_cost: f64,
    pub total_tokens: usize,
    pub elapsed_secs: f64,

    // Approval dialog
    pub pending_approval: Option<ApprovalDialogState>,

    // Log console
    pub log_lines: VecDeque<LogLine>,

    // Keyboard state
    pub active_pane: Pane,
    pub scroll_offsets: HashMap<Pane, usize>,
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs` (TaskState enum)
- agtrs: `agtrs-runtime/src/agent.rs` (on_start, on_complete hooks)
- Next: [Crate Organization →](08_crate_organization.md)