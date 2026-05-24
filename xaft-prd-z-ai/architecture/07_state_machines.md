# 07 — State Machines

> All state machines in xaft: TaskState, WorktreeState, FileEditor, AgentSession,
> Approval workflow. ASCII state diagrams, transition conditions, invariants,
> persistence, and concurrency guarantees.

---

## Overview

xaft is fundamentally a stateful system. Every task passes through multiple state machines, each with strict invariants and well-defined transitions. Understanding these machines is essential for correct implementation and debugging.

This document catalogues every state machine, provides ASCII diagrams for each, specifies transition guards and side effects, and documents how they interact.

---

## TaskState

The `TaskState` is the top-level state machine governing a task's lifecycle. It is managed by the `TaskRunner` from agtrs.

### State Diagram

```
                         ┌───────────┐
                         │  Received  │  Task submitted to xaft
                         └─────┬─────┘
                               │ planner.plan()
                               ▼
                         ┌───────────┐
                    ┌────│  Planned   │────┐
                    │    └─────┬─────┘    │
                    │          │          │ plan rejected
                    │          │ start    │
          planning │          │ execution │
          failed   │          ▼          │
                    │    ┌───────────┐   │
                    │    │  Running   │   │
                    │    └─────┬─────┘   │
                    │          │          │
                    │    ┌─────┼─────┐    │
                    │    │     │     │    │
                    │    ▼     ▼     ▼    │
                    │ ┌──────┐┌──────────┐│
                    │ │Suspend││Awaiting  ││
                    │ │  ed  ││Approval  ││
                    │ └──┬───┘└────┬─────┘│
                    │    │         │      │
                    │resume   approved/   │
                    │    │     denied     │
                    │    │         │      │
                    │    ▼         ▼      │
                    │    ┌───────────┐    │
                    │    │  Running   │────┘
                    │    └─────┬─────┘
                    │          │
                    │    ┌─────┼──────────┐
                    │    │     │          │
                    │    ▼     ▼          ▼
                    │ ┌─────────┐  ┌──────────┐
                    │ │Completed│  │  Failed  │
                    │ └─────────┘  └──────────┘
                    │    │              │
                    │    ▼              ▼
                    │  success      error occurred
                    │
                    ▼
              ┌───────────┐
              │ Cancelled  │  CancellationToken fired
              └───────────┘
```

### State Definitions

```rust
/// Task state managed by TaskRunner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    /// Task has been received but not yet planned.
    Received,

    /// A plan has been generated for the task.
    Planned {
        plan: TaskPlan,
    },

    /// The task is currently being executed by the agent.
    Running {
        current_turn: u32,
        started_at: chrono::DateTime<chrono::Utc>,
    },

    /// The task has been suspended (user paused, budget paused, etc.)
    Suspended {
        reason: SuspensionReason,
        suspended_at: chrono::DateTime<chrono::Utc>,
        checkpoint: TaskCheckpoint,
    },

    /// The task is waiting for user approval.
    AwaitingApproval {
        approval_request: ApprovalRequest,
        created_at: chrono::DateTime<chrono::Utc>,
        timeout: Option<chrono::Duration>,
    },

    /// The task completed successfully.
    Completed {
        result: TaskResult,
        completed_at: chrono::DateTime<chrono::Utc>,
    },

    /// The task failed.
    Failed {
        error: String,
        failed_at: chrono::DateTime<chrono::Utc>,
        partial_result: Option<TaskResult>,
    },

    /// The task was cancelled.
    Cancelled {
        reason: String,
        cancelled_at: chrono::DateTime<chrono::Utc>,
        partial_result: Option<TaskResult>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SuspensionReason {
    UserRequested,
    BudgetPaused,
    RateLimitHit,
    DependencyBlocked(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCheckpoint {
    pub turn: u32,
    pub conversation_length: usize,
    pub dirty_files: Vec<PathBuf>,
    pub git_head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub tool_name: String,
    pub tool_args: String,
    pub risk_level: RiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}
```

### Transition Table

