# PRD: Failure Recovery & Resilience

> xaft — Autonomous Coding CLI built on agtrs
> Document: `safety/03_failure_recovery.md`
> Version: 0.1.0-draft

---

## 1. Overview

Autonomous coding agents operate in an inherently unreliable environment: LLM
APIs return errors, tool executions fail, network connections drop, and
operations timeout. xaft must be **resilient by default** — failures must be
contained, recovered from, and never leave the workspace in an inconsistent
state. This document specifies the complete failure taxonomy, recovery
strategies, and the agtrs primitives that implement them.

---

## 2. Failure Taxonomy

```
┌──────────────────────────────────────────────────────────────────┐
│                     FAILURE TAXONOMY                             │
├──────────────┬───────────────────────────────────────────────────┤
│ Category     │ Examples                                         │
├──────────────┼───────────────────────────────────────────────────┤
│ LLM Failures │ Rate limit (429), Auth error (401/403),          │
│              │ Server error (500/502/503), Context length       │
│              │ exceeded, Malformed response, Content filter      │
│              │ refusal, Provider outage                         │
├──────────────┼───────────────────────────────────────────────────┤
│ Tool Failures│ File not found, Permission denied, Path escape,  │
│              │ Command non-zero exit, Invalid arguments,        │
│              │ Sandbox violation, Resource limit exceeded        │
├──────────────┼───────────────────────────────────────────────────┤
│ Network      │ DNS resolution failure, Connection refused,       │
│ Errors       │ TLS handshake failure, Connection reset,          │
│              │ Read timeout, Unexpected EOF                      │
├──────────────┼───────────────────────────────────────────────────┤
│ Timeouts     │ LLM response timeout, Tool execution timeout,    │
│              │ Task-level timeout, User inactivity timeout       │
├──────────────┼───────────────────────────────────────────────────┤
│ System       │ Out of memory, Disk full, Process killed (OOM),  │
│ Failures     │ Signal interruption (SIGTERM/SIGINT)             │
└──────────────┴───────────────────────────────────────────────────┘
```

---

## 3. LLM Failure Handling

### 3.1 Error Classification

```rust
/// Categorization of LLM API errors for recovery strategy selection.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// Rate limited — retry after delay
    RateLimited {
        retry_after: Option<Duration>,
        requests_remaining: Option<u64>,
    },
    /// Authentication / authorization failure — do not retry
    AuthError {
        status: u16,
        message: String,
    },
    /// Server-side error — may be transient
    ServerError {
        status: u16,
        message: String,
    },
    /// Request too large for the model's context window
    ContextLengthExceeded {
        token_count: Option<u64>,
        max_tokens: Option<u64>,
    },
    /// Response could not be parsed into expected format
    MalformedResponse {
        raw: String,
        parse_error: String,
    },
    /// Content was filtered by the provider's safety system
    ContentFiltered {
        reason: Option<String>,
    },
    /// Complete provider outage
    ProviderUnavailable {
        provider: String,
    },
    /// Network-level failure reaching the provider
    NetworkError {
        source: NetworkError,
    },
}
```

### 3.2 FallbackProvider

The `FallbackProvider` implements a **primary/fallback** pattern. When the
primary LLM provider fails, xaft automatically switches to a configured
fallback provider, preserving conversation context.

```rust
/// Multi-provider LLM client with automatic fallback.
pub struct FallbackProvider {
    /// Ordered list of providers to try
    providers: Vec<ProviderConfig>,
    /// Current active provider index
    active_index: AtomicUsize,
    /// Retry configuration
    retry_config: RetryConfig,
    /// Circuit breaker per provider
    circuit_breakers: Vec<CircuitBreaker>,
    /// Conversation context to carry across providers
    context_adapter: Box<dyn ContextAdapter>,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub name: String,
    pub model: String,
    pub api_base: String,
    pub api_key: KeySource,
    pub priority: u32,
    pub max_context_tokens: u64,
    pub supports_tools: bool,
    pub supports_streaming: bool,
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retries per request
    pub max_retries: u32,
    /// Base delay for exponential backoff
    pub base_delay: Duration,
    /// Maximum delay cap
    pub max_delay: Duration,
    /// Jitter factor (0.0 = no jitter, 1.0 = full jitter)
    pub jitter: f64,
    /// Whether to multiply delay on each retry
    pub exponential_base: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            jitter: 0.25,
            exponential_base: 2.0,
        }
    }
}
```

