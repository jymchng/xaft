# xaft-memory — Unified Memory Store PRD

## Problem

xaft has no persistent memory system. Agents cannot remember facts across sessions, learn from past interactions, or build project-specific knowledge. Each session starts from zero — the agent re-reads the same files, re-learns the same architecture, and re-discovers the same patterns every time.

The `agtrs` framework provides low-level memory primitives (`MemoryStore`, `UserMemoryStore`, `ConversationStore`) but these are thin vector-store interfaces — they lack structured metadata, scoping, relevance decay, garbage collection, and the typed domain model that xaft agents need.

## Goal

Create `crates/xaft-memory` — a dedicated crate for agent memory with:

- Typed `MemoryEntry` with metadata, scoping, embeddings, access tracking
- `MemoryStore` trait with CRUD, search, GC, and lifecycle management
- `InMemoryStore` for tests and ephemeral session-scoped memory
- `SqliteStore` for durable project/user-scoped memory
- `MemoryManager` that composes stores and provides the agent-facing API
- Integration with `XaftRuntime` via `with_memory_store()` injection
- Config section in `.xaft.toml` for backend selection and tuning

---

## Relationship to existing crates

```
agtrs-runtime::memory    ← low-level: MemoryStore (vector), ConversationStore, UserMemoryStore
xaft-memory (NEW)        ← domain:    MemoryEntry, MemoryStore trait, SqliteStore, InMemoryStore, MemoryManager
xaft-runtime             ← integration: imports from xaft-memory, injects into agents
xaft-agent               ← consumer: agents call MemoryManager via ToolContext
```

`xaft-memory` depends on `agtrs-runtime` (for `AgtrsError` compatibility), `serde`, `serde_json`, `chrono`, `uuid`, `sqlx` (SQLite).
`xaft-runtime` depends on `xaft-memory`.

---

## Crate layout

```
crates/xaft-memory/
├── Cargo.toml
└── src/
    ├── lib.rs           — re-exports; crate-level doc
    ├── types.rs         — EntryId, MemoryEntry, MemoryMetadata, MemorySource, MemoryScope
    ├── query.rs         — MemoryQuery, MemoryFilter, MemorySort, MemorySearchResult, TextHighlight
    ├── gc.rs            — GcPolicy, GcReport
    ├── error.rs         — MemoryError (thiserror)
    ├── store.rs         — MemoryStore trait
    ├── in_memory.rs     — InMemoryStore
    ├── sqlite.rs        — SqliteStore
    ├── manager.rs       — MemoryManager (composes stores, agent-facing API)
    └── config.rs        — MemoryConfig (for xaft-config integration)
```

---

## Module specifications

### `error.rs` — MemoryError

```rust
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("entry not found: {0}")]
    NotFound(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid query: {0}")]
    InvalidQuery(String),

    #[error("embedding error: {0}")]
    Embedding(String),
}
```

Follows xaft convention: thiserror enum, named variants, `#[from]` for automatic conversion.

### `types.rs` — Core types

```rust
/// Unique identifier for a memory entry.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntryId(Uuid);

impl EntryId {
    pub fn new() -> Self { Self(Uuid::new_v4()) }
    pub fn as_str(&self) -> String { self.0.to_string() }
    pub fn from_string(s: &str) -> Result<Self, uuid::Error> { Ok(Self(Uuid::parse_str(s)?)) }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: EntryId,
    pub key: String,
    pub value: serde_json::Value,
    pub metadata: MemoryMetadata,
    pub embedding: Option<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub access_count: u64,
    pub last_accessed: Option<DateTime<Utc>>,
    pub relevance_score: f64,
}

impl MemoryEntry {
    pub fn new(key: impl Into<String>, value: serde_json::Value) -> Self { ... }
    pub fn with_scope(mut self, scope: MemoryScope) -> Self { ... }
    pub fn with_tags(mut self, tags: Vec<String>) -> Self { ... }
    pub fn with_ttl(mut self, duration: Duration) -> Self { ... }
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self { ... }
    pub fn with_confidence(mut self, confidence: f64) -> Self { ... }
    pub fn is_expired(&self) -> bool { ... }
    pub fn touch(&mut self) { self.access_count += 1; self.last_accessed = Some(Utc::now()); }
}

/// Metadata attached to every memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub source: MemorySource,
    pub scope: MemoryScope,
    pub tags: Vec<String>,
    pub confidence: f64,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
}

impl Default for MemoryMetadata {
    fn default() -> Self {
        Self {
            source: MemorySource::AgentExtracted,
            scope: MemoryScope::Session,
            tags: Vec::new(),
            confidence: 0.8,
            user_id: None,
            project_id: None,
            task_id: None,
            session_id: None,
        }
    }
}

/// How the memory entry was created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    AgentExtracted,
    UserProvided,
    MacroExtracted,
    Imported { source: String },
}

/// Visibility scope of a memory entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Task,
    Session,
    Project,
    User,
}
```

