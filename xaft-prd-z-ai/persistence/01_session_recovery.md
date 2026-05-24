# XAFT Session Recovery — Product Requirements Document

> **Status**: Draft v0.1  
> **Last Updated**: 2025-03-04  
> **Authors**: xaft core team  
> **Scope**: TaskStore, ConversationStore, FileEditor staged state, WorktreeGuard state, crash recovery, checkpoint-based resumption, session file format, TUI reconnection

---

## 1. Overview

xaft agents perform long-running, multi-step tasks that may involve editing dozens of files, running shell commands, and maintaining extended LLM conversations. A crash, network failure, or accidental termination must not lose hours of work. This PRD defines the session persistence and recovery system that enables xaft to resume from the last consistent checkpoint.

### 1.1 Goals

| # | Goal | Metric |
|---|------|--------|
| G1 | Crash recovery with zero data loss | All committed file edits preserved; no partial writes |
| G2 | Sub-second checkpoint writes | Non-blocking, append-only log with periodic compaction |
| G3 | Sub-5-second session resumption | Full state reconstruction from latest checkpoint |
| G4 | Granular rollback to any checkpoint | Agent can undo to any prior consistent state |
| G5 | TUI reconnection after terminal disconnect | Reconnect without losing in-progress agent turns |

### 1.2 Non-Goals

- Distributed session storage (single-machine only in v1)
- Real-time session replication across machines
- Branching/forking sessions into parallel timelines
- Session migration between different xaft versions (forward-only)

---

## 2. Architecture

### 2.1 Session Storage Stack

```
┌─────────────────────────────────────────────────────────────┐
│                      XAFT TUI / CLI                         │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    Session Manager                           │
│  ┌─────────────┐ ┌──────────────┐ ┌──────────────────────┐ │
│  │ TaskStore   │ │ Conversation │ │  CheckpointManager   │ │
│  │             │ │ Store        │ │                      │ │
│  └──────┬──────┘ └──────┬───────┘ └──────────┬───────────┘ │
│         │               │                     │             │
│         ▼               ▼                     ▼             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              Write-Ahead Log (WAL)                    │   │
│  │  ┌─────┐ ┌─────┐ ┌─────┐ ┌─────┐ ┌─────┐           │   │
│  │  │ 0   │ │ 1   │ │ 2   │ │ 3   │ │ 4   │  ...      │   │
│  │  │ txn │ │ txn │ │ txn │ │ txn │ │ chk │           │   │
│  │  └─────┘ └─────┘ └─────┘ └─────┘ └─────┘           │   │
│  └──────────────────────────────────────────────────────┘   │
│                           │                                 │
│                           ▼                                 │
│  ┌──────────────────────────────────────────────────────┐   │
│  │           Compacted Snapshot (periodic)               │   │
│  │  session-<id>-snapshot.bincode                        │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼
                  ~/.xaft/sessions/<id>/
```

### 2.2 State Components

Each xaft session comprises four independent but coordinated state stores:

```
┌──────────────────────────────────────────────────────────────┐
│                     SESSION STATE                             │
│                                                              │
│  ┌────────────┐  ┌────────────────┐  ┌───────────────────┐  │
│  │ TaskStore  │  │ Conversation   │  │ FileEditor       │  │
│  │            │  │ Store          │  │ Staged State     │  │
│  │ - task DAG │  │ - messages[]   │  │ - pending edits  │  │
│  │ - status   │  │ - tool calls   │  │ - file versions  │  │
│  │ - results  │  │ - token counts │  │ - undo stack     │  │
│  └────────────┘  └────────────────┘  └───────────────────┘  │
│                                                              │
│  ┌────────────────┐  ┌────────────────────────────────────┐  │
│  │ WorktreeGuard  │  │ Agent Runtime State                │  │
│  │ State          │  │ - current agent ID                 │  │
│  │                │  │ - turn counter                     │  │
│  │ - lock status  │  │ - reasoning buffer                 │  │
│  │ - branch info  │  │ - pending tool calls               │  │
│  │ - dirty files  │  │ - guardrail decisions              │  │
│  └────────────────┘  └────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

---

## 3. TaskStore

### 3.1 Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub description: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub result: Option<TaskResult>,
    pub children: Vec<TaskId>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running { started_at: DateTime<Utc> },
    Completed { completed_at: DateTime<Utc> },
    Failed { error: String, failed_at: DateTime<Utc> },
    Cancelled { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub summary: String,
    pub files_modified: Vec<PathBuf>,
    pub commands_run: Vec<String>,
    pub subtask_ids: Vec<TaskId>,
}
```