### 3.3 Fallback Flow

```
Agent requests LLM completion
            │
            ▼
    ┌───────────────────┐
    │ FallbackProvider  │
    │ .complete()       │
    └─────────┬─────────┘
              │
              ▼
    ┌──────────────────┐     ┌───────────────────────────┐
    │ Try provider[0]  │────▶│ Circuit breaker open?     │
    │ (primary)        │     │ Yes → skip to next        │
    └────────┬─────────┘     │ No  → send request        │
             │               └───────────┬───────────────┘
             │                           │
       ┌─────┴──────┐                    │
       │  Success?  │                    │
       └──┬─────┬───┘                    │
       Yes│     │No                      │
          ▼     ▼                        │
       Return  Classify error            │
               ┌─────┴──────────┐        │
               │                │        │
          ┌────┴────┐    ┌──────┴───┐    │
          │Retryable│    │Permanent │    │
          │ 429/5xx │    │ 401/403  │    │
          └────┬────┘    └──────┬───┘    │
               │                │        │
               ▼                ▼        │
        ┌──────────────┐   Trip circuit │
        │ Exponential  │   breaker for  │
        │ backoff +    │   this provider│
        │ retry        │                │
        └──────┬───────┘                │
               │                        │
          Retries                        │
          exhausted?                    │
          ┌────┴────┐                   │
          Yes      No                   │
          │         │                   │
          ▼      Retry ◀────────────────┘
   ┌────────────────────┐
   │ Try provider[1]    │──── (same flow)
   │ (fallback)         │
   └────────┬───────────┘
            │
       All providers
       exhausted?
            │
       ┌────┴────┐
       Yes      No
       │         │
       ▼      Return success
  ┌──────────────────┐
  │ Return            │
  │ LlmError::       │
  │ ProviderUnavailable│
  └──────────────────┘
```

### 3.4 Circuit Breaker

```rust
/// Circuit breaker per LLM provider to avoid hammering a failing service.
pub struct CircuitBreaker {
    state: Atomic<CircuitState>,
    failure_count: AtomicU32,
    last_failure: AtomicInstant,
    /// Threshold to open the circuit
    failure_threshold: u32,
    /// How long to wait before trying half-open
    reset_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircuitState {
    /// Normal operation — requests go through
    Closed,
    /// Failing — requests are short-circuited
    Open,
    /// Testing — one request allowed to probe recovery
    HalfOpen,
}

impl CircuitBreaker {
    pub fn should_try(&self) -> bool {
        match self.state.load() {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if reset_timeout has elapsed
                let elapsed = self.last_failure.elapsed();
                if elapsed >= self.reset_timeout {
                    self.state.store(CircuitState::HalfOpen);
                    true // allow one probe request
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true, // allow one probe
        }
    }

    pub fn record_success(&self) {
        self.failure_count.store(0);
        self.state.store(CircuitState::Closed);
    }

    pub fn record_failure(&self) {
        let failures = self.failure_count.fetch_add(1) + 1;
        self.last_failure.update_to_now();
        if failures >= self.failure_threshold {
            self.state.store(CircuitState::Open);
        }
    }
}
```

### 3.5 Context Adapter

When falling back to a different provider, the conversation context may need
adaptation (e.g., different max tokens, different tool formats):

```rust
/// Adapts conversation context when switching between providers.
pub trait ContextAdapter: Send + Sync {
    /// Trim or compress the conversation to fit the new provider's limits.
    fn adapt_context(&self, context: &Conversation, target: &ProviderConfig) -> Conversation;

    /// Convert tool definitions between provider formats.
    fn adapt_tools(&self, tools: &[ToolDef], target: &ProviderConfig) -> Vec<ToolDef>;
}
```

---

## 4. Tool Failure Handling

### 4.1 ToolErrorPolicy

When a tool execution fails, the `ToolErrorPolicy` determines how the agent
framework reacts:

```rust
/// Policy for handling tool execution failures.
#[derive(Debug, Clone)]
pub enum ToolErrorPolicy {
    /// Propagate the error to the agent as-is, letting it decide.
    /// The agent receives the error message and can re-plan.
    Raise,

    /// Return a structured error result to the agent, but don't
    /// abort the task. The agent can try a different approach.
    ReturnError,

    /// Silently skip the failed tool call and return an empty result.
    /// The agent continues as if the call succeeded with no output.
    Skip,
}

/// Per-tool error policy configuration.
pub struct ToolErrorConfig {
    /// Default policy for all tools
    default: ToolErrorPolicy,
    /// Per-tool overrides
    overrides: HashMap<String, ToolErrorPolicy>,
}

impl ToolErrorConfig {
    pub fn policy_for(&self, tool_name: &str) -> &ToolErrorPolicy {
        self.overrides.get(tool_name).unwrap_or(&self.default)
    }
}
```

### 4.2 Error Policy Decision Flow

```
Tool execution fails
        │
        ▼
┌──────────────────────┐
│ ToolErrorConfig::    │
│ policy_for(tool)     │
└──────────┬───────────┘
           │
    ┌──────┼──────────┬──────────┐
    │      │          │          │
    ▼      ▼          ▼          ▼
  Raise  ReturnError  Skip     (unconfigured)
    │      │          │          │
    ▼      ▼          ▼          ▼
┌──────┐ ┌────────┐ ┌──────┐ ┌──────┐
│Agent │ │Agent   │ │Agent │ │Agent │
│gets  │ │gets    │ │gets  │ │gets  │
│error │ │error   │ │empty │ │error │
│msg + │ │result  │ │result│ │msg   │
│MUST  │ │can     │ │conti-│ │MUST  │
│replan│ │replan  │ │nues  │ │replan│
└──────┘ └────────┘ └──────┘ └──────┘
```

### 4.3 Tool Retry Strategy

For transient tool failures (e.g., file locked, network blip), xaft
implements automatic retry before applying the error policy:

```rust
pub struct ToolRetryConfig {
    /// Maximum retries for transient errors
    pub max_retries: u32,
    /// Delay between retries
    pub retry_delay: Duration,
    /// Errors that are considered transient and retryable
    pub retryable_errors: Vec<String>,
}

/// Determine if a tool error is transient and retryable.
pub fn is_retryable(error: &ToolError) -> bool {
    match error {
        ToolError::Io(ref io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::ConnectionReset
        ),
        ToolError::Network(_) => true,
        ToolError::Sandbox(_) => false,    // never retry sandbox violations
        ToolError::Denied { .. } => false, // never retry denied operations
        _ => false,
    }
}
```

---

## 5. Network Error Handling

### 5.1 Network Error Hierarchy

```rust
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("DNS resolution failed for '{host}': {source}")]
    Dns { host: String, source: std::io::Error },

    #[error("Connection refused to {addr}")]
    ConnectionRefused { addr: String },

    #[error("TLS handshake failed: {reason}")]
    TlsHandshake { reason: String },

    #[error("Connection reset by {peer}")]
    ConnectionReset { peer: String },

    #[error("Read timeout after {duration:?}")]
    ReadTimeout { duration: Duration },

    #[error("Unexpected EOF reading {bytes_read} of {expected} bytes")]
    UnexpectedEof { bytes_read: usize, expected: usize },

    #[error("Proxy error: {message}")]
    Proxy { message: String },
}
```

### 5.2 Network Recovery Strategy

```
Network Error
      │
      ▼
┌──────────────────┐
│ Classify error   │
└────────┬─────────┘
         │
   ┌─────┼──────────────┐
   │     │              │
   ▼     ▼              ▼
 DNS  Connection    Timeout/
 Error Refused     Reset
   │     │              │
   ▼     ▼              ▼
Retry  Retry with     Retry with
with   exponential    exponential
cached backoff +      backoff +
result alternate     reduce
       provider      timeout
   │     │              │
   ▼     ▼              ▼
 After N retries exhausted:
   │     │              │
   ▼     ▼              ▼
 Report  Report to     Report to
 cached  agent as     agent as
 stale   network      timeout
 result  error        error →
         → Fallback   agent can
         Provider     adjust
         → agent      strategy
         re-plans
```