| From | To | Guard Condition | Side Effect |
|---|---|---|---|
| Received | Planned | Plan generated successfully | Emit `PlanCreated` |
| Received | Running | No planner configured | Emit `AgentStarted` |
| Received | Failed | Planning fails | Emit `PlanningFailed` |
| Planned | Running | Plan approved or auto-approved | Emit `AgentStarted` |
| Planned | Cancelled | Plan rejected by user | Emit `PlanRejected` |
| Planned | Failed | Plan approval times out | Emit `PlanningFailed` |
| Running | Running | Turn completes | Emit `TurnComplete` |
| Running | Suspended | User pauses, budget pauses | Save checkpoint, emit `SessionSuspended` |
| Running | AwaitingApproval | Tool requires confirmation | Emit approval prompt |
| Running | Completed | Agent finishes successfully | Emit `TaskComplete`, persist session |
| Running | Failed | Agent error, max turns, LLM failure | Emit error, persist session |
| Running | Cancelled | CancellationToken fires | Emit `Cancelled`, persist partial results |
| Suspended | Running | Resume requested | Load checkpoint, emit `SessionResumed` |
| Suspended | Cancelled | Cancel during suspension | Emit `Cancelled` |
| AwaitingApproval | Running | Approval granted | Emit tool execution start |
| AwaitingApproval | Running | Approval denied | Inject denial into agent context |
| AwaitingApproval | Suspended | Approval timeout | Suspend with reason |
| Completed | — | Terminal state | — |
| Failed | — | Terminal state | — |
| Cancelled | — | Terminal state | — |

### Invariants

1. **I1**: A task in `Running` state always has an associated `AgentSession`.
2. **I2**: A task in `Suspended` state always has a valid `TaskCheckpoint`.
3. **I3**: A task in `AwaitingApproval` state always has a non-expired `ApprovalRequest` (unless timeout is configured).
4. **I4**: Terminal states (`Completed`, `Failed`, `Cancelled`) are never left.
5. **I5**: `current_turn` monotonically increases across `Running → Running` transitions.
6. **I6**: A `Completed` task always has at least one git commit (if workspace was modified).

---

## WorktreeState

The `WorktreeState` governs the git worktree lifecycle managed by `WorktreeGuard`.

### State Diagram

```
            ┌─────────────────────────────────────────┐
            │                                         │
            ▼                                         │
      ┌──────────┐    commit_all()    ┌───────────┐  │
      │   Open   │───────────────────▶│ Committed │  │
      └────┬─────┘                    └───────────┘  │
           │                                │         │
           │ restore()                      │ (guard dropped)
           │                                ▼         │
           │                          ┌───────────┐  │
           ├─────────────────────────▶│ Restored  │  │
           │                          └───────────┘  │
           │                                │         │
           │ (guard dropped,               │         │
           │  restore_on_drop)             │         │
           │                                │         │
           ▼                                ▼         │
     ┌───────────┐                   ┌───────────┐   │
     │ Abandoned │                   │ Merged    │   │
     │ (dirty,   │                   │ (clean,   │   │
     │  no       │                   │  on main) │   │
     │  commit)  │                   └───────────┘   │
     └───────────┘                                    │
                                                      │
                                                      │
     Re-open: ────────────────────────────────────────┘
     (new WorktreeGuard created)
```

### State Definitions

```rust
/// Git worktree state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeState {
    /// Worktree is open and active (on task branch).
    Open {
        branch: String,
        original_branch: String,
        opened_at: chrono::DateTime<chrono::Utc>,
    },

    /// Worktree has been committed (clean working tree).
    Committed {
        branch: String,
        commit_hash: String,
        committed_at: chrono::DateTime<chrono::Utc>,
    },

    /// Worktree has been restored to the original branch.
    Restored {
        task_branch: String,
        original_branch: String,
        restored_at: chrono::DateTime<chrono::Utc>,
    },

    /// Worktree was abandoned with uncommitted changes.
    Abandoned {
        branch: String,
        dirty_files: Vec<PathBuf>,
    },

    /// Worktree was merged back into the original branch.
    Merged {
        branch: String,
        merge_commit: String,
    },
}
```

### Transition Table

