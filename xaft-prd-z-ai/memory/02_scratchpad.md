# PRD: Scratchpad

> xaft — Autonomous Coding CLI built on agtrs
> Document: `memory/02_scratchpad.md`
> Version: 0.1.0-draft

---

## 1. Overview

The **Scratchpad** is a persistent, cross-turn key-value store scoped to a
`run_id`. It serves as the agent's working memory — a place to jot down
intermediate results, plan fragments, computed values, and contextual notes
that must survive across LLM turns within a single task run. Unlike
`ShortTermMemory` (which auto-evicts) or `MemoryFact` (which is extracted
knowledge), the Scratchpad is **explicitly managed** by the agent: it writes
what it needs and reads what it stored.

Key properties:
- **Cross-turn persistence** — values survive from one LLM turn to the next
- **Scoped to run_id** — each task run gets an isolated scratchpad
- **Key-value interface** — simple, familiar, and deterministic
- **Agent read/write** — the agent decides what to store and retrieve
- **Planning integration** — the planner uses the scratchpad for plan state
- **Survives task suspension** — persisted to disk, not just in-memory

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                    xaft SCRATCHPAD SYSTEM                        │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │                    ScratchpadStore                         │  │
│  │  (key-value store scoped to run_id)                       │  │
│  │                                                            │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  │  │
│  │  │ Key:     │  │ Key:     │  │ Key:     │  │ Key:     │  │  │
│  │  │ plan     │  │ current_ │  │ partial_ │  │ decision │  │  │
│  │  │          │  │ step     │  │ results  │  │ log      │  │  │
│  │  │ Value:   │  │ Value:   │  │ Value:   │  │ Value:   │  │  │
│  │  │ {JSON}   │  │ 3        │  │ {JSON}   │  │ [entries]│  │  │
│  │  └──────────┘  └──────────┘  └──────────┘  └──────────┘  │  │
│  └────────────────────────────────────────────────────────────┘  │
│                            │                                     │
│            ┌───────────────┼───────────────┐                     │
│            │               │               │                     │
│     ┌──────▼──────┐ ┌─────▼──────┐ ┌──────▼──────┐             │
│     │  Agent      │ │  Planner   │ │  Checkpoint │             │
│     │  (read/     │ │  (plan     │ │  Manager    │             │
│     │   write)    │ │   state)   │ │  (snapshot/ │             │
│     │             │ │            │ │   restore)  │             │
│     └─────────────┘ └────────────┘ └─────────────┘             │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │                Persistence Layer                            │  │
│  │  .xaft/scratchpads/{run_id}.json                           │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

---

## 3. Scratchpad Data Model

### 3.1 Core Types

```rust
/// A single scratchpad entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadEntry {
    /// Unique key within the scratchpad namespace
    pub key: ScratchKey,
    /// The stored value (arbitrary JSON)
    pub value: serde_json::Value,
    /// When this entry was created
    pub created_at: DateTime<Utc>,
    /// When this entry was last updated
    pub updated_at: DateTime<Utc>,
    /// Number of times this entry has been updated
    pub version: u64,
    /// Optional type hint for the value
    pub type_hint: Option<ScratchpadType>,
    /// Whether this entry is pinned (never auto-evicted)
    pub pinned: bool,
    /// Optional expiration (entries are soft-deleted after this)
    pub expires_at: Option<DateTime<Utc>>,
    /// Origin of this entry
    pub origin: EntryOrigin,
    /// Token estimate for budget tracking
    pub token_estimate: usize,
}

/// Strongly-typed scratchpad key.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScratchKey(String);

impl ScratchKey {
    /// Well-known keys used by the xaft system itself.
    pub const PLAN: &'static str = "xaft:plan";
    pub const CURRENT_STEP: &'static str = "xaft:current_step";
    pub const DECISION_LOG: &'static str = "xaft:decision_log";
    pub const FILES_MODIFIED: &'static str = "xaft:files_modified";
    pub const TASK_CONTEXT: &'static str = "xaft:task_context";
    pub const AGENT_NOTES: &'static str = "xaft:agent_notes";
    pub const PARTIAL_RESULTS: &'static str = "xaft:partial_results";
    pub const ERROR_HISTORY: &'static str = "xaft:error_history";

    pub fn new(key: impl Into<String>) -> Self {
        let key = key.into();
        // Validate key format
        assert!(
            key.len() <= 256,
            "Scratchpad key too long: {} bytes (max 256)",
            key.len()
        );
        assert!(
            key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '-' || c == '.'),
            "Invalid scratchpad key: '{}'. Only alphanumeric, '_', ':', '-', '.' allowed.",
            key
        );
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Type hints for scratchpad values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScratchpadType {
    String,
    Number,
    Boolean,
    Array,
    Object,
    Plan,
    StepIndex,
    FileList,
    DecisionLog,
    ErrorLog,
    Custom { name: String },
}

/// Origin of a scratchpad entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryOrigin {
    /// Written by the agent explicitly
    Agent,
    /// Written by the planner subsystem
    Planner,
    /// Written by the checkpoint system during restore
    CheckpointRestore,
    /// Written by a tool
    Tool { tool_name: String },
    /// Written by the user via CLI
    User,
}
```

