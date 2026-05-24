# Persistence Layer

## Storage Architecture

```
.xaft/sessions/{session_id}.db    ← SQLite per session
.xaft/index/symbols.db            ← symbol index
.xaft/index/content.db            ← content index
.xaft/audit/{date}.jsonl          ← append-only audit log
~/.config/xaft/memory.db          ← user-level long-term memory
```

## SQLite Session Store

```rust
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn save_session(&self, session: &SessionSnapshot) -> Result<(), XaftError>;
    async fn load_session(&self, session_id: Uuid) -> Result<Option<SessionSnapshot>, XaftError>;
    async fn save_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), XaftError>;
    async fn load_checkpoint(&self, task_id: Uuid) -> Result<Option<Checkpoint>, XaftError>;
    async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, XaftError>;
}
```

## Store Backends (from agtrs-store)

| Backend | Use case | Implementation |
|---|---|---|
| `MemoryStore` | Testing, ephemeral | HashMap in Arc<RwLock<>> |
| `FsStore` | Simple local persistence | JSON files per key |
| `SqliteStore` | Production session data | SQLite via sqlx |

## Session Snapshot

```rust
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: Uuid,
    pub session_state: SessionState,
    pub intent: Intent,
    pub plan: Option<Plan>,
    pub current_task_id: Option<Uuid>,
    pub worktree_branch: Option<String>,
    pub total_cost_usd: f64,
    pub total_tokens: usize,
    pub started_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
}
```

## References

- agtrs: `agtrs-store/src/`
- agtrs tests: `agtrs-store/tests/store_integration.rs`