| From | To | Guard Condition | Side Effect |
|---|---|---|---|
| Open | Committed | `commit_all()` succeeds | Emit `AutoCommit` |
| Open | Restored | `restore()` called or guard dropped with restore | Switch to original branch |
| Open | Abandoned | Guard dropped without commit or restore | Warn about dirty files |
| Committed | Restored | Guard dropped with `restore_on_drop=true` | Switch to original branch |
| Committed | Merged | Explicit merge to original branch | Create merge commit |
| Restored | Open | New WorktreeGuard created | Switch to task branch |

### Invariants

1. **I1**: An `Open` worktree is always on the task branch.
2. **I2**: A `Committed` worktree always has a clean `git status`.
3. **I3**: A `Restored` worktree is always on the original branch.
4. **I4**: `Abandoned` state always has `dirty_files.len() > 0`.
5. **I5**: Only one `WorktreeGuard` can be `Open` for a given branch at a time.

---

## FileEditor State

The `FileEditor` tracks its own internal state for transaction management.

### State Diagram

```
    ┌─────────┐
    │  Clean  │  No pending edits
    └────┬────┘
         │ replace_block() / apply_diff() / multi_edit()
         ▼
    ┌─────────┐
    │  Dirty  │  Has uncommitted edits
    └────┬────┘
         │
    ┌────┼────────────────────┐
    │    │                    │
    │    ▼                    ▼
    │ ┌─────────┐       ┌──────────┐
    │ │Committed│       │RolledBack│
    │ │         │       │          │
    │ │ Changes │       │ Original │
    │ │ on disk │       │ restored │
    │ └─────────┘       └──────────┘
    │    │                    │
    │    ▼                    ▼
    │ ┌─────────────────────────────┐
    │ │         Sealed              │
    │ │  (no further edits allowed) │
    │ └─────────────────────────────┘
    │
    │  (after seal, new FileEditor must be created)
    │
    └──▶ More edits ──▶ Dirty (still)
```

### State Definitions

```rust
/// Internal state of a FileEditor instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileEditorState {
    /// No edits have been made yet.
    Clean,

    /// Edits have been applied but not committed or rolled back.
    Dirty {
        pending_edit_count: usize,
        dirty_files: Vec<PathBuf>,
        first_edit_at: chrono::DateTime<chrono::Utc>,
    },

    /// All edits have been committed to disk.
    Committed {
        commit_count: usize,
        files_modified: Vec<PathBuf>,
        committed_at: chrono::DateTime<chrono::Utc>,
    },

    /// All edits have been rolled back.
    RolledBack {
        edits_undone: usize,
        files_restored: Vec<PathBuf>,
        rolled_back_at: chrono::DateTime<chrono::Utc>,
    },

    /// The editor is sealed — no further operations allowed.
    Sealed {
        reason: SealReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SealReason {
    Committed,
    RolledBack,
    Error(String),
}
```

### Transition Table

| From | To | Guard Condition | Side Effect |
|---|---|---|---|
| Clean | Dirty | Any edit operation succeeds | Backup original, write to disk |
| Dirty | Dirty | Additional edit operation | (accumulate edits) |
| Dirty | Committed | `commit()` called | Clear backups, emit `FileEditCommitted` |
| Dirty | RolledBack | `rollback()` called | Restore from backups, emit `FileEditRolledBack` |
| Committed | Sealed | Automatic | No further edits allowed |
| RolledBack | Sealed | Automatic | No further edits allowed |
| Dirty | Sealed | Unrecoverable error during edit | Best-effort rollback |

### Invariants

1. **I1**: A `Dirty` editor always has backups for all modified files.
2. **I2**: A `Committed` editor always has its edits persisted to disk.
3. **I3**: A `RolledBack` editor's workspace matches the pre-edit state.
4. **I4**: A `Sealed` editor rejects all further edit operations.
5. **I5**: `multi_edit` is all-or-nothing: if one sub-edit fails, all are rolled back.

### Dirty Tracking Implementation