### 3.2 Scratchpad Store

```rust
/// The main scratchpad store, scoped to a run_id.
pub struct ScratchpadStore {
    /// The run_id this scratchpad belongs to
    run_id: RunId,
    /// In-memory cache of entries
    entries: DashMap<ScratchKey, ScratchpadEntry>,
    /// Persistence backend
    backend: Box<dyn ScratchpadBackend>,
    /// Configuration
    config: ScratchpadConfig,
    /// Token budget tracker
    token_budget: TokenBudgetTracker,
}

#[derive(Debug, Clone)]
pub struct ScratchpadConfig {
    /// Maximum number of entries
    pub max_entries: usize,
    /// Maximum token budget across all entries
    pub max_total_tokens: usize,
    /// Maximum size of a single value (bytes)
    pub max_value_size: usize,
    /// Whether to auto-persist on every write
    pub auto_persist: bool,
    /// Whether to persist synchronously or asynchronously
    pub sync_persist: bool,
    /// Default TTL for entries (None = no expiration)
    pub default_ttl: Option<Duration>,
}

pub struct TokenBudgetTracker {
    /// Current token usage
    usage: AtomicUsize,
    /// Maximum allowed tokens
    budget: usize,
}

impl TokenBudgetTracker {
    pub fn try_allocate(&self, tokens: usize) -> Result<(), ScratchpadError> {
        loop {
            let current = self.usage.load(Ordering::Relaxed);
            if current + tokens > self.budget {
                return Err(ScratchpadError::TokenBudgetExceeded {
                    current,
                    requested: tokens,
                    budget: self.budget,
                });
            }
            if self.usage.compare_exchange(
                current,
                current + tokens,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ).is_ok() {
                return Ok(());
            }
        }
    }

    pub fn release(&self, tokens: usize) {
        self.usage.fetch_sub(tokens, Ordering::Relaxed);
    }
}
```

---

## 4. Scratchpad API

### 4.1 Read/Write Operations