---

## 6. TaskRunner Checkpoint Recovery

### 6.1 Checkpoint Model

The `TaskRunner` periodically captures checkpoints of the agent's execution
state. If the task fails or is interrupted, it can be resumed from the most
recent checkpoint.

```rust
/// A snapshot of the agent's execution state at a point in time.
#[derive(Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique checkpoint ID
    pub id: Uuid,
    /// Task this checkpoint belongs to
    pub task_id: Uuid,
    /// Timestamp of checkpoint creation
    pub created_at: DateTime<Utc>,
    /// The conversation history up to this point
    pub conversation: Vec<Message>,
    /// Tool calls made since last checkpoint
    pub tool_calls: Vec<ToolCallRecord>,
    /// Files modified since task start (for rollback)
    pub file_mutations: Vec<FileMutation>,
    /// Current plan step index
    pub plan_step_index: usize,
    /// Git state at checkpoint time
    pub git_state: GitCheckpointState,
    /// Scratchpad contents at checkpoint time
    pub scratchpad: HashMap<String, String>,
    /// Memory facts accumulated so far
    pub memory_facts: Vec<MemoryFact>,
}

#[derive(Serialize, Deserialize)]
pub struct FileMutation {
    /// Path relative to workspace root
    pub path: String,
    /// Content before mutation (for rollback)
    pub before: Option<String>,
    /// Content after mutation
    pub after: Option<String>,
    /// Type of mutation
    pub mutation_type: MutationType,
}

#[derive(Serialize, Deserialize)]
pub enum MutationType {
    Created,
    Modified,
    Deleted,
}

#[derive(Serialize, Deserialize)]
pub struct GitCheckpointState {
    pub branch: String,
    pub commit_hash: Option<String>,
    pub dirty: bool,
}
```

### 6.2 Checkpoint Manager

```rust
/// Manages checkpoint creation, storage, and recovery.
pub struct CheckpointManager {
    /// Storage backend for checkpoints
    store: Box<dyn CheckpointStore>,
    /// How often to auto-checkpoint (in number of tool calls)
    auto_checkpoint_interval: u32,
    /// Tool call counter since last checkpoint
    call_counter: AtomicU32,
}

#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// Persist a checkpoint
    async fn save(&self, checkpoint: &Checkpoint) -> Result<()>;
    /// Load the most recent checkpoint for a task
    async fn load_latest(&self, task_id: Uuid) -> Result<Option<Checkpoint>>;
    /// List all checkpoints for a task
    async fn list(&self, task_id: Uuid) -> Result<Vec<CheckpointSummary>>;
    /// Delete a checkpoint
    async fn delete(&self, checkpoint_id: Uuid) -> Result<()>;
}

impl CheckpointManager {
    /// Check if a checkpoint should be created (auto-checkpoint).
    pub fn should_checkpoint(&self) -> bool {
        let count = self.call_counter.fetch_add(1, Ordering::Relaxed) + 1;
        count % self.auto_checkpoint_interval == 0
    }

    /// Create a checkpoint from the current task state.
    pub async fn create_checkpoint(&self, state: &TaskState) -> Result<Checkpoint> {
        let checkpoint = Checkpoint {
            id: Uuid::new_v4(),
            task_id: state.task_id,
            created_at: Utc::now(),
            conversation: state.conversation.clone(),
            tool_calls: state.tool_calls.clone(),
            file_mutations: state.file_mutations.clone(),
            plan_step_index: state.plan_step_index,
            git_state: state.git_state.clone(),
            scratchpad: state.scratchpad.clone(),
            memory_facts: state.memory_facts.clone(),
        };
        self.store.save(&checkpoint).await?;
        Ok(checkpoint)
    }

    /// Resume a task from its most recent checkpoint.
    pub async fn resume_from_checkpoint(
        &self,
        task_id: Uuid,
    ) -> Result<Option<TaskState>> {
        let checkpoint = self.store.load_latest(task_id).await?;
        match checkpoint {
            Some(cp) => Ok(Some(TaskState::from_checkpoint(cp))),
            None => Ok(None),
        }
    }
}
```