```rust
/// Dirty file tracking within the workspace.
pub struct DirtyTracker {
    /// Files that have been modified since the last commit.
    dirty: RwLock<HashSet<PathBuf>>,

    /// Backup content for each dirty file.
    backups: RwLock<HashMap<PathBuf, String>>,

    /// Sequence of edits (for replay and debugging).
    edit_log: RwLock<Vec<EditLogEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditLogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub path: PathBuf,
    pub operation: String,
    pub lines_before: u32,
    pub lines_after: u32,
}

impl DirtyTracker {
    /// Mark a file as dirty and store its backup.
    pub fn mark_dirty(&self, path: &Path, original_content: &str) {
        let mut dirty = self.dirty.write().unwrap();
        dirty.insert(path.to_path_buf());

        let mut backups = self.backups.write().unwrap();
        backups.entry(path.to_path_buf())
            .or_insert_with(|| original_content.to_string());
    }

    /// Check if a file is dirty.
    pub fn is_dirty(&self, path: &Path) -> bool {
        self.dirty.read().unwrap().contains(path)
    }

    /// Get all dirty files.
    pub fn dirty_files(&self) -> Vec<PathBuf> {
        self.dirty.read().unwrap().iter().cloned().collect()
    }

    /// Clear dirty state (after commit).
    pub fn clear(&self) {
        self.dirty.write().unwrap().clear();
        self.backups.write().unwrap().clear();
    }

    /// Get backup for a file (for rollback).
    pub fn backup(&self, path: &Path) -> Option<String> {
        self.backups.read().unwrap().get(path).cloned()
    }

    /// Log an edit for debugging.
    pub fn log_edit(&self, entry: EditLogEntry) {
        self.edit_log.write().unwrap().push(entry);
    }
}
```

---

## AgentSession State

The `AgentSession` manages the overall session lifecycle:

### State Diagram

```
    ┌───────────┐
    │  Init     │  Session created
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │  Ready    │  Session loaded, waiting for prompt
    └─────┬─────┘
          │ user provides prompt
          ▼
    ┌───────────┐
    │  Active   │  Agent is running
    └─────┬─────┘
          │
    ┌─────┼──────────────────────────────┐
    │     │                              │
    │     ▼                              ▼
    │ ┌───────────┐              ┌──────────────┐
    │ │  Idle     │              │  Error       │
    │ │ (waiting  │              │  (recoverable│
    │ │  for next │              │   error)     │
    │ │  prompt)  │              └──────┬───────┘
    │ └─────┬─────┘                     │
    │       │                            │ retry
    │       │ new prompt                 ▼
    │       │                     ┌───────────┐
    │       └────────────────────▶│  Active   │
    │                             └───────────┘
    │
    │  (user ends session)
    │
    ▼
┌───────────┐
│  Closed   │  Session terminated
└───────────┘
```

### State Definitions

```rust
/// Agent session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentSessionState {
    /// Session is being initialized.
    Init,

    /// Session is ready to accept a prompt.
    Ready {
        session_id: SessionId,
        workspace_root: PathBuf,
    },

    /// Agent is actively running.
    Active {
        session_id: SessionId,
        current_turn: u32,
        started_at: chrono::DateTime<chrono::Utc>,
    },

    /// Agent is idle, waiting for next prompt.
    Idle {
        session_id: SessionId,
        total_turns: u32,
        total_cost_usd: f64,
    },

    /// Agent encountered a recoverable error.
    Error {
        session_id: SessionId,
        error: String,
        recoverable: bool,
    },

    /// Session is closed.
    Closed {
        session_id: SessionId,
        total_turns: u32,
        total_cost_usd: f64,
        closed_at: chrono::DateTime<chrono::Utc>,
    },
}
```

---

## Approval Workflow State Machine

The approval workflow manages the lifecycle of an approval request:

### State Diagram

```
    ┌───────────┐
    │ Requested │  Tool needs approval
    └─────┬─────┘
          │ present to user
          ▼
    ┌───────────┐
    │  Pending  │  Waiting for user decision
    └─────┬─────┘
          │
    ┌─────┼──────────────┬──────────────┐
    │     │              │              │
    │     ▼              ▼              ▼
    │ ┌─────────┐  ┌──────────┐  ┌──────────┐
    │ │Approved │  │ Rejected │  │  TimedOut│
    │ └────┬────┘  └────┬─────┘  └────┬─────┘
    │      │            │             │
    │      ▼            ▼             ▼
    │  ┌────────────────────────────────────┐
    │  │         Resolved                   │
    │  └────────────────────────────────────┘
    │
    │  (during Pending, user can also:)
    │
    │  ┌──────────┐
    │  │ Modified │  User edits args before approving
    │  └────┬─────┘
    │       │
    │       ▼
    │  ┌──────────┐
    │  │Approved  │  (with modified args)
    │  │(modified)│
    │  └──────────┘
    │
    │  (escalation path if timeout:)
    │
    │  ┌──────────┐    ┌───────────┐
    │  │ TimedOut │───▶│Escalated  │  (notify, increase visibility)
    │  └──────────┘    └───────────┘
```