```rust
impl ScratchpadStore {
    /// Create a new scratchpad store for a run.
    pub async fn new(
        run_id: RunId,
        backend: Box<dyn ScratchpadBackend>,
        config: ScratchpadConfig,
    ) -> Result<Self> {
        let store = Self {
            run_id,
            entries: DashMap::new(),
            backend,
            config,
            token_budget: TokenBudgetTracker {
                usage: AtomicUsize::new(0),
                budget: config.max_total_tokens,
            },
        };

        // Load existing entries from persistence
        let persisted = store.backend.load(&run_id).await?;
        for entry in persisted {
            store.token_budget.try_allocate(entry.token_estimate)?;
            store.entries.insert(entry.key.clone(), entry);
        }

        Ok(store)
    }

    /// Write a value to the scratchpad.
    pub async fn set(
        &self,
        key: ScratchKey,
        value: serde_json::Value,
        origin: EntryOrigin,
    ) -> Result<(), ScratchpadError> {
        // Validate value size
        let value_str = serde_json::to_string(&value)
            .map_err(|e| ScratchpadError::SerializationError { source: e })?;
        if value_str.len() > self.config.max_value_size {
            return Err(ScratchpadError::ValueTooLarge {
                size: value_str.len(),
                max: self.config.max_value_size,
            });
        }

        let token_estimate = estimate_tokens(&value_str);

        // Handle upsert: if key exists, release old tokens first
        if let Some(mut existing) = self.entries.get_mut(&key) {
            self.token_budget.release(existing.token_estimate);
            self.token_budget.try_allocate(token_estimate)?;

            existing.value = value;
            existing.updated_at = Utc::now();
            existing.version += 1;
            existing.token_estimate = token_estimate;
            existing.origin = origin;
        } else {
            // Check entry count limit
            if self.entries.len() >= self.config.max_entries {
                return Err(ScratchpadError::EntryLimitExceeded {
                    current: self.entries.len(),
                    max: self.config.max_entries,
                });
            }

            self.token_budget.try_allocate(token_estimate)?;

            let entry = ScratchpadEntry {
                key: key.clone(),
                value,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                version: 1,
                type_hint: None,
                pinned: false,
                expires_at: self.config.default_ttl.map(|d| Utc::now() + d),
                origin,
                token_estimate,
            };
            self.entries.insert(key, entry);
        }

        // Auto-persist
        if self.config.auto_persist {
            self.persist().await?;
        }

        Ok(())
    }

    /// Read a value from the scratchpad.
    pub async fn get(
        &self,
        key: &ScratchKey,
    ) -> Result<Option<serde_json::Value>, ScratchpadError> {
        let entry = self.entries.get(key);
        match entry {
            Some(e) => {
                // Check expiration
                if let Some(expires) = e.expires_at {
                    if Utc::now() > expires {
                        // Soft-delete expired entry
                        drop(entry);
                        self.delete(key).await?;
                        return Ok(None);
                    }
                }
                Ok(Some(e.value.clone()))
            }
            None => Ok(None),
        }
    }

    /// Delete a value from the scratchpad.
    pub async fn delete(&self, key: &ScratchKey) -> Result<(), ScratchpadError> {
        if let Some((_, entry)) = self.entries.remove(key) {
            self.token_budget.release(entry.token_estimate);
        }
        if self.config.auto_persist {
            self.persist().await?;
        }
        Ok(())
    }

    /// List all keys in the scratchpad.
    pub fn keys(&self) -> Vec<ScratchKey> {
        self.entries.iter().map(|e| e.key.clone()).collect()
    }

    /// List all entries (for debugging and checkpoint).
    pub fn entries(&self) -> Vec<ScratchpadEntry> {
        self.entries.iter().map(|e| e.value.clone()).collect()
    }

    /// Get metadata about a key without reading the full value.
    pub fn metadata(&self, key: &ScratchKey) -> Option<ScratchpadMetadata> {
        self.entries.get(key).map(|e| ScratchpadMetadata {
            key: e.key.clone(),
            version: e.version,
            created_at: e.created_at,
            updated_at: e.updated_at,
            token_estimate: e.token_estimate,
            pinned: e.pinned,
            type_hint: e.type_hint.clone(),
        })
    }

    /// Persist the entire scratchpad to the backend.
    pub async fn persist(&self) -> Result<(), ScratchpadError> {
        let entries: Vec<ScratchpadEntry> = self.entries.iter()
            .map(|e| e.value.clone())
            .collect();
        self.backend.save(&self.run_id, &entries).await
    }
}

#[derive(Debug, Clone)]
pub struct ScratchpadMetadata {
    pub key: ScratchKey,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub token_estimate: usize,
    pub pinned: bool,
    pub type_hint: Option<ScratchpadType>,
}
```

### 4.2 Typed Accessors

```rust
impl ScratchpadStore {
    /// Set a string value.
    pub async fn set_string(
        &self,
        key: ScratchKey,
        value: String,
        origin: EntryOrigin,
    ) -> Result<(), ScratchpadError> {
        self.set(key, serde_json::Value::String(value), origin).await
    }

    /// Get a string value.
    pub async fn get_string(&self, key: &ScratchKey) -> Result<Option<String>, ScratchpadError> {
        match self.get(key).await? {
            Some(serde_json::Value::String(s)) => Ok(Some(s)),
            Some(other) => Ok(Some(other.to_string())),
            None => Ok(None),
        }
    }

    /// Set a numeric value.
    pub async fn set_number(
        &self,
        key: ScratchKey,
        value: f64,
        origin: EntryOrigin,
    ) -> Result<(), ScratchpadError> {
        self.set(key, serde_json::json!(value), origin).await
    }

    /// Get a numeric value.
    pub async fn get_number(&self, key: &ScratchKey) -> Result<Option<f64>, ScratchpadError> {
        match self.get(key).await? {
            Some(serde_json::Value::Number(n)) => Ok(n.as_f64()),
            Some(other) => {
                // Try to parse string as number
                if let Some(s) = other.as_str() {
                    Ok(s.parse().ok())
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Append to a list value (creates the list if it doesn't exist).
    pub async fn list_append(
        &self,
        key: ScratchKey,
        item: serde_json::Value,
        origin: EntryOrigin,
    ) -> Result<(), ScratchpadError> {
        let mut list = match self.get(&key).await? {
            Some(serde_json::Value::Array(arr)) => arr,
            Some(_) => return Err(ScratchpadError::TypeMismatch {
                key: key.as_str().to_string(),
                expected: "array",
            }),
            None => Vec::new(),
        };
        list.push(item);
        self.set(key, serde_json::Value::Array(list), origin).await
    }

    /// Increment a counter value.
    pub async fn increment(
        &self,
        key: ScratchKey,
        delta: i64,
        origin: EntryOrigin,
    ) -> Result<i64, ScratchpadError> {
        let current = match self.get_number(&key).await? {
            Some(n) => n as i64,
            None => 0,
        };
        let new_value = current + delta;
        self.set_number(key, new_value as f64, origin).await?;
        Ok(new_value)
    }
}
```