### 3.2 Task DAG Structure

```
          Task A (root)
         /    |    \
    Task B  Task C  Task D
      |       |       |
    Task E    |     Task F
              |
            Task G

  Parent → Child relationships form a DAG.
  Each task can fan out to subtasks.
  Completion propagates upward.
```

### 3.3 TaskStore Implementation

```rust
pub struct TaskStore {
    tasks: HashMap<TaskId, Task>,
    wal: Arc<WalWriter>,
    root_id: TaskId,
}

impl TaskStore {
    pub fn create_task(&mut self, description: String, parent: Option<TaskId>) -> TaskId {
        let id = TaskId::new();
        let task = Task {
            id,
            parent_id: parent,
            description,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            result: None,
            children: vec![],
            metadata: serde_json::Value::Null,
        };

        // Append to WAL before mutating in-memory state
        self.wal.append(WalEntry::TaskCreated { task: task.clone() }).expect("WAL write failed");

        if let Some(parent_id) = parent {
            self.tasks.get_mut(&parent_id).unwrap().children.push(id);
        }

        self.tasks.insert(id, task);
        id
    }

    pub fn transition(&mut self, id: TaskId, new_status: TaskStatus) -> Result<(), StoreError> {
        let task = self.tasks.get_mut(&id).ok_or(StoreError::TaskNotFound(id))?;

        // Validate transition
        Self::validate_transition(&task.status, &new_status)?;

        self.wal.append(WalEntry::TaskStatusChanged {
            task_id: id,
            from: task.status.clone(),
            to: new_status.clone(),
        })?;

        task.status = new_status;
        task.updated_at = Utc::now();
        Ok(())
    }

    fn validate_transition(from: &TaskStatus, to: &TaskStatus) -> Result<(), StoreError> {
        match (from, to) {
            (Pending, Running { .. }) => Ok(()),
            (Running { .. }, Completed { .. }) => Ok(()),
            (Running { .. }, Failed { .. }) => Ok(()),
            (Pending | Running { .. }, Cancelled { .. }) => Ok(()),
            _ => Err(StoreError::InvalidTransition),
        }
    }
}
```

---

## 4. ConversationStore