### `query.rs` — Search types

```rust
/// A memory search query.
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    pub text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub tags: Vec<String>,
    pub scope: Option<MemoryScope>,
    pub source: Option<MemorySource>,
    pub limit: usize,
    pub min_relevance: f64,
    pub sort: MemorySort,
}

impl MemoryQuery {
    pub fn text(q: impl Into<String>) -> Self {
        Self { text: Some(q.into()), limit: 10, ..Default::default() }
    }
    pub fn with_limit(mut self, n: usize) -> Self { self.limit = n; self }
    pub fn with_scope(mut self, s: MemoryScope) -> Self { self.scope = Some(s); self }
    pub fn with_tags(mut self, t: Vec<String>) -> Self { self.tags = t; self }
}

/// Filter for listing entries (broader than search).
#[derive(Debug, Clone, Default)]
pub struct MemoryFilter {
    pub scope: Option<MemoryScope>,
    pub source: Option<MemorySource>,
    pub tags: Vec<String>,
    pub expired: Option<bool>,
    pub min_access_count: Option<u64>,
    pub created_after: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// Sort order for search results.
#[derive(Debug, Clone, Default)]
pub enum MemorySort {
    #[default]
    Relevance,
    Recency,
    AccessCount,
    Confidence,
}

/// A search result with score and highlights.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub entry: MemoryEntry,
    pub score: f64,
    pub highlights: Vec<TextHighlight>,
}

/// A highlighted text snippet from a search result.
#[derive(Debug, Clone)]
pub struct TextHighlight {
    pub field: String,
    pub snippet: String,
    pub start: usize,
    pub end: usize,
}
```

### `gc.rs` — Garbage collection

```rust
/// Policy for garbage collection.
#[derive(Debug, Clone)]
pub struct GcPolicy {
    /// Remove entries older than this duration.
    pub max_age: Option<Duration>,
    /// Remove entries with relevance below this threshold.
    pub min_relevance: f64,
    /// Remove entries not accessed in this duration.
    pub max_idle: Option<Duration>,
    /// Maximum entries to keep (0 = unlimited).
    pub max_entries: usize,
    /// Remove expired entries (expires_at < now).
    pub remove_expired: bool,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            max_age: Some(Duration::from_secs(30 * 24 * 3600)), // 30 days
            min_relevance: 0.1,
            max_idle: Some(Duration::from_secs(7 * 24 * 3600)), // 7 days
            max_entries: 0,
            remove_expired: true,
        }
    }
}

/// Report from a GC run.
#[derive(Debug, Clone)]
pub struct GcReport {
    pub entries_scanned: usize,
    pub entries_removed: usize,
    pub entries_kept: usize,
    pub bytes_freed: u64,
}
```

### `store.rs` — MemoryStore trait

```rust
/// Core memory store trait — all memory backends implement this.
#[async_trait]
pub trait MemoryStore: Send + Sync + 'static {
    /// Store a new entry. Returns the assigned EntryId.
    async fn store(&self, entry: MemoryEntry) -> Result<EntryId, MemoryError>;

    /// Retrieve an entry by ID. Returns None if not found.
    async fn retrieve(&self, id: &EntryId) -> Result<Option<MemoryEntry>, MemoryError>;

    /// Search memory with a query.
    async fn search(&self, query: &MemoryQuery) -> Result<Vec<MemorySearchResult>, MemoryError>;

    /// Update an existing entry (full replacement).
    async fn update(&self, id: &EntryId, entry: MemoryEntry) -> Result<(), MemoryError>;

    /// Delete an entry. Idempotent — returns Ok(()) if not found.
    async fn delete(&self, id: &EntryId) -> Result<(), MemoryError>;

    /// List entries matching a filter.
    async fn list(&self, filter: &MemoryFilter) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Garbage collect expired or low-relevance entries.
    async fn gc(&self, policy: &GcPolicy) -> Result<GcReport, MemoryError>;

    /// Return the store type identifier (e.g. "sqlite", "in-memory").
    fn store_type(&self) -> &str;
}
```