---

## 5. Cross-Turn Persistence

### 5.1 Persistence Lifecycle

The scratchpad is persisted to disk at `.xaft/scratchpads/{run_id}.json`
and survives across LLM turns within the same task run.

```
Task Run Start (run_id = abc123)
        │
        ▼
┌───────────────────────────────────┐
│ ScratchpadStore::new()            │
│ • Create or load from disk        │
│ • Initialize empty if new         │
│ • Restore from persistence if     │
│   resuming after crash            │
└───────────────┬───────────────────┘
                │
                ▼
        ┌───────────────┐
        │  LLM Turn 1   │
        │               │
        │  Agent writes:│
        │  • plan       │
        │  • step_idx=0 │
        │  • notes      │
        │               │
        │  → auto-persist│
        └───────┬───────┘
                │
                ▼
        ┌───────────────┐
        │  LLM Turn 2   │
        │               │
        │  Agent reads: │
        │  • plan ✓     │
        │  • step_idx=0 ✓│
        │               │
        │  Agent writes:│
        │  • step_idx=1 │
        │  • results    │
        │               │
        │  → auto-persist│
        └───────┬───────┘
                │
                ▼
        ┌───────────────┐
        │  LLM Turn N   │
        │               │
        │  Agent reads: │
        │  • plan ✓     │
        │  • step_idx=N-1│
        │  • results ✓  │
        │               │
        │  All data     │
        │  from Turn 1  │
        │  still        │
        │  available ✓  │
        └───────┬───────┘
                │
                ▼
        Task Run End
        Scratchpad persists
        for checkpoint recovery
```

### 5.2 Persistence Backend

```rust
/// Backend trait for scratchpad persistence.
#[async_trait]
pub trait ScratchpadBackend: Send + Sync {
    /// Save scratchpad entries for a run.
    async fn save(&self, run_id: &RunId, entries: &[ScratchpadEntry]) -> Result<(), ScratchpadError>;

    /// Load scratchpad entries for a run.
    async fn load(&self, run_id: &RunId) -> Result<Vec<ScratchpadEntry>, ScratchpadError>;

    /// Delete the scratchpad for a run.
    async fn delete(&self, run_id: &RunId) -> Result<(), ScratchpadError>;

    /// List all run_ids with persisted scratchpads.
    async fn list_runs(&self) -> Result<Vec<RunId>, ScratchpadError>;
}

/// File-based persistence backend.
pub struct FileScratchpadBackend {
    base_dir: PathBuf,
}

#[async_trait]
impl ScratchpadBackend for FileScratchpadBackend {
    async fn save(&self, run_id: &RunId, entries: &[ScratchpadEntry]) -> Result<(), ScratchpadError> {
        let path = self.base_dir.join(format!("{}.json", run_id));
        let parent = path.parent().unwrap();

        tokio::fs::create_dir_all(parent).await?;

        // Atomic write: write to temp, then rename
        let temp_path = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(entries)
            .map_err(|e| ScratchpadError::SerializationError { source: e })?;
        tokio::fs::write(&temp_path, &json).await?;
        tokio::fs::rename(&temp_path, &path).await?;

        Ok(())
    }

    async fn load(&self, run_id: &RunId) -> Result<Vec<ScratchpadEntry>, ScratchpadError> {
        let path = self.base_dir.join(format!("{}.json", run_id));
        if !path.exists() {
            return Ok(vec![]);
        }
        let json = tokio::fs::read_to_string(&path).await?;
        let entries: Vec<ScratchpadEntry> = serde_json::from_str(&json)
            .map_err(|e| ScratchpadError::DeserializationError { source: e })?;
        Ok(entries)
    }

    async fn delete(&self, run_id: &RunId) -> Result<(), ScratchpadError> {
        let path = self.base_dir.join(format!("{}.json", run_id));
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn list_runs(&self) -> Result<Vec<RunId>, ScratchpadError> {
        let mut runs = Vec::new();
        let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(run_id) = stem.parse::<RunId>() {
                        runs.push(run_id);
                    }
                }
            }
        }
        Ok(runs)
    }
}
```

---

## 6. Agent Read/Write Integration

### 6.1 Scratchpad Tool

The agent accesses the scratchpad through dedicated tools:

```rust
/// Read a value from the agent's scratchpad.
#[tool(
    name = "scratchpad_get",
    description = "Read a value from the scratchpad. The scratchpad is a persistent \
                   key-value store that survives across conversation turns within a task."
)]
async fn scratchpad_get(
    ctx: &ToolContext,
    #[desc("Key to read")] key: String,
) -> Result<ToolOutput, ToolError> {
    let scratch_key = ScratchKey::new(&key)
        .map_err(|e| ToolError::InvalidArguments { reason: e.to_string() })?;

    match ctx.scratchpad.get(&scratch_key).await? {
        Some(value) => {
            let formatted = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| value.to_string());
            Ok(ToolOutput::text(formatted))
        }
        None => Ok(ToolOutput::text(format!("No value found for key: {}", key))),
    }
}

/// Write a value to the agent's scratchpad.
#[requires_confirmation(risk = "low", reason = "Modifying scratchpad state")]
#[tool(
    name = "scratchpad_set",
    description = "Write a value to the scratchpad. Values persist across turns."
)]
async fn scratchpad_set(
    ctx: &ToolContext,
    #[desc("Key to write")] key: String,
    #[desc("Value to store (JSON)")] value: String,
) -> Result<ToolOutput, ToolError> {
    let scratch_key = ScratchKey::new(&key)
        .map_err(|e| ToolError::InvalidArguments { reason: e.to_string() })?;

    let json_value: serde_json::Value = serde_json::from_str(&value)
        .unwrap_or(serde_json::Value::String(value.clone()));

    ctx.scratchpad.set(scratch_key, json_value, EntryOrigin::Agent).await?;
    Ok(ToolOutput::text(format!("Set scratchpad key: {}", key)))
}

/// List all keys in the agent's scratchpad.
#[tool(
    name = "scratchpad_keys",
    description = "List all keys currently in the scratchpad."
)]
async fn scratchpad_keys(ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let keys = ctx.scratchpad.keys();
    let output = keys.iter()
        .map(|k| format!("  {} (v{})", k.as_str(), {
            // Show version if available
            ctx.scratchpad.metadata(k).map(|m| m.version).unwrap_or(0)
        }))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ToolOutput::text(format!("Scratchpad keys ({} total):\n{}", keys.len(), output)))
}

/// Delete a key from the scratchpad.
#[requires_confirmation(risk = "low", reason = "Removing scratchpad data")]
#[tool(
    name = "scratchpad_delete",
    description = "Delete a key from the scratchpad."
)]
async fn scratchpad_delete(
    ctx: &ToolContext,
    #[desc("Key to delete")] key: String,
) -> Result<ToolOutput, ToolError> {
    let scratch_key = ScratchKey::new(&key)
        .map_err(|e| ToolError::InvalidArguments { reason: e.to_string() })?;

    ctx.scratchpad.delete(&scratch_key).await?;
    Ok(ToolOutput::text(format!("Deleted scratchpad key: {}", key)))
}
```

### 6.2 Automatic Scratchpad Injection

The agent doesn't always need to explicitly call `scratchpad_get`. The system
automatically injects relevant scratchpad contents into the LLM prompt:

```rust
/// Inject scratchpad context into the LLM prompt.
pub fn inject_scratchpad_context(
    prompt: &mut LlmPrompt,
    scratchpad: &ScratchpadStore,
) {
    let keys = scratchpad.keys();
    if keys.is_empty() {
        return;
    }

    let mut context = String::from("## Scratchpad (persistent working memory)\n");
    context.push_str("You can read from and write to the scratchpad using the ");
    context.push_str("scratchpad_get/scratchpad_set tools.\n\n");
    context.push_str("Current scratchpad contents:\n");

    for key in &keys {
        if let Some(metadata) = scratchpad.metadata(key) {
            // Only show key names and metadata in the prompt to save tokens
            context.push_str(&format!(
                "- `{}` (v{}, {} tokens, {})\n",
                key.as_str(),
                metadata.version,
                metadata.token_estimate,
                metadata.type_hint
                    .as_ref()
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "untyped".to_string()),
            ));
        }
    }

    context.push_str("\nUse `scratchpad_get` to read any key's full value.\n");
    prompt.add_system_context(&context);
}
```

---

## 7. Planning Integration

### 7.1 Plan State on the Scratchpad

The planner subsystem uses well-known scratchpad keys to store and manage
plan state. This allows the agent and the planner to share a consistent view
of progress.