### 6.3 Checkpoint Recovery Flow

```
Task fails or interrupted
           │
           ▼
   ┌───────────────────┐
   │ CheckpointManager │
   │ .load_latest()    │
   └─────────┬─────────┘
             │
       ┌─────┴──────┐
       │ Checkpoint │
       │ exists?    │
       └──┬─────┬───┘
       Yes│     │No
          ▼     ▼
   ┌──────────┐ ┌────────────────┐
   │ Restore  │ │ Start fresh    │
   │ state    │ │ (cannot resume)│
   └─────┬────┘ └────────────────┘
         │
         ▼
   ┌──────────────────────────────────┐
   │ Recovery Steps:                  │
   │                                  │
   │ 1. Restore conversation history  │
   │ 2. Restore git state             │
   │ 3. Restore scratchpad            │
   │ 4. Restore memory facts          │
   │ 5. Replay file mutations?        │
   │    (configurable)                │
   │ 6. Resume at plan_step_index     │
   └──────────────────────────────────┘
         │
         ▼
   ┌────────────────┐
   │ Agent resumes  │
   │ with context   │
   │ "I was doing X │
   │ when I failed. │
   │ Retrying..."   │
   └────────────────┘
```

---

## 7. WorktreeGuard Restore

### 7.1 Automatic Restore on Failure

When a task fails and the user chooses to discard changes, `WorktreeGuard`
provides atomic restore:

```rust
impl WorktreeGuard {
    /// Restore the worktree to the state before the agent made changes.
    /// This is a hard reset to the base commit + clean untracked files.
    pub async fn restore(&self) -> Result<RestoreResult, WorktreeError> {
        // 1. Reset all tracked files to base commit
        git::hard_reset(&self.worktree_path, &self.base_commit.to_string()).await?;

        // 2. Remove all untracked files and directories
        git::clean_force(&self.worktree_path).await?;

        // 3. Verify the worktree matches base_commit
        let current_head = git::get_head(&self.worktree_path).await?;
        if current_head != self.base_commit {
            return Err(WorktreeError::RestoreVerificationFailed {
                expected: self.base_commit.clone(),
                actual: current_head,
            });
        }

        Ok(RestoreResult {
            files_reverted: git::count_changed_files(&self.worktree_path).await?,
            untracked_removed: true,
            base_commit: self.base_commit.clone(),
        })
    }
}

#[derive(Debug)]
pub struct RestoreResult {
    pub files_reverted: usize,
    pub untracked_removed: bool,
    pub base_commit: GitHash,
}
```

### 7.2 Restore Flow

```
Task failure detected
        │
        ▼
┌──────────────────┐
│ User chooses:    │
│ "Discard changes"│
└────────┬─────────┘
         │
         ▼
┌──────────────────────────────────┐
│ WorktreeGuard::restore()         │
│                                  │
│ 1. git reset --hard <base>       │
│ 2. git clean -fdx               │
│ 3. Verify HEAD == base_commit    │
│                                  │
│ ┌──────────────────────────────┐ │
│ │ If verification fails:       │ │
│ │ • Log critical error         │ │
│ │ • Alert user manually        │ │
│ │ • Do NOT proceed silently    │ │
│ └──────────────────────────────┘ │
└──────────────────────────────────┘
         │
         ▼
   Workspace is clean
   (same as before task started)
```

---

## 8. FileEditor Rollback

### 8.1 Per-File Undo Stack

The `FileEditor` maintains an undo stack for every file it modifies. This
enables fine-grained rollback of individual file changes without reverting the
entire worktree.