### `in_memory.rs` — InMemoryStore

```rust
/// In-memory store for tests and ephemeral session-scoped memory.
///
/// Uses `RwLock<HashMap<EntryId, MemoryEntry>>` for concurrent access.
/// Not persisted — data is lost when the process exits.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    entries: RwLock<HashMap<EntryId, MemoryEntry>>,
}

impl InMemoryStore {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    // store: insert into hashmap
    // retrieve: get by id, touch entry
    // search: linear scan with substring match on key/value, tag filtering, scope filtering
    //         score = text relevance * confidence * recency_decay
    // update: replace entry
    // delete: remove from hashmap
    // list: filter scan
    // gc: remove expired + low-relevance + idle entries
    // store_type: "in-memory"
}
```

### `sqlite.rs` — SqliteStore

```rust
/// SQLite-backed durable memory store.
///
/// Schema:
///   CREATE TABLE memory_entries (
///       id TEXT PRIMARY KEY,
///       key TEXT NOT NULL,
///       value TEXT NOT NULL,          -- JSON
///       metadata TEXT NOT NULL,       -- JSON
///       embedding BLOB,              -- f32 array as bytes
///       created_at TEXT NOT NULL,     -- ISO 8601
///       updated_at TEXT NOT NULL,
///       expires_at TEXT,
///       access_count INTEGER NOT NULL DEFAULT 0,
///       last_accessed TEXT,
///       relevance_score REAL NOT NULL DEFAULT 0.5
///   );
///   CREATE INDEX idx_memory_scope ON memory_entries(json_extract(metadata, '$.scope'));
///   CREATE INDEX idx_memory_tags ON memory_entries(json_extract(metadata, '$.tags'));
///   CREATE INDEX idx_memory_expires ON memory_entries(expires_at);
///   CREATE INDEX idx_memory_relevance ON memory_entries(relevance_score);
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Open or create a SQLite database at the given path.
    pub async fn new(path: &Path) -> Result<Self, MemoryError> { ... }

    /// Create an in-memory SQLite database (for tests).
    pub async fn in_memory() -> Result<Self, MemoryError> { ... }
}

#[async_trait]
impl MemoryStore for SqliteStore {
    // store: INSERT INTO memory_entries
    // retrieve: SELECT by id, UPDATE access_count + last_accessed
    // search: SELECT with WHERE clauses for scope/tags/source, LIKE for text
    //         cosine similarity for embedding search (if embedding column populated)
    // update: UPDATE by id
    // delete: DELETE by id
    // list: SELECT with filter WHERE clauses
    // gc: DELETE WHERE expires_at < now OR relevance_score < min OR last_accessed < max_idle
    // store_type: "sqlite"
}
```

### `manager.rs` — MemoryManager