```rust
/// Planner integration with the scratchpad.
pub struct ScratchpadPlanner {
    scratchpad: Arc<ScratchpadStore>,
}

impl ScratchpadPlanner {
    /// Store the plan in the scratchpad.
    pub async fn store_plan(&self, plan: &Plan) -> Result<(), ScratchpadError> {
        self.scratchpad.set(
            ScratchKey::new(ScratchKey::PLAN)?,
            serde_json::to_value(plan)?,
            EntryOrigin::Planner,
        ).await
    }

    /// Read the current plan from the scratchpad.
    pub async fn load_plan(&self) -> Result<Option<Plan>, ScratchpadError> {
        let value = self.scratchpad.get(&ScratchKey::new(ScratchKey::PLAN)?).await?;
        match value {
            Some(v) => Ok(Some(serde_json::from_value(v)?)),
            None => Ok(None),
        }
    }

    /// Update the current step index.
    pub async fn advance_step(&self) -> Result<usize, ScratchpadError> {
        let new_idx = self.scratchpad.increment(
            ScratchKey::new(ScratchKey::CURRENT_STEP)?,
            1,
            EntryOrigin::Planner,
        ).await?;
        Ok(new_idx as usize)
    }

    /// Get the current step index.
    pub async fn current_step(&self) -> Result<usize, ScratchpadError> {
        let value = self.scratchpad.get_number(
            &ScratchKey::new(ScratchKey::CURRENT_STEP)?
        ).await?;
        Ok(value.unwrap_or(0.0) as usize)
    }

    /// Log a decision made during plan execution.
    pub async fn log_decision(
        &self,
        step: usize,
        decision: &str,
        rationale: &str,
    ) -> Result<(), ScratchpadError> {
        let entry = serde_json::json!({
            "step": step,
            "decision": decision,
            "rationale": rationale,
            "timestamp": Utc::now().to_rfc3339(),
        });

        self.scratchpad.list_append(
            ScratchKey::new(ScratchKey::DECISION_LOG)?,
            entry,
            EntryOrigin::Planner,
        ).await
    }

    /// Track a file modification.
    pub async fn track_file_modification(
        &self,
        path: &str,
        operation: &str,
    ) -> Result<(), ScratchpadError> {
        let entry = serde_json::json!({
            "path": path,
            "operation": operation,
            "timestamp": Utc::now().to_rfc3339(),
        });

        self.scratchpad.list_append(
            ScratchKey::new(ScratchKey::FILES_MODIFIED)?,
            entry,
            EntryOrigin::Planner,
        ).await
    }
}
```

### 7.2 Plan Execution Flow with Scratchpad

```
┌──────────────┐     ┌───────────────────────┐     ┌──────────────┐
│ Agent        │────▶│ ScratchpadPlanner     │────▶│ Plan stored  │
│ generates    │     │ .store_plan()         │     │ in scratchpad│
│ plan         │     │ .set current_step=0   │     │              │
└──────────────┘     └───────────────────────┘     └──────┬───────┘
                                                          │
                    ┌─────────────────────────────────────┘
                    │
                    ▼
            ┌──────────────┐
            │  LLM Turn N  │
            │              │
            │  1. Read plan│
            │  2. Read step│
            │  3. Execute  │
            │  4. Log      │
            │     decision │
            │  5. Track    │
            │     files    │
            │  6. Advance  │
            │     step     │
            └──────┬───────┘
                   │
                   ▼
            ┌──────────────┐
            │  LLM Turn N+1│
            │              │
            │  Reads:      │
            │  • plan ✓    │
            │  • step=N ✓  │
            │  • decisions ✓│
            │  • files ✓   │
            │              │
            │  Full context│
            │  preserved ✓ │
            └──────────────┘
```

---

## 8. Surviving Task Suspension

### 8.1 Problem

Tasks can be suspended for many reasons:
- User pauses the task (`Ctrl+Z` or `xaft pause`)
- System sleep / hibernate
- Preemption by a higher-priority task
- Network disconnection
- Process crash and restart

When the task resumes, the scratchpad must be fully intact.

### 8.2 Suspension Lifecycle

```
┌────────────────────┐     ┌───────────────────┐     ┌──────────────────┐
│ Task running       │────▶│ Suspension signal  │────▶│ Scratchpad::     │
│                    │     │ received           │     │ persist()        │
│ Scratchpad active  │     │                    │     │ (sync to disk)   │
└────────────────────┘     └───────────────────┘     └────────┬─────────┘
                                                              │
                     ┌────────────────────────────────────────┘
                     │
                     ▼
            ┌─────────────────┐
            │ Task suspended  │
            │                 │
            │ Scratchpad      │
            │ persisted at:   │
            │ .xaft/scratch-  │
            │ pads/{run}.json │
            │                 │
            │ Process may     │
            │ exit here       │
            └────────┬────────┘
                     │
                     ▼
            ┌──────────────────┐
            │ Task resumes     │
            │                  │
            │ ScratchpadStore::│
            │ new(run_id)      │
            │                  │
            │ • Load from disk │
            │ • Restore all    │
            │   entries        │
            │ • Rebuild token  │
            │   budget         │
            │ • Agent reads    │
            │   plan, step,   │
            │   decisions...  │
            │   All intact ✓  │
            └──────────────────┘
```