```rust
/// Tracks mutations to individual files for fine-grained rollback.
pub struct FileEditor {
    /// Map from file path to its undo stack
    undo_stacks: HashMap<PathBuf, Vec<FileSnapshot>>,
    /// Maximum undo depth per file
    max_undo_depth: usize,
    /// Reference to workspace for path validation
    workspace: Arc<WorkspaceStore>,
}

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    /// Content before the mutation
    pub content: Option<String>,  // None = file didn't exist
    /// Timestamp of the snapshot
    pub timestamp: DateTime<Utc>,
    /// What operation created this snapshot
    pub operation: FileOperation,
}

#[derive(Debug, Clone)]
pub enum FileOperation {
    Write,
    Append,
    Delete,
    Rename { from: PathBuf },
}

impl FileEditor {
    /// Write content to a file, pushing the current state onto the undo stack.
    pub async fn write_file(&mut self, path: &str, content: &str) -> Result<(), ToolError> {
        let validated = self.workspace.sanitize_path(path)?;

        // Snapshot current state before overwriting
        let current_content = tokio::fs::read_to_string(&validated).await.ok();
        let snapshot = FileSnapshot {
            content: current_content,
            timestamp: Utc::now(),
            operation: FileOperation::Write,
        };

        // Push to undo stack (with depth limit)
        let stack = self.undo_stacks.entry(validated.clone()).or_default();
        if stack.len() >= self.max_undo_depth {
            stack.remove(0); // drop oldest snapshot
        }
        stack.push(snapshot);

        // Write the new content
        if let Some(parent) = validated.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&validated, content).await?;
        Ok(())
    }

    /// Undo the last N mutations to a specific file.
    pub async fn undo(&mut self, path: &str, steps: usize) -> Result<(), ToolError> {
        let validated = self.workspace.sanitize_path(path)?;
        let stack = self.undo_stacks.get_mut(&validated)
            .ok_or(ToolError::NoUndoHistory { path: path.to_string() })?;

        // Pop snapshots and restore
        for _ in 0..steps {
            if let Some(snapshot) = stack.pop() {
                match &snapshot.content {
                    Some(content) => {
                        tokio::fs::write(&validated, content).await?;
                    }
                    None => {
                        // File didn't exist before — delete it
                        tokio::fs::remove_file(&validated).await.ok();
                    }
                }
            }
        }
        Ok(())
    }

    /// Rollback ALL mutations made by this editor (restore all files).
    pub async fn rollback_all(&mut self) -> Result<RollbackSummary, ToolError> {
        let mut summary = RollbackSummary::default();
        let paths: Vec<PathBuf> = self.undo_stacks.keys().cloned().collect();

        for path in &paths {
            if let Some(stack) = self.undo_stacks.get_mut(path) {
                if let Some(earliest) = stack.first() {
                    match &earliest.content {
                        Some(content) => {
                            tokio::fs::write(path, content).await?;
                        }
                        None => {
                            tokio::fs::remove_file(path).await.ok();
                        }
                    }
                    summary.files_restored += 1;
                }
                stack.clear();
            }
        }
        Ok(summary)
    }
}

#[derive(Debug, Default)]
pub struct RollbackSummary {
    pub files_restored: usize,
}
```

### 8.2 Rollback Integration with Checkpoints

```
┌──────────────────────────────────────────────────────────┐
│                ROLLBACK HIERARCHY                         │
│                                                          │
│  ┌────────────────┐                                      │
│  │ FileEditor     │  Undo single file mutations          │
│  │ undo()         │  (finest granularity)                │
│  └───────┬────────┘                                      │
│          │                                               │
│  ┌───────▼────────┐                                      │
│  │ FileEditor     │  Undo ALL file mutations in task     │
│  │ rollback_all() │  (medium granularity)                │
│  └───────┬────────┘                                      │
│          │                                               │
│  ┌───────▼────────┐                                      │
│  │ WorktreeGuard  │  Hard reset entire worktree          │
│  │ restore()      │  (coarsest granularity)              │
│  └───────┬────────┘                                      │
│          │                                               │
│  ┌───────▼────────┐                                      │
│  │ Checkpoint     │  Resume from saved state             │
│  │ Recovery       │  (time-travel granularity)           │
│  └────────────────┘                                      │
└──────────────────────────────────────────────────────────┘
```

---

## 9. CancellationToken Propagation

### 9.1 Cancellation Model

Users can cancel a running task at any time. The `CancellationToken` propagates
the cancellation signal through the entire agent stack, ensuring graceful
shutdown.