```rust
/// Agent-facing memory API. Composes one or more stores and provides
/// convenience methods for common memory operations.
#[derive(Clone)]
pub struct MemoryManager {
    /// Durable store for project/user-scoped memories.
    durable: Arc<dyn MemoryStore>,
    /// Ephemeral store for task/session-scoped memories.
    ephemeral: Option<Arc<dyn MemoryStore>>,
    /// Current session context (injected into every entry).
    session_id: String,
    /// Current project context.
    project_id: Option<String>,
}

impl MemoryManager {
    /// Create with a durable store only.
    pub fn new(durable: Arc<dyn MemoryStore>, session_id: impl Into<String>) -> Self { ... }

    /// Add an ephemeral store for task/session-scoped memory.
    pub fn with_ephemeral(mut self, store: Arc<dyn MemoryStore>) -> Self { ... }

    /// Set the project context.
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self { ... }

    /// Remember a fact (convenience wrapper around store).
    pub async fn remember(
        &self,
        key: impl Into<String>,
        value: serde_json::Value,
        scope: MemoryScope,
    ) -> Result<EntryId, MemoryError> { ... }

    /// Recall facts matching a text query.
    pub async fn recall(
        &self,
        query: &str,
        scope: Option<MemoryScope>,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>, MemoryError> { ... }

    /// Forget (delete) an entry by ID.
    pub async fn forget(&self, id: &EntryId) -> Result<(), MemoryError> { ... }

    /// List all memories for the current project.
    pub async fn list_project(&self) -> Result<Vec<MemoryEntry>, MemoryError> { ... }

    /// Run garbage collection on both stores.
    pub async fn gc(&self, policy: &GcPolicy) -> Result<GcReport, MemoryError> { ... }

    /// Search across both stores (ephemeral first, then durable).
    pub async fn search_all(&self, query: &MemoryQuery) -> Result<Vec<MemorySearchResult>, MemoryError> { ... }
}
```

### `config.rs` — MemoryConfig

```rust
/// Memory system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Enable the memory system.
    pub enabled: bool,
    /// Backend: "sqlite" or "in-memory".
    pub backend: MemoryBackend,
    /// Path for SQLite database (relative to data_dir).
    pub sqlite_path: String,
    /// Maximum entries before GC triggers.
    pub max_entries: usize,
    /// Default TTL for session-scoped entries (seconds).
    pub session_ttl_secs: u64,
    /// Default TTL for project-scoped entries (seconds, 0 = never).
    pub project_ttl_secs: u64,
    /// Minimum relevance score for search results.
    pub min_relevance: f64,
    /// Enable embedding-based search (requires embedding provider).
    pub embeddings_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryBackend {
    Sqlite,
    InMemory,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: MemoryBackend::Sqlite,
            sqlite_path: "memory.db".into(),
            max_entries: 10_000,
            session_ttl_secs: 3600,       // 1 hour
            project_ttl_secs: 0,          // never
            min_relevance: 0.1,
            embeddings_enabled: false,
        }
    }
}
```

---

## Runtime integration

### `XaftRuntime` changes

```rust
pub struct XaftRuntime {
    // ... existing fields ...
    pub(crate) memory_manager: Option<Arc<MemoryManager>>,
}

impl XaftRuntime {
    /// Inject a memory manager (called from binary layer after bootstrapping).
    pub fn with_memory_manager(mut self, manager: Arc<MemoryManager>) -> Self {
        self.memory_manager = Some(manager);
        self
    }
}
```

### Binary layer (`src/main.rs`) changes

```rust
async fn bootstrap_runtime(config: XaftConfig) -> Result<XaftRuntime, RuntimeError> {
    let runtime = XaftRuntime::bootstrap(config.clone()).await?;

    // ... existing session manager setup ...

    // Bootstrap memory if enabled
    let runtime = if config.memory.enabled {
        let store: Arc<dyn MemoryStore> = match config.memory.backend {
            MemoryBackend::Sqlite => {
                let path = config.core.data_dir.join(&config.memory.sqlite_path);
                Arc::new(SqliteStore::new(&path).await?)
            }
            MemoryBackend::InMemory => Arc::new(InMemoryStore::new()),
        };
        let manager = MemoryManager::new(store, "system")
            .with_project(config.core.data_dir.display().to_string());
        runtime.with_memory_manager(Arc::new(manager))
    } else {
        runtime
    };

    Ok(runtime)
}
```

### Agent tool integration

A new `remember` / `recall` tool pair in `xaft-tools`:

```rust
pub struct RememberTool {
    manager: Arc<MemoryManager>,
}

pub struct RecallTool {
    manager: Arc<MemoryManager>,
}
```

These are registered in `ToolRegistryBuilder` via `.with_memory(manager)` and added to the coder/QA tool sets.

---

## Config changes

### `~/.config/xaft/xaft.toml`

```toml
[memory]
enabled     = true
backend     = "sqlite"          # or "in-memory"
sqlite_path = "memory.db"       # relative to data_dir
max_entries = 10000
session_ttl_secs = 3600         # 1 hour
project_ttl_secs = 0            # never expire
min_relevance = 0.1
embeddings_enabled = false
```