### 8.3 Emergency Persistence

On process termination (SIGTERM, SIGINT), the scratchpad performs an
emergency synchronous persist:

```rust
/// Register emergency persist on process signals.
pub fn register_emergency_persist(scratchpad: Arc<ScratchpadStore>) {
    ctrlc::set_handler(move || {
        // Synchronous emergency persist — must complete before exit
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            if let Err(e) = scratchpad.emergency_persist().await {
                eprintln!("Emergency scratchpad persist failed: {}", e);
            }
        });
        std::process::exit(0);
    }).expect("Failed to set Ctrl+C handler");
}

impl ScratchpadStore {
    /// Emergency synchronous persist — used during process shutdown.
    pub async fn emergency_persist(&self) -> Result<(), ScratchpadError> {
        // Bypass async_persist and do a blocking write
        let entries: Vec<ScratchpadEntry> = self.entries.iter()
            .map(|e| e.value.clone())
            .collect();
        self.backend.save(&self.run_id, &entries).await
    }
}
```

### 8.4 Checkpoint Integration

The checkpoint manager snapshots the scratchpad as part of every checkpoint:

```rust
impl CheckpointManager {
    pub async fn create_checkpoint(&self, state: &TaskState) -> Result<Checkpoint> {
        // ... other checkpoint fields ...

        // Snapshot the entire scratchpad
        let scratchpad_entries = state.scratchpad.entries();

        Ok(Checkpoint {
            // ... other fields ...
            scratchpad: scratchpad_entries.iter()
                .map(|e| (e.key.as_str().to_string(), e.value.clone()))
                .collect(),
        })
    }

    pub async fn restore_from_checkpoint(
        &self,
        checkpoint: &Checkpoint,
        scratchpad: &ScratchpadStore,
    ) -> Result<()> {
        // Clear existing scratchpad
        let keys = scratchpad.keys();
        for key in &keys {
            scratchpad.delete(key).await?;
        }

        // Restore from checkpoint
        for (key, value) in &checkpoint.scratchpad {
            scratchpad.set(
                ScratchKey::new(key)?,
                value.clone(),
                EntryOrigin::CheckpointRestore,
            ).await?;
        }

        Ok(())
    }
}
```

---

## 9. Scratchpad Scoping and Namespacing

### 9.1 Key Namespacing Convention

Keys are namespaced with colon-separated prefixes to avoid collisions:

```
┌─────────────────────────────────────────────────────────────────┐
│              SCRATCHPAD KEY NAMESPACING                         │
├────────────────────┬────────────────────────────────────────────┤
│ Namespace          │ Purpose                               Example│
├────────────────────┼────────────────────────────────────────────┤
│ xaft:              │ System-reserved keys                  xaft:plan│
│                    │ (managed by xaft internals)           xaft:step│
│                    │                                       xaft:files│
├────────────────────┼────────────────────────────────────────────┤
│ plan:              │ Plan-specific data                    plan:step_3│
│                    │                                       plan:alt_1│
├────────────────────┼────────────────────────────────────────────┤
│ agent:             │ Agent's own notes and state           agent:notes│
│                    │                                       agent:ctx│
├────────────────────┼────────────────────────────────────────────┤
│ tool:{name}:       │ Tool-specific state                   tool:search:idx│
│                    │                                       tool:edit:buf│
├────────────────────┼────────────────────────────────────────────┤
│ user:              │ User-provided data                    user:prefs│
│                    │                                       user:notes│
├────────────────────┼────────────────────────────────────────────┤
│ temp:              │ Temporary data (auto-expiring)        temp:cache│
│                    │                                       temp:staging│
└────────────────────┴────────────────────────────────────────────┘
```

### 9.2 Cross-Run Isolation

Each `run_id` gets its own scratchpad file. Runs never share scratchpad data:

```
.xaft/scratchpads/
├── run_abc123.json     ← Task: "Refactor auth module"
├── run_def456.json     ← Task: "Add pagination to API"
├── run_ghi789.json     ← Task: "Fix flaky test"
└── ...
```

### 9.3 Cross-Run Knowledge Transfer

When the agent starts a new run, it can optionally import relevant data from
previous runs via the `MemoryFact` system (not the scratchpad directly):