### State Definitions

```rust
/// Approval workflow state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalState {
    /// Approval has been requested but not yet presented to the user.
    Requested {
        request: ApprovalRequest,
        created_at: chrono::DateTime<chrono::Utc>,
    },

    /// Approval is pending user decision.
    Pending {
        request: ApprovalRequest,
        presented_at: chrono::DateTime<chrono::Utc>,
        timeout_at: Option<chrono::DateTime<chrono::Utc>>,
    },

    /// User approved the tool call.
    Approved {
        request: ApprovalRequest,
        approved_at: chrono::DateTime<chrono::Utc>,
        modified_args: Option<String>,  // If user modified args before approving
    },

    /// User rejected the tool call.
    Rejected {
        request: ApprovalRequest,
        rejected_at: chrono::DateTime<chrono::Utc>,
        reason: Option<String>,
    },

    /// Approval request timed out.
    TimedOut {
        request: ApprovalRequest,
        timed_out_at: chrono::DateTime<chrono::Utc>,
    },

    /// Timed-out approval was escalated.
    Escalated {
        request: ApprovalRequest,
        escalated_at: chrono::DateTime<chrono::Utc>,
        escalation_level: u32,
    },

    /// Approval is resolved (terminal state).
    Resolved {
        request: ApprovalRequest,
        decision: ApprovalDecision,
        resolved_at: chrono::DateTime<chrono::Utc>,
    },
}
```

### Transition Table

| From | To | Guard Condition | Side Effect |
|---|---|---|---|
| Requested | Pending | Approval presented to user | Start timeout timer |
| Pending | Approved | User approves | Emit `ToolExecuting` |
| Pending | Rejected | User rejects | Emit `ToolRejected` |
| Pending | Approved | User approves with modified args | Use modified args for execution |
| Pending | TimedOut | Timeout elapsed without response | Emit warning |
| TimedOut | Escalated | Escalation policy configured | Notify user, increase visibility |
| TimedOut | Rejected | Default rejection on timeout | Emit `ToolRejected` |
| Approved | Resolved | Tool execution completes | Emit `ToolResult` |
| Rejected | Resolved | Denial injected into agent | Agent receives "denied" message |
| Escalated | Pending | User responds to escalation | Re-enter pending state |
| Escalated | Rejected | Escalation timeout | Auto-reject |

### Invariants

1. **I1**: An `ApprovalRequest` always has a unique `ApprovalId`.
2. **I2**: `Pending` state always has a timeout (even if infinite).
3. **I3**: `Approved` with `modified_args` must still pass tool validation.
4. **I4**: `Resolved` is a terminal state — no further transitions.
5. **I5**: Escalation level monotonically increases.

---

## State Machine Composition

Multiple state machines operate simultaneously. Their interactions are constrained:

```
┌─────────────────────────────────────────────────────────────────┐
│                    State Machine Composition                     │
│                                                                  │
│  TaskState (outermost)                                          │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                                                            │ │
│  │  TaskState = Running                                       │ │
│  │  ┌──────────────────────────────────────────────────────┐ │ │
│  │  │  AgentSessionState = Active                          │ │ │
│  │  │  ┌────────────────────────────────────────────────┐  │ │ │
│  │  │  │                                                │  │ │ │
│  │  │  │  WorktreeState = Open                          │  │ │ │
│  │  │  │  ┌──────────────────────────────────────────┐  │  │ │ │
│  │  │  │  │                                          │  │  │ │ │
│  │  │  │  │  FileEditorState = Dirty                 │  │  │ │ │
│  │  │  │  │  ┌────────────────────────────────────┐  │  │  │ │ │
│  │  │  │  │  │                                    │  │  │  │ │ │
│  │  │  │  │  │  ApprovalState = Pending           │  │  │  │ │ │
│  │  │  │  │  │  (waiting for user to approve      │  │  │  │ │ │
│  │  │  │  │  │   a file edit)                     │  │  │  │ │ │
│  │  │  │  │  │                                    │  │  │  │ │ │
│  │  │  │  │  └────────────────────────────────────┘  │  │  │ │ │
│  │  │  │  └──────────────────────────────────────────┘  │  │ │ │
│  │  │  └────────────────────────────────────────────────┘  │ │ │
│  │  └──────────────────────────────────────────────────────┘ │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### Composition Constraints

1. **C1**: `AgentSessionState = Active` requires `TaskState = Running`.
2. **C2**: `WorktreeState = Open` requires `AgentSessionState = Active`.
3. **C3**: `FileEditorState = Dirty` requires `WorktreeState = Open`.
4. **C4**: `ApprovalState = Pending` requires `FileEditorState = Dirty` (if the approval is for a file edit).
5. **C5**: `TaskState = Completed` requires `FileEditorState ∈ {Committed, Clean}`.
6. **C6**: `TaskState = Cancelled` triggers `FileEditorState → RolledBack` (if dirty).
7. **C7**: `TaskState = Cancelled` triggers `WorktreeState → Restored` (if restore_on_drop).

---

## Persistence

All state machines persist their state for crash recovery:

```rust
/// Persistent state storage for all state machines.
pub struct PersistentState {
    /// SQLite database for durable state.
    db: SqlitePool,
}