### 4.1 Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub session_id: SessionId,
    pub messages: Vec<Message>,
    pub token_counts: TokenCounts,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub content: Content,
    pub timestamp: DateTime<Utc>,
    pub metadata: MessageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,       // Tool result message
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    ToolCall {
        call_id: String,
        tool_id: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
    Multi(Vec<Content>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCounts {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
```

### 4.2 Append-Only Storage

The conversation is append-only: messages are never modified after writing. This enables efficient WAL operations:

```rust
pub struct ConversationStore {
    conversation: Conversation,
    wal: Arc<WalWriter>,
}

impl ConversationStore {
    pub fn append_message(&mut self, message: Message) -> Result<(), StoreError> {
        // Update token counts before WAL write
        self.conversation.token_counts.total_tokens += message.metadata.token_count.unwrap_or(0);

        self.wal.append(WalEntry::MessageAppended {
            conversation_id: self.conversation.id,
            message: message.clone(),
        })?;

        self.conversation.messages.push(message);
        Ok(())
    }

    /// Truncate the conversation to a specific message index (for undo).
    /// This creates a WAL entry but does NOT delete — it records a truncation point.
    pub fn truncate_to(&mut self, message_idx: usize) -> Result<(), StoreError> {
        let removed: Vec<Message> = self.conversation.messages.drain(message_idx..).collect();

        self.wal.append(WalEntry::ConversationTruncated {
            conversation_id: self.conversation.id,
            removed_count: removed.len(),
            removed_ids: removed.iter().map(|m| m.id).collect(),
        })?;

        // Recalculate token counts
        self.conversation.token_counts = self.recalculate_tokens();
        Ok(())
    }
}
```

---

## 5. FileEditor Staged State

### 5.1 Problem

xaft agents edit files incrementally. A crash mid-edit must not leave files in a partially modified state. The FileEditor uses a **staged state** model: edits accumulate in memory and are atomically committed to disk at checkpoint boundaries.

### 5.2 Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                     FileEditor Staged State                      │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │                   Staged Edits (in-memory)                 │  │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐       │  │
│  │  │ file_a.rs   │  │ file_b.toml │  │ file_c.md   │       │  │
│  │  │ patch 1     │  │ patch 1     │  │ patch 1     │       │  │
│  │  │ patch 2     │  │             │  │ patch 2     │       │  │
│  │  │ patch 3     │  │             │  │ patch 3     │       │  │
│  │  └─────────────┘  └─────────────┘  └─────────────┘       │  │
│  └──────────────────────────┬─────────────────────────────────┘  │
│                             │ checkpoint_commit()               │
│                             ▼                                    │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │              Committed Files (on disk)                      │  │
│  │  file_a.rs  →  committed version with patches 1-3 applied  │  │
│  │  file_b.toml → committed version with patch 1 applied      │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │              Undo Stack (per file)                          │  │
│  │  file_a.rs  →  [original, after-p1, after-p1-p2]          │  │
│  │  file_c.md  →  [original, after-p1, after-p1-p2]          │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### 5.3 Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFileState {
    pub path: PathBuf,
    pub original_content: Option<String>,    // Content before any edits
    pub current_staged: String,              // Content with all staged patches applied
    pub patch_history: Vec<FilePatch>,       // Ordered list of applied patches
    pub is_committed: bool,                  // Whether current_staged is on disk
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePatch {
    pub id: PatchId,
    pub file_path: PathBuf,
    pub patch_type: PatchType,
    pub timestamp: DateTime<Utc>,
    pub checkpoint_id: Option<CheckpointId>, // Set when committed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatchType {
    FullReplace { content: String },
    Diff {
        old_range: LineRange,
        new_content: String,
    },
    Append { content: String },
    Delete { range: LineRange },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,  // Exclusive
}
```

### 5.4 FileEditor Implementation

```rust
pub struct FileEditor {
    staged: HashMap<PathBuf, StagedFileState>,
    wal: Arc<WalWriter>,
    project_root: PathBuf,
}

impl FileEditor {
    /// Stage an edit to a file. Does NOT write to disk.
    pub fn stage_edit(&mut self, path: &Path, patch: FilePatch) -> Result<(), EditorError> {
        let abs_path = self.project_root.join(path);

        let state = self.staged.entry(abs_path.clone()).or_insert_with(|| {
            // Load original content from disk on first edit
            let original = std::fs::read_to_string(&abs_path).ok();
            StagedFileState {
                path: abs_path.clone(),
                original_content: original.clone(),
                current_staged: original.unwrap_or_default(),
                patch_history: vec![],
                is_committed: true, // The original is on disk
            }
        });

        // Apply patch to staged content
        let new_content = Self::apply_patch(&state.current_staged, &patch.patch_type)?;
        state.current_staged = new_content;
        state.patch_history.push(patch.clone());
        state.is_committed = false;

        // Record in WAL
        self.wal.append(WalEntry::FileEditStaged {
            path: abs_path.clone(),
            patch,
        })?;

        Ok(())
    }

    /// Commit all staged edits to disk atomically.
    pub fn checkpoint_commit(&mut self, checkpoint_id: CheckpointId) -> Result<Vec<PathBuf>, EditorError> {
        let mut committed_files = Vec::new();

        for (path, state) in &mut self.staged {
            if state.is_committed {
                continue;
            }

            // Write to temp file then rename (atomic on most filesystems)
            let tmp_path = path.with_extension("xaft-tmp");
            std::fs::write(&tmp_path, &state.current_staged)?;
            std::fs::rename(&tmp_path, path)?;

            // Mark patches as committed
            for patch in &mut state.patch_history {
                patch.checkpoint_id = Some(checkpoint_id);
            }
            state.is_committed = true;
            committed_files.push(path.clone());
        }

        self.wal.append(WalEntry::CheckpointCommitted {
            checkpoint_id,
            files: committed_files.clone(),
        })?;

        Ok(committed_files)
    }

    /// Roll back a file to its state at a specific checkpoint.
    pub fn rollback_to_checkpoint(&mut self, path: &Path, checkpoint_id: CheckpointId) -> Result<(), EditorError> {
        let state = self.staged.get(path).ok_or(EditorError::FileNotStaged)?;

        // Find the content at the checkpoint by replaying patches up to that point
        let mut content = state.original_content.clone().unwrap_or_default();
        for patch in &state.patch_history {
            if patch.checkpoint_id == Some(checkpoint_id) || patch.checkpoint_id.is_none() {
                break;
            }
            content = Self::apply_patch(&content, &patch.patch_type)?;
        }

        // Write the rolled-back content
        std::fs::write(path, &content)?;

        self.wal.append(WalEntry::FileRolledBack {
            path: path.to_path_buf(),
            to_checkpoint: checkpoint_id,
        })?;

        Ok(())
    }
}
```

---

## 6. WorktreeGuard State

### 6.1 Purpose

The `WorktreeGuard` manages the git worktree state for xaft's sandboxed execution. It tracks branch names, lock status, and dirty file lists. This state must survive crashes to prevent orphaned worktrees or stale locks.

### 6.2 Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeState {
    pub worktree_id: WorktreeId,
    pub session_id: SessionId,
    pub path: PathBuf,
    pub base_branch: String,
    pub work_branch: String,
    pub status: WorktreeStatus,
    pub dirty_files: Vec<DirtyFile>,
    pub lock: WorktreeLock,
    pub created_at: DateTime<Utc>,
    pub last_checkpoint: Option<CheckpointId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorktreeStatus {
    Active,
    Suspended,
    Committed { revision: String },
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyFile {
    pub path: PathBuf,
    pub status: GitFileStatus,
    pub staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed { from: PathBuf },
    Untracked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorktreeLock {
    Unlocked,
    Locked { by: String, at: DateTime<Utc>, reason: String },
}
```

### 6.3 Crash-Safe Lock Management

```rust
impl WorktreeGuard {
    /// Acquire a worktree lock. The lock is recorded in both the WAL
    /// and a sidecar file so orphaned locks can be detected after a crash.
    pub fn acquire_lock(&mut self, reason: &str) -> Result<(), WorktreeError> {
        match &self.state.lock {
            WorktreeLock::Locked { by, .. } if by != &self.id => {
                return Err(WorktreeError::AlreadyLocked(by.clone()));
            }
            _ => {}
        }

        let lock = WorktreeLock::Locked {
            by: self.id.clone(),
            at: Utc::now(),
            reason: reason.to_string(),
        };

        // Write lock file FIRST (if crash before WAL, lock file prevents concurrent access)
        self.write_lock_file(&lock)?;

        // Then record in WAL
        self.wal.append(WalEntry::WorktreeLockAcquired {
            worktree_id: self.state.worktree_id,
            lock: lock.clone(),
        })?;

        self.state.lock = lock;
        Ok(())
    }

    /// Release the worktree lock.
    pub fn release_lock(&mut self) -> Result<(), WorktreeError> {
        let lock_file = self.state.path.join(".xaft-lock");

        // Remove lock file FIRST
        if lock_file.exists() {
            std::fs::remove_file(&lock_file)?;
        }

        // Then record in WAL
        self.wal.append(WalEntry::WorktreeLockReleased {
            worktree_id: self.state.worktree_id,
        })?;

        self.state.lock = WorktreeLock::Unlocked;
        Ok(())
    }

    /// Detect and clean up orphaned locks from a previous crash.
    pub fn recover_orphaned_locks(session_dir: &Path) -> Vec<WorktreeId> {
        let mut orphaned = Vec::new();
        if let Ok(entries) = std::fs::read_dir(session_dir) {
            for entry in entries.flatten() {
                let lock_path = entry.path().join(".xaft-lock");
                if lock_path.exists() {
                    // Lock file exists but process is dead — orphaned
                    let pid = std::fs::read_to_string(&lock_path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());

                    if let Some(pid) = pid {
                        if !is_process_alive(pid) {
                            std::fs::remove_file(&lock_path).ok();
                            orphaned.push(WorktreeId::from_path(&entry.path()));
                        }
                    }
                }
            }
        }
        orphaned
    }
}
```

---

## 7. Write-Ahead Log (WAL)

### 7.1 WAL Entry Types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalEntry {
    // TaskStore
    TaskCreated { task: Task },
    TaskStatusChanged { task_id: TaskId, from: TaskStatus, to: TaskStatus },

    // ConversationStore
    MessageAppended { conversation_id: ConversationId, message: Message },
    ConversationTruncated { conversation_id: ConversationId, removed_count: usize, removed_ids: Vec<MessageId> },

    // FileEditor
    FileEditStaged { path: PathBuf, patch: FilePatch },
    CheckpointCommitted { checkpoint_id: CheckpointId, files: Vec<PathBuf> },
    FileRolledBack { path: PathBuf, to_checkpoint: CheckpointId },

    // WorktreeGuard
    WorktreeLockAcquired { worktree_id: WorktreeId, lock: WorktreeLock },
    WorktreeLockReleased { worktree_id: WorktreeId },
    WorktreeBranchCreated { worktree_id: WorktreeId, branch: String },

    // Checkpoint markers
    CheckpointStarted { checkpoint_id: CheckpointId, timestamp: DateTime<Utc> },
    CheckpointCompleted { checkpoint_id: CheckpointId, timestamp: DateTime<Utc> },

    // Session lifecycle
    SessionStarted { session_id: SessionId, timestamp: DateTime<Utc> },
    SessionEnded { session_id: SessionId, timestamp: DateTime<Utc>, reason: SessionEndReason },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEndReason {
    Normal,
    Crash,
    Cancelled,
    Timeout,
}
```

### 7.2 WAL Writer

```rust
pub struct WalWriter {
    file: BufWriter<File>,
    sequence: AtomicU64,
    path: PathBuf,
}

impl WalWriter {
    pub fn open(session_dir: &Path) -> Result<Self, WalError> {
        let wal_path = session_dir.join("wal.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&wal_path)?;

        Ok(Self {
            file: BufWriter::new(file),
            sequence: AtomicU64::new(0),
            path: wal_path,
        })
    }

    pub fn append(&self, entry: WalEntry) -> Result<u64, WalError> {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);

        let framed = FramedWalEntry {
            sequence: seq,
            timestamp: Utc::now(),
            entry,
        };

        let mut line = serde_json::to_string(&framed)?;
        line.push('\n');

        // Write to buffer
        let mut file = &self.file;
        file.write_all(line.as_bytes())?;
        file.flush()?; // Force to OS buffer

        // Fsync on every Nth write for durability
        if seq % 16 == 0 {
            file.get_ref().sync_all()?;
        }

        Ok(seq)
    }
}
```

### 7.3 WAL Reader (for Recovery)

```rust
pub struct WalReader;

impl WalReader {
    /// Replay the WAL to reconstruct session state.
    pub fn replay(session_dir: &Path) -> Result<ReconstructedState, WalError> {
        let wal_path = session_dir.join("wal.jsonl");

        // First, try to load the latest snapshot
        let mut state = Self::load_latest_snapshot(session_dir)?;

        // Then replay WAL entries after the snapshot
        let snapshot_seq = state.last_sequence;
        let file = File::open(&wal_path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            let framed: FramedWalEntry = serde_json::from_str(&line)?;

            if framed.sequence <= snapshot_seq {
                continue; // Already captured in snapshot
            }

            state.apply_entry(framed.entry)?;
            state.last_sequence = framed.sequence;
        }

        Ok(state)
    }
}
```

---

## 8. Checkpoint-Based Resumption

### 8.1 Checkpoint Lifecycle

```
  Agent Turn 1          Agent Turn 2          Agent Turn 3
  ┌──────────┐         ┌──────────┐         ┌──────────┐
  │ Tool Call │         │ Tool Call │         │ Tool Call │
  │ Tool Call │         │ Tool Call │         │ Tool Call │
  │ File Edit │         │ File Edit │         │           │
  └─────┬─────┘         └─────┬─────┘         └─────┬─────┘
        │                     │                     │
        ▼                     ▼                     ▼
  ══════════════       ══════════════       ══════════════
  Checkpoint C1        Checkpoint C2        Checkpoint C3
  ══════════════       ══════════════       ══════════════
  - task status        - task status        - task status
  - conversation       - conversation       - conversation
  - files committed    - files committed    - files committed
  - worktree lock      - worktree lock      - worktree lock

  ─ ─ ─ ─ CRASH ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─
                          │
                          ▼
                   Resume from C2
                   (last completed checkpoint)
```

### 8.2 Checkpoint Manager

```rust
pub struct CheckpointManager {
    checkpoints: Vec<Checkpoint>,
    wal: Arc<WalWriter>,
    file_editor: Arc<Mutex<FileEditor>>,
    task_store: Arc<Mutex<TaskStore>>,
    conversation_store: Arc<Mutex<ConversationStore>>,
    worktree_guard: Arc<Mutex<WorktreeGuard>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub task_summary: String,
    pub files_committed: Vec<PathBuf>,
    pub token_counts: TokenCounts,
}

impl CheckpointManager {
    /// Create a checkpoint: commit staged file edits and record state.
    pub async fn create_checkpoint(&self, task_summary: String) -> Result<CheckpointId, CheckpointError> {
        let checkpoint_id = CheckpointId::new();

        self.wal.append(WalEntry::CheckpointStarted {
            checkpoint_id,
            timestamp: Utc::now(),
        })?;

        // 1. Commit all staged file edits
        let committed = self.file_editor.lock().await.checkpoint_commit(checkpoint_id)?;

        // 2. Snapshot current state
        let task_store = self.task_store.lock().await;
        let conversation = self.conversation_store.lock().await;
        let worktree = self.worktree_guard.lock().await;

        let checkpoint = Checkpoint {
            id: checkpoint_id,
            sequence: self.wal.current_sequence(),
            timestamp: Utc::now(),
            task_summary,
            files_committed: committed,
            token_counts: conversation.token_counts().clone(),
        };

        self.wal.append(WalEntry::CheckpointCompleted {
            checkpoint_id,
            timestamp: Utc::now(),
        })?;

        Ok(checkpoint_id)
    }

    /// Resume from a checkpoint: reconstruct state and continue.
    pub async fn resume_from(&self, checkpoint_id: CheckpointId) -> Result<ResumedState, CheckpointError> {
        // 1. Reconstruct from WAL up to the checkpoint
        let state = WalReader::replay(&self.session_dir)?;

        // 2. Verify checkpoint integrity
        let checkpoint = state.checkpoints.iter()
            .find(|c| c.id == checkpoint_id)
            .ok_or(CheckpointError::CheckpointNotFound(checkpoint_id))?;

        // 3. Verify all committed files exist on disk
        for file in &checkpoint.files_committed {
            if !file.exists() {
                return Err(CheckpointError::CommittedFileMissing(file.clone()));
            }
        }

        // 4. Reconstruct in-memory state
        Ok(ResumedState {
            task_store: state.task_store,
            conversation: state.conversation,
            file_editor: state.file_editor,
            worktree: state.worktree,
            last_checkpoint: checkpoint.clone(),
        })
    }
}
```

---

## 9. Session File Format

### 9.1 Directory Layout

```
~/.xaft/sessions/<session-id>/
├── meta.toml                    # Session metadata
├── wal.jsonl                    # Write-ahead log
├── snapshots/
│   ├── snapshot-001.bincode     # Compacted snapshot
│   ├── snapshot-002.bincode
│   └── snapshot-LATEST.bincode  # Symlink to latest
├── worktree/
│   ├── .xaft-lock               # Worktree lock file (PID-based)
│   └── state.toml               # Worktree state
└── checkpoints/
    ├── cp-<id>.toml             # Checkpoint metadata
    └── cp-<id>-file-manifest.jsonl  # File states at checkpoint
```

### 9.2 Session Metadata

```toml
# meta.toml
[session]
id = "550e8400-e29b-41d4-a716-446655440000"
created_at = "2025-03-04T10:30:00Z"
updated_at = "2025-03-04T11:45:00Z"
status = "active"  # "active" | "completed" | "crashed" | "abandoned"

[session.project]
root = "/home/user/my-project"
git_branch = "feature/auth"
git_commit = "abc1234"

[session.agent]
preset = "default"
model = "claude-3.5-sonnet"
provider = "anthropic"

[session.checkpoints]
count = 5
latest = "cp-550e8400-0003"
```

### 9.3 Snapshot Format

Snapshots use bincode for compactness and speed:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: u32,
    pub session_id: SessionId,
    pub last_wal_sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub task_store: TaskStoreSnapshot,
    pub conversation: ConversationSnapshot,
    pub file_editor: FileEditorSnapshot,
    pub worktree: WorktreeSnapshot,
}

pub struct TaskStoreSnapshot {
    pub tasks: Vec<Task>,
    pub root_id: TaskId,
}

pub struct ConversationSnapshot {
    pub id: ConversationId,
    pub messages: Vec<Message>,
    pub token_counts: TokenCounts,
}

pub struct FileEditorSnapshot {
    pub staged_files: Vec<StagedFileState>,
}

pub struct WorktreeSnapshot {
    pub state: WorktreeState,
}

// Snapshot compaction: merge WAL entries into a snapshot
impl SessionSnapshot {
    pub fn compact(session_dir: &Path) -> Result<(), SnapshotError> {
        let state = WalReader::replay(session_dir)?;
        let snapshot = SessionSnapshot::from_reconstructed_state(state);

        let snapshot_path = session_dir.join(format!(
            "snapshots/snapshot-{:06}.bincode",
            snapshot.last_wal_sequence
        ));

        let encoded = bincode::encode_to_vec(&snapshot, bincode::config::standard())?;
        std::fs::write(&snapshot_path, encoded)?;

        // Update LATEST symlink
        let latest = session_dir.join("snapshots/snapshot-LATEST.bincode");
        let _ = std::fs::remove_file(&latest);
        std::os::unix::fs::symlink(&snapshot_path, &latest)?;

        // Truncate WAL entries that are now in the snapshot
        WalCompactor::truncate_to(session_dir, snapshot.last_wal_sequence)?;

        Ok(())
    }
}
```

---

## 10. TUI Reconnection

### 10.1 Problem

When the terminal disconnects (SSH drop, tmux detach, terminal crash), the xaft TUI must be able to reconnect to a still-running session daemon.

### 10.2 Architecture

```
┌──────────────┐     Unix Socket      ┌────────────────────┐
│  TUI Frontend │◄───────────────────►│  xaft Daemon       │
│  (ratatui)    │   /tmp/xaft-<id>    │  (background proc) │
│               │                      │                    │
│  - Renders UI │   JSON messages      │  - Runs agent loop │
│  - Captures   │◄───────────────────►│  - Manages tools   │
│    input      │                      │  - Owns session    │
└──────────────┘                      └────────────────────┘
```

### 10.3 Daemon Mode

```rust
pub struct XaftDaemon {
    session: Arc<Session>,
    listener: UnixListener,
    clients: Arc<Mutex<Vec<DaemonClient>>>,
}

impl XaftDaemon {
    pub async fn run(&mut self) -> Result<(), DaemonError> {
        loop {
            tokio::select! {
                // Accept new TUI connections
                Ok((stream, _addr)) = self.listener.accept() => {
                    let client = DaemonClient::new(stream, self.session.clone());
                    self.clients.lock().await.push(client);
                }

                // Process agent events and broadcast to TUI clients
                Some(event) = self.session.next_event() => {
                    let json = serde_json::to_string(&event)?;
                    let clients = self.clients.lock().await;
                    for client in clients.iter() {
                        client.send(&json).await.ok(); // Best effort
                    }
                }
            }
        }
    }
}
```

### 10.4 Reconnection Protocol

```
  TUI (reconnecting)                  xaft Daemon
  ─────────────────                   ───────────
       │                                    │
       │  CONNECT /tmp/xaft-<session-id>    │
       │───────────────────────────────────►│
       │                                    │
       │  RECONNECT { last_event_id: 42 }   │
       │───────────────────────────────────►│
       │                                    │
       │  REPLAY { events[43..47] }         │
       │◄───────────────────────────────────│
       │                                    │
       │  STATE_SYNC { full state dump }    │
       │◄───────────────────────────────────│
       │                                    │
       │  LIVE_EVENTS { stream }            │
       │◄═══════════════════════════════════│
       │                                    │
```

```rust
#[derive(Debug, Serialize, Deserialize)]
pub enum DaemonMessage {
    // Client → Daemon
    Connect { protocol_version: u32 },
    Reconnect { last_event_id: u64 },
    UserInput { content: String },
    Command { cmd: TuiCommand },

    // Daemon → Client
    Replay { events: Vec<SessionEvent> },
    StateSync { state: TuiStateSnapshot },
    LiveEvent { event: SessionEvent },
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TuiStateSnapshot {
    pub session_id: SessionId,
    pub task_tree: Vec<TaskSummary>,
    pub conversation_preview: Vec<MessageSummary>,
    pub active_files: Vec<PathBuf>,
    pub agent_status: AgentStatus,
    pub current_checkpoint: Option<CheckpointId>,
    pub token_counts: TokenCounts,
}
```

### 10.5 Session Discovery

When the TUI starts, it must find existing sessions:

```rust
pub fn discover_sessions() -> Vec<SessionInfo> {
    let sessions_dir = dirs::config_dir()
        .unwrap_or_default()
        .join("xaft/sessions");

    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let meta_path = entry.path().join("meta.toml");
            if meta_path.exists() {
                if let Ok(meta) = std::fs::read_to_string(&meta_path) {
                    if let Ok(session_meta) = toml::from_str::<SessionMeta>(&meta) {
                        // Check if daemon is still alive
                        let socket = std::path::PathBuf::from(format!("/tmp/xaft-{}", session_meta.id));
                        let alive = socket.exists();

                        sessions.push(SessionInfo {
                            id: session_meta.id,
                            project: session_meta.project.root.clone(),
                            status: if alive { "active" } else { &session_meta.status }.to_string(),
                            created_at: session_meta.created_at,
                        });
                    }
                }
            }
        }
    }
    sessions
}
```

---

## 11. Crash Recovery Procedure

### 11.1 Recovery Sequence

```
  xaft startup
       │
       ▼
  Discover sessions ──────► No sessions ──► Normal startup
       │
       ▼
  Found crashed session?
       │
       ├─ No ──► Normal startup
       │
       └─ Yes ──► Recovery mode
                    │
                    ▼
             ┌──────────────────────┐
             │ 1. Check PID liveness│
             │    (is daemon alive?)│
             └──────────┬───────────┘
                        │
                  ┌─────┴─────┐
                  │ Alive?    │
                  ├─Yes───────┤──► Reconnect to daemon
                  │ No        │
                  └───────────┘
                        │
                        ▼
             ┌──────────────────────┐
             │ 2. Scan WAL          │
             │    Find last complete│
             │    checkpoint        │
             └──────────┬───────────┘
                        │
                        ▼
             ┌──────────────────────┐
             │ 3. Replay WAL from   │
             │    snapshot to last  │
             │    checkpoint        │
             └──────────┬───────────┘
                        │
                        ▼
             ┌──────────────────────┐
             │ 4. Clean up orphaned │
             │    worktree locks    │
             └──────────┬───────────┘
                        │
                        ▼
             ┌──────────────────────┐
             │ 5. Verify committed  │
             │    files on disk     │
             └──────────┬───────────┘
                        │
                        ▼
             ┌──────────────────────┐
             │ 6. Present recovery  │
             │    options to user   │
             └──────────┬───────────┘
                        │
                  ┌─────┴──────┐
                  │ User choice│
                  ├────────────┤
                  │ Resume     ├──► Restore state + continue
                  │ Rollback   ├──► Pick checkpoint + revert
                  │ Inspect    ├──► Open TUI in read-only mode
                  │ Discard    ├──► Delete session, clean worktree
                  └────────────┘
```

### 11.2 Recovery Implementation

```rust
pub async fn recover_session(session_id: &SessionId) -> Result<RecoveryOutcome, RecoveryError> {
    let session_dir = dirs::config_dir()
        .unwrap_or_default()
        .join(format!("xaft/sessions/{}", session_id));

    // Step 1: Check if daemon is alive
    let socket_path = format!("/tmp/xaft-{}", session_id);
    if PathBuf::from(&socket_path).exists() {
        if let Ok(_) = try_connect_daemon(&socket_path).await {
            return Ok(RecoveryOutcome::DaemonAlive { socket: socket_path });
        }
    }

    // Step 2: Load latest snapshot + replay WAL
    let mut state = WalReader::replay(&session_dir)?;

    // Step 3: Find last complete checkpoint
    let last_checkpoint = state.checkpoints.last()
        .ok_or(RecoveryError::NoCheckpoints)?
        .clone();

    // Step 4: Clean orphaned locks
    let orphaned = WorktreeGuard::recover_orphaned_locks(&session_dir);

    // Step 5: Verify file integrity
    let mut missing_files = Vec::new();
    for file in &last_checkpoint.files_committed {
        if !file.exists() {
            missing_files.push(file.clone());
        }
    }

    if !missing_files.is_empty() {
        return Ok(RecoveryOutcome::DataLoss {
            checkpoint: last_checkpoint,
            missing_files,
            orphaned_locks: orphaned,
        });
    }

    Ok(RecoveryOutcome::Recoverable {
        checkpoint: last_checkpoint,
        state,
        orphaned_locks: orphaned,
    })
}
```

---

## 12. Testing Strategy

| Level | Test | Approach |
|-------|------|----------|
| Unit | WAL write/read | Round-trip serialization, sequence ordering |
| Unit | TaskStore transitions | State machine property testing |
| Integration | FileEditor staged state | Apply patches, commit, verify disk content |
| Integration | Checkpoint + resume | Create checkpoint, inject crash, resume |
| Chaos | Kill at random WAL entry | SIGKILL during agent loop, verify recovery |
| Performance | WAL throughput | 10K entries/sec target |
| Performance | Snapshot compaction | 100K-entry WAL compaction in <1s |
| E2E | Full session lifecycle | Start → work → crash → recover → continue |

---

## 13. Milestones

| Phase | Deliverable | Timeline |
|-------|-------------|----------|
| P1 | WAL writer/reader + session directory layout | Week 1 |
| P2 | TaskStore + ConversationStore with WAL integration | Week 2 |
| P3 | FileEditor staged state + atomic commit | Week 3 |
| P4 | WorktreeGuard state + lock management | Week 4 |
| P5 | CheckpointManager + snapshot compaction | Week 5 |
| P6 | Crash recovery procedure + user-facing recovery TUI | Week 6-7 |
| P7 | Daemon mode + TUI reconnection | Week 8-9 |

---

## 14. Open Questions

1. **WAL durability vs. performance**: Should we fsync on every WAL entry or batch? Current design fsyncs every 16th entry — acceptable for SSDs but risky on HDD.
2. **Snapshot format**: bincode is fast but not forward-compatible. Should we use a versioned format for cross-version migration?
3. **Concurrent sessions**: Can two xaft instances share the same project? How do we detect and prevent conflicting sessions?
4. **Binary file support**: The current FileEditor assumes text files. How should binary files (images, compiled objects) be handled in staged state?
5. **Remote session storage**: Should we support storing sessions on S3/GCS for team sharing? This affects the WAL and snapshot formats.