```
┌─────────────────┐          ┌─────────────────┐
│ Run 1            │          │ Run 2            │
│ Scratchpad       │  extract │ FactStore        │
│ (ephemeral)      │─────────▶│ (persistent)     │
│                  │  facts   │                  │
│ xaft:plan        │          │ Fact: "prefers   │
│ xaft:step=5      │          │ tabs over spaces"│
│ agent:notes      │          │ Fact: "uses      │
│                  │          │ conventional     │
│                  │          │ commits"         │
└─────────────────┘          └────────┬──────────┘
                                      │
                              RAG injects
                              into Run 2
                              system prompt
                                      │
                                      ▼
                             ┌─────────────────┐
                             │ Run 2 Scratchpad │
                             │ (fresh, empty)   │
                             │                  │
                             │ Agent has access │
                             │ to persistent    │
                             │ facts via RAG    │
                             └─────────────────┘
```

---

## 10. Scratchpad Observation and Debugging

### 10.1 Real-Time Observation

Users can observe the scratchpad state in real-time via the CLI:

```bash
$ xaft scratchpad list --run abc123
  KEY                     VERSION  TOKENS  TYPE
  xaft:plan               1        342     Plan
  xaft:current_step       7        1       StepIndex
  xaft:decision_log       4        128     DecisionLog
  xaft:files_modified     3        56      FileList
  agent:notes             2        89      String

$ xaft scratchpad get --run abc123 --key xaft:current_step
  3

$ xaft scratchpad watch --run abc123
  [12:00:01] SET   xaft:current_step = 3 (v7)
  [12:00:05] SET   xaft:decision_log += {step: 3, decision: "use HashMap", ...}
  [12:00:12] SET   xaft:files_modified += {path: "src/auth.rs", operation: "modified"}
```

### 10.2 Scratchpad Diff Between Turns

```bash
$ xaft scratchpad diff --run abc123 --from-turn 5 --to-turn 6
  Changes between turn 5 and turn 6:

  MODIFIED  xaft:current_step: 2 → 3
  APPENDED  xaft:decision_log: [{step: 2, ...}, {step: 3, decision: "extract trait", ...}]
  APPENDED  xaft:files_modified: [{path: "src/auth.rs"}, {path: "src/auth/trait.rs"}]
  NEW       agent:notes: "Refactoring auth into trait + impl..."
```

---

## 11. Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum ScratchpadError {
    #[error("token budget exceeded: used {current}, need +{requested}, budget {budget}")]
    TokenBudgetExceeded {
        current: usize,
        requested: usize,
        budget: usize,
    },

    #[error("entry limit exceeded: {current}/{max}")]
    EntryLimitExceeded {
        current: usize,
        max: usize,
    },

    #[error("value too large: {size} bytes (max {max})")]
    ValueTooLarge {
        size: usize,
        max: usize,
    },

    #[error("type mismatch for key '{key}': expected {expected}")]
    TypeMismatch {
        key: String,
        expected: String,
    },

    #[error("serialization error: {source}")]
    SerializationError {
        source: serde_json::Error,
    },

    #[error("deserialization error: {source}")]
    DeserializationError {
        source: serde_json::Error,
    },

    #[error("persistence error: {0}")]
    PersistenceError(String),

    #[error("invalid key: {0}")]
    InvalidKey(String),
}
```

---

## 12. Configuration

```toml
[scratchpad]
# Maximum number of entries per run
max_entries = 200
# Maximum total tokens across all entries
max_total_tokens = 16000
# Maximum size of a single value (bytes)
max_value_size = 65536
# Auto-persist on every write
auto_persist = true
# Synchronous persist (slower but safer)
sync_persist = false
# Default TTL for entries (None = no expiration)
default_ttl = "24h"
# Persistence directory
persist_dir = ".xaft/scratchpads"

[scratchpad.well_known]
# System keys that are always pinned (never evicted)
pinned_keys = ["xaft:plan", "xaft:current_step", "xaft:decision_log", "xaft:files_modified"]

[scratchpad.cleanup]
# Automatically clean up scratchpads for completed runs
auto_cleanup = true
# Retention period for completed run scratchpads
retention = "7d"
```

---

## 13. Open Questions

| # | Question | Status |
|---|----------|--------|
| 1 | Should sub-agents (spawned tasks) share the parent's scratchpad? | Open |
| 2 | Scratchpad size limits — what's the right default token budget? | Open |
| 3 | Should the scratchpad support binary values (e.g., serialized ASTs)? | Deferred |
| 4 | Scratchpad compression for large values? | Open |
| 5 | Cross-machine scratchpad sync (for distributed xaft)? | Deferred |
| 6 | Should the agent be able to "export" scratchpad keys to MemoryFact? | Planned |
| 7 | Rate limiting on scratchpad writes (prevent agent write storms)? | Open |
| 8 | Scratchpad versioning — keep history of all changes for a key? | Open |