```rust
/// Cooperative cancellation token inspired by Tokio's CancellationToken.
pub struct CancellationToken {
    inner: Arc<CancellationTokenInner>,
}

struct CancellationTokenInner {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationTokenInner {
                cancelled: AtomicBool::new(false),
                wakers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Signal cancellation to all holders of child tokens.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        let wakers = std::mem::take(&mut *self.inner.wakers.lock().unwrap());
        for waker in wakers {
            waker.wake();
        }
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Create a child token that shares the same cancellation signal.
    pub fn child_token(&self) -> CancellationToken {
        CancellationToken {
            inner: Arc::clone(&self.inner),
        }
    }
}
```

### 9.2 Cancellation Propagation Flow

```
User presses Ctrl+C
        │
        ▼
┌─────────────────────┐
│ Signal handler      │
│ sets cancel token   │
└─────────┬───────────┘
          │
          ▼
┌──────────────────────────────────────────────────────┐
│            CANCELLATION PROPAGATION                   │
│                                                      │
│  CancellationToken                                   │
│       │                                              │
│       ├──▶ LLM Client: abort in-flight request       │
│       │    (cancel HTTP connection)                   │
│       │                                              │
│       ├──▶ Tool Dispatcher: skip queued tools         │
│       │    (don't start new tool calls)               │
│       │                                              │
│       ├──▶ Running Tool: check between steps          │
│       │    (cooperative yield points)                 │
│       │                                              │
│       ├──▶ CheckpointManager: save emergency cp       │
│       │    (preserve state before exit)               │
│       │                                              │
│       └──▶ WorktreeGuard: decide commit vs restore    │
│            (based on user config)                     │
└──────────────────────────────────────────────────────┘
```

### 9.3 Cooperative Cancellation in Tools

Tool implementations must check for cancellation at natural yield points:

```rust
#[tool(name = "shell_exec", description = "Execute a shell command")]
async fn shell_exec(
    ctx: &ToolContext,
    command: String,
) -> Result<ToolOutput, ToolError> {
    // Check cancellation before starting
    if ctx.cancel_token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let mut child = ctx.sandbox.execute_async(&command).await?;

    // Poll with cancellation awareness
    loop {
        tokio::select! {
            status = child.wait() => {
                return Ok(ToolOutput::text(format!("Exit: {:?}", status)));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if ctx.cancel_token.is_cancelled() {
                    child.kill().await.ok();
                    return Err(ToolError::Cancelled);
                }
            }
        }
    }
}
```

---

## 10. Workspace Consistency Guarantees

### 10.1 Consistency Levels

xaft provides three levels of workspace consistency:

```
┌──────────────────────────────────────────────────────────────────┐
│               WORKSPACE CONSISTENCY LEVELS                       │
├──────────┬───────────────────────────────────────────────────────┤
│ Level    │ Description                                           │
├──────────┼───────────────────────────────────────────────────────┤
│ Atomic   │ All mutations within a single tool call are atomic.  │
│          │ If the tool fails, no partial state is left.         │
├──────────┼───────────────────────────────────────────────────────┤
│ Task     │ All mutations within a task are tracked. On task     │
│          │ failure, the worktree can be restored atomically.    │
├──────────┼───────────────────────────────────────────────────────┤
│ Session  │ Across multiple tasks in a session, checkpoints      │
│          │ enable time-travel to any consistent state.          │
└──────────┴───────────────────────────────────────────────────────┘
```

### 10.2 Atomic File Writes

```rust
/// Atomic file write: write to temp file, then rename.
/// On crash, either the old or new content is present — never partial.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let temp_path = path.with_extension(".xaft-tmp");

    // Write to temporary file
    tokio::fs::write(&temp_path, content).await?;

    // fsync to ensure data is on disk
    let file = tokio::fs::File::open(&temp_path).await?;
    file.sync_all().await?;

    // Atomic rename (POSIX guarantees atomicity on same filesystem)
    tokio::fs::rename(&temp_path, path).await?;

    Ok(())
}
```

### 10.3 Consistency Verification

After every checkpoint or restore, xaft verifies workspace consistency:

```rust
pub struct ConsistencyVerifier {
    /// Hashes of all files at last known consistent state
    file_hashes: HashMap<PathBuf, blake3::Hash>,
}

impl ConsistencyVerifier {
    /// Snapshot current workspace file hashes.
    pub async fn snapshot(&mut self, workspace: &WorkspaceStore) -> Result<()> {
        self.file_hashes.clear();
        let mut entries = tokio::fs::read_dir(workspace.root()).await?;
        while let Some(entry) = entries.next_entry().await? {
            // Recursively hash all files (excluding .git, .xaft)
            self.hash_tree(entry.path()).await?;
        }
        Ok(())
    }

    /// Verify workspace matches the last snapshot.
    pub async fn verify(&self, workspace: &WorkspaceStore) -> Result<VerificationReport> {
        let mut report = VerificationReport::default();
        for (path, expected_hash) in &self.file_hashes {
            let actual = self.hash_file(path).await.ok();
            match actual {
                Some(h) if &h == expected_hash => report.matching += 1,
                Some(h) => report.modified.push(FileDiff {
                    path: path.clone(),
                    expected: expected_hash.to_string(),
                    actual: h.to_string(),
                }),
                None => report.deleted.push(path.clone()),
            }
        }
        report.consistent = report.modified.is_empty() && report.deleted.is_empty();
        Ok(report)
    }
}
```

---

## 11. Error Reporting to Agent

When errors occur, the agent receives structured error information that it can
use to re-plan:

```rust
/// Structured error result returned to the agent for re-planning.
pub struct AgentErrorContext {
    /// What operation failed
    pub operation: String,
    /// Categorized error
    pub error_category: ErrorCategory,
    /// Human-readable error message
    pub message: String,
    /// Suggested recovery actions the agent can take
    pub suggestions: Vec<RecoverySuggestion>,
    /// Whether retrying the same operation might succeed
    pub retry_might_help: bool,
    /// Number of retries already attempted
    pub retries_attempted: u32,
}

#[derive(Debug)]
pub enum ErrorCategory {
    LlmError,
    ToolError,
    NetworkError,
    Timeout,
    Cancellation,
    SandboxViolation,
    BudgetExceeded,
}

#[derive(Debug)]
pub enum RecoverySuggestion {
    RetryWithBackoff,
    UseFallbackProvider,
    SimplifyRequest,
    ReduceContext,
    TryAlternativeTool,
    SkipAndContinue,
    AbortAndReport,
    RestoreCheckpoint,
}
```

---

## 12. Configuration

```toml
[recovery]
# Auto-checkpoint every N tool calls
auto_checkpoint_interval = 5

[recovery.llm]
max_retries = 3
base_delay_ms = 500
max_delay_ms = 30000
jitter = 0.25

[recovery.llm.providers]
primary = { name = "openai", model = "gpt-4o", priority = 0 }
fallback = [{ name = "anthropic", model = "claude-sonnet-4-20250514", priority = 1 }]

[recovery.llm.circuit_breaker]
failure_threshold = 5
reset_timeout_secs = 60

[recovery.tools]
default_error_policy = "return_error"

[recovery.tools.overrides]
delete_file = "raise"
shell_exec = "raise"
net_request = "skip"
read_file = "return_error"

[recovery.tools.retry]
max_retries = 2
delay_ms = 1000

[recovery.network]
connect_timeout_secs = 10
read_timeout_secs = 30
max_retries = 3

[recovery.cancellation]
# On cancellation: "checkpoint" (save state for resume) or "restore" (discard changes)
on_cancel = "checkpoint"
# Grace period for cleanup before forced termination
grace_period_secs = 5

[recovery.consistency]
verify_after_restore = true
verify_after_checkpoint = true
hash_algorithm = "blake3"
```

---

## 13. Open Questions

| # | Question | Status |
|---|----------|--------|
| 1 | Should checkpoints be stored locally or in a shared store? | Open |
| 2 | Maximum number of checkpoints per task before pruning? | Open |
| 3 | How to handle partial writes during crash (kernel-level, not app-level)? | Open |
| 4 | Should FallbackProvider support model-downgrade fallbacks (e.g., GPT-4 → GPT-3.5)? | Planned |
| 5 | Conflict resolution when resuming a checkpoint but the workspace has changed? | Open |
| 6 | Distributed task recovery (if xaft runs across multiple machines)? | Deferred |
| 7 | Automatic error categorization via LLM analysis? | Deferred |