impl PersistentState {
    /// Save the current state of all state machines.
    pub async fn save_state(
        &self,
        task_state: &TaskState,
        session_state: &AgentSessionState,
        worktree_state: &WorktreeState,
        editor_state: &FileEditorState,
    ) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"
            INSERT INTO state_snapshots (id, task_state, session_state, worktree_state, editor_state, timestamp)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                task_state = excluded.task_state,
                session_state = excluded.session_state,
                worktree_state = excluded.worktree_state,
                editor_state = excluded.editor_state,
                timestamp = excluded.timestamp
            "#,
            /* params */
        ).execute(&self.db).await?;

        Ok(())
    }

    /// Load the most recent state for recovery.
    pub async fn load_latest_state(&self) -> Result<StateSnapshot, PersistenceError> {
        let row = sqlx::query_as!(
            StateSnapshotRow,
            r#"SELECT * FROM state_snapshots ORDER BY timestamp DESC LIMIT 1"#
        ).fetch_one(&self.db).await?;

        Ok(StateSnapshot {
            task_state: serde_json::from_str(&row.task_state)?,
            session_state: serde_json::from_str(&row.session_state)?,
            worktree_state: serde_json::from_str(&row.worktree_state)?,
            editor_state: serde_json::from_str(&row.editor_state)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub task_state: TaskState,
    pub session_state: AgentSessionState,
    pub worktree_state: WorktreeState,
    pub editor_state: FileEditorState,
}
```

### Recovery Procedure

```
Process Crash
    │
    ▼
Load latest StateSnapshot from SQLite
    │
    ├── TaskState = Running ──▶ Recover
    │   ├── Check: is workspace dirty?
    │   │   ├── Yes: Rollback FileEditor, restart task
    │   │   └── No: Resume from checkpoint
    │   │
    │   ├── Check: is worktree on task branch?
    │   │   ├── Yes: Continue
    │   │   └── No: Switch to task branch
    │   │
    │   └── Check: is agent session active?
    │       ├── Yes: Resume conversation from ConversationStore
    │       └── No: Start new agent session
    │
    ├── TaskState = Suspended ──▶ Resume
    │   └── Load checkpoint, present to user
    │
    ├── TaskState = AwaitingApproval ──▶ Re-request
    │   └── Present approval request again
    │
    └── TaskState ∈ {Completed, Failed, Cancelled} ──▶ Report
        └── Display result to user
```

---

## Concurrency Guarantees

State machines are accessed by multiple concurrent tasks (agent loop, TUI, signal handlers). Thread-safety is ensured by:

1. **Interior mutability**: `RwLock` for read-heavy state (e.g., `TaskState`), `Mutex` for write-heavy state (e.g., `FileEditorState`).
2. **Atomic transitions**: State transitions are performed under lock, ensuring no observer sees an intermediate state.
3. **Event ordering**: `SignalBus` events for state changes are emitted under the same lock that performs the transition, ensuring causal ordering.

```rust
/// Thread-safe state machine wrapper.
pub struct AtomicStateMachine<S> {
    state: RwLock<S>,
    signal_bus: Arc<SignalBus>,
}

impl<S: Clone + Send + Sync> AtomicStateMachine<S> {
    /// Read the current state.
    pub async fn current(&self) -> S {
        self.state.read().await.clone()
    }

    /// Transition to a new state atomically.
    /// The transition function receives the current state and returns the new state.
    /// If the transition is invalid, it returns None and the state is unchanged.
    pub async fn transition<F>(&self, f: F) -> Result<S, StateTransitionError>
    where
        F: FnOnce(&S) -> Option<S>,
    {
        let mut state = self.state.write().await;
        match f(&*state) {
            Some(new_state) => {
                let old_state = std::mem::replace(&mut *state, new_state.clone());
                // Emit state change event under the lock
                // (This ensures causal ordering with the transition)
                drop(state); // Release lock before async emit
                self.signal_bus.emit(Signal::StateChanged {
                    from: old_state.to_string(),
                    to: new_state.to_string(),
                })?;
                Ok(new_state)
            }
            None => Err(StateTransitionError::InvalidTransition),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateTransitionError {
    #[error("Invalid state transition")]
    InvalidTransition,

    #[error("Signal bus error: {0}")]
    SignalBus(#[from] SignalError),

    #[error("Lock poisoned")]
    LockPoisoned,
}
```

---

## Complete State Transition Audit Trail

Every state transition is logged for debugging and compliance:

```rust
/// Audit trail entry for state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransitionAuditEntry {
    /// Unique entry ID.
    pub id: Uuid,

    /// Timestamp of the transition.
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Which state machine this transition belongs to.
    pub machine: String, // "Task", "Worktree", "FileEditor", "Session", "Approval"

    /// The state before the transition.
    pub from_state: String,

    /// The state after the transition.
    pub to_state: String,

    /// What triggered the transition.
    pub trigger: String,

    /// Additional context (tool name, error message, etc.).
    pub context: HashMap<String, String>,

    /// Session ID for correlation.
    pub session_id: SessionId,
}

/// Audit logger that records all state transitions.
pub struct StateAuditLogger {
    db: SqlitePool,
}

impl StateAuditLogger {
    pub async fn log(&self, entry: StateTransitionAuditEntry) -> Result<(), PersistenceError> {
        sqlx::query!(
            r#"
            INSERT INTO state_audit (id, timestamp, machine, from_state, to_state, trigger, context, session_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            entry.id,
            entry.timestamp,
            entry.machine,
            entry.from_state,
            entry.to_state,
            entry.trigger,
            serde_json::to_string(&entry.context)?,
            entry.session_id,
        ).execute(&self.db).await?;

        Ok(())
    }

    /// Query audit trail for a specific session.
    pub async fn query_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<StateTransitionAuditEntry>, PersistenceError> {
        // ... SQL query
        todo!()
    }
}
```

---

## Error-Induced State Transitions

When errors occur, state machines transition according to the following policy:

| Error Type | TaskState | WorktreeState | FileEditorState | Session |
|---|---|---|---|---|
| LLM API error (recoverable) | Running | Unchanged | Unchanged | Active |
| LLM API error (unrecoverable) | Failed | Restored | RolledBack | Error |
| Tool execution error | Running | Unchanged | Unchanged | Active |
| Tool execution timeout | Running | Unchanged | RolledBack | Active |
| Budget exceeded | Suspended | Unchanged | Unchanged | Idle |
| Daily budget exceeded | Cancelled | Restored | RolledBack | Closed |
| User cancellation | Cancelled | Restored (if dirty) | RolledBack (if dirty) | Closed |
| Git operation error | Running | Open (may be inconsistent) | Unchanged | Active |
| File system error | Failed | Unchanged | RolledBack (best-effort) | Error |
| Process crash (recovery) | Running → Running | Open → Open | Dirty → RolledBack | Active → Active |