### `XaftConfig` addition

```rust
pub struct XaftConfig {
    // ... existing fields ...
    #[serde(default)]
    pub memory: MemoryConfig,
}
```

---

## Files to modify / create

| File | Change |
|------|--------|
| `crates/xaft-memory/` | **New crate** — all modules above |
| `Cargo.toml` | Add `xaft-memory` to workspace members + dependencies |
| `xaft-config/src/types.rs` | Add `MemoryConfig`, `MemoryBackend` |
| `xaft-config/src/defaults.rs` | Add `MemoryConfig::default()` to `XaftConfig::default()` |
| `xaft-config/src/lib.rs` | Re-export `MemoryConfig`, `MemoryBackend` |
| `xaft-runtime/Cargo.toml` | Add `xaft-memory` dependency |
| `xaft-runtime/src/runtime.rs` | Add `memory_manager` field, `with_memory_manager()` |
| `xaft-runtime/src/lib.rs` | Re-export memory types |
| `xaft-tools/src/` | Add `RememberTool`, `RecallTool` |
| `xaft-tools/src/registry.rs` | Add `.with_memory()` to `ToolRegistryBuilder` |
| `src/main.rs` | Bootstrap memory manager from config |

---

## Testing

### Unit tests (in each module file)

```
entry_id_new_is_unique
entry_id_from_string_roundtrip
entry_builder_sets_fields
entry_is_expired_detects_ttl
entry_touch_increments_count
memory_metadata_default_values
memory_query_text_constructor
memory_query_chaining
gc_policy_default_values
memory_error_display
memory_error_from_serde_json
memory_error_from_sqlx
```

### Store tests (in_memory.rs, sqlite.rs)

```
store_and_retrieve
retrieve_nonexistent_returns_none
store_and_search_by_text
search_respects_limit
search_respects_scope_filter
search_respects_tag_filter
search_min_relevance_filter
update_existing_entry
delete_existing_entry
delete_nonexistent_is_idempotent
list_with_filter
gc_removes_expired
gc_removes_low_relevance
gc_removes_idle_entries
gc_report_counts
concurrent_store_and_retrieve
store_type_identifier
```

### Integration tests (`tests/memory_integration.rs`)

```
manager_remember_and_recall
manager_remember_scoped_to_project
manager_recall_across_scopes
manager_forget_removes_entry
manager_gc_cleans_both_stores
manager_search_all_merges_results
manager_session_vs_project_scoping
remember_tool_stores_via_agent
recall_tool_returns_results
full_workflow_with_memory
memory_persists_across_runtime_instances  (SQLite only)
```

### End-to-end tests (`tests/memory_e2e.rs`)

```
agent_remembers_fact_in_session
agent_recalls_fact_from_previous_session
memory_respects_scope_boundaries
gc_runs_on_session_end
config_drives_backend_selection
disabled_memory_returns_noop
```

---

## Acceptance Criteria

1. `xaft-memory` compiles as a standalone crate with `#![deny(missing_docs)]`.
2. `InMemoryStore` passes all store tests.
3. `SqliteStore` passes all store tests (using `:memory:` for CI).
4. `MemoryManager::remember()` + `recall()` round-trip correctly.
5. GC removes expired and low-relevance entries.
6. `XaftRuntime::with_memory_manager()` injects the manager.
7. `.xaft.toml` `[memory]` section deserializes with defaults.
8. Disabled config (`enabled = false`) skips memory bootstrap.
9. `RememberTool` and `RecallTool` integrate with `ToolRegistryBuilder`.
10. All existing tests continue to pass (no regressions).
11. Search works with substring match (text) and optional embedding similarity.
12. Concurrent access is safe (`RwLock` for in-memory, connection pool for SQLite).

---

## Future extensions (out of scope)

- Embedding provider integration (OpenAI, local model) for semantic search
- Cross-session knowledge graph
- Memory summarization agent (periodic compression of old entries)
- Memory export/import (JSON, markdown)
- MCP memory server for external tool access
- Distributed memory (Redis, Postgres) for multi-instance deployments
