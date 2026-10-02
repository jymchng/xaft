//! Client-neutral session service core.
//!
//! This is xaft's port of agenthicc's `agenthicc/session_service/` subsystem
//! (`models.py` 363, `service.py` 951, `store.py` 453), reduced to the parts
//! the user-visible CLI needs.
//!
//! # What the service is
//!
//! agenthicc's `session` family is not six thin wrappers around a local
//! session log — it is a *client-neutral service* that sits between any number
//! of clients (the TUI, the CLI, an HTTP attachment transport) and one
//! session. Three properties make it a service rather than a store:
//!
//! 1. **Capability gating.** Every call carries the caller's capability set.
//!    A caller holding only `{"read", "workspace"}` can take a snapshot but is
//!    refused a control operation, and the refusal is an error rather than a
//!    silently ignored flag.
//! 2. **A durable event log.** Each session has a monotonic
//!    `last_event_sequence`. Events survive process restarts, and a client
//!    resumes by passing the last sequence it saw as an `after` cursor.
//! 3. **A policy-filtered snapshot.** A client never sees raw state: the
//!    service projects the snapshot through the caller's capabilities first.
//!
//! # Why this lives on top of the existing store
//!
//! The service tables (`service_sessions`, `service_events`) are created in the
//! **same** `sessions.db` file that [`crate::SessionManager`] already opens.
//! This is deliberate: the goal is a second *view* over one store, not a
//! second store. Two stores would mean two sources of truth about which
//! sessions exist — the exact failure mode the store-reconciliation invariant
//! in `xaft-cli/tests/sessions.rs` exists to prevent.
//!
//! The local-log family (`xaft session list|show|inspect`) and the service
//! family (`xaft session create|events|export`) therefore share one file and
//! cannot drift apart.
//!
//! # Deliberate divergences from agenthicc
//!
//! * agenthicc keeps live runtimes in memory behind an `asyncio.Lock`; xaft is
//!   durable-first, so the snapshot *is* the record and there is no separate
//!   runtime object to keep in sync.
//! * agenthicc redacts event payloads through `_SENSITIVE` before serving them.
//!   xaft's payloads are built by the service itself and contain no provider
//!   credentials, so there is nothing to redact here. If a future caller
//!   submits provider material through `submit`, redaction must be added at
//!   that boundary rather than assumed.
//! * agenthicc runs the turn a `submit_message` queues through an in-process
//!   handler. xaft has no such handler behind the service, so `submit_message`
//!   records the queued turn durably and moves the session to `idle`; claiming
//!   `running` for work nothing is executing would be a lie in the snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};

use crate::error::SessionError;

// ── Capability vocabulary ────────────────────────────────────────────────────

/// The `read` capability: permits `snapshot`, `list_sessions` and `events`.
pub const CAP_READ: &str = "read";
/// The `control` capability: permits mutations, including `create_session`.
pub const CAP_CONTROL: &str = "control";
/// The `workspace` capability: permits seeing project-root-scoped payloads
/// (`workflow`, `agent`) inside an otherwise policy-filtered snapshot.
pub const CAP_WORKSPACE: &str = "workspace";
/// The `admin` capability: a wildcard that satisfies every requirement.
pub const CAP_ADMIN: &str = "admin";

/// A caller's capability set.
///
/// Gating lives here rather than in each command so that the rule is stated
/// once: [`CapabilitySet::require`] is the only way a service call checks
/// authorisation, and `admin` is the only way to bypass it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet(BTreeSet<String>);

impl CapabilitySet {
    /// Build from an iterator of capability names. Blank names are dropped.
    pub fn new<I, S>(caps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self(
            caps.into_iter()
                .map(|c| c.as_ref().trim().to_string())
                .filter(|c| !c.is_empty())
                .collect(),
        )
    }

    /// The three capabilities the CLI uses for a full-access caller.
    pub fn cli() -> Self {
        Self::new([CAP_READ, CAP_CONTROL, CAP_WORKSPACE])
    }

    /// A read-only caller: can see a session, cannot change it.
    pub fn read_only() -> Self {
        Self::new([CAP_READ, CAP_WORKSPACE])
    }

    /// Whether `capability` is held (directly, or via `admin`).
    pub fn holds(&self, capability: &str) -> bool {
        self.0.contains(capability) || self.0.contains(CAP_ADMIN)
    }

    /// Return an error unless `capability` is held.
    ///
    /// This is the single authorisation gate for the service. `admin` is a
    /// wildcard because that is what agenthicc does, and because a service
    /// with no privileged caller has no way to repair its own state.
    pub fn require(&self, capability: &str) -> Result<(), SessionServiceError> {
        if self.holds(capability) {
            Ok(())
        } else {
            Err(SessionServiceError::forbidden(capability))
        }
    }

    /// The held capabilities, sorted — the form snapshots serialise.
    pub fn sorted(&self) -> Vec<String> {
        self.0.iter().cloned().collect()
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// A structured service error carrying a stable machine-readable code.
///
/// Distinct from [`crate::SessionError`], which reports store/IO failures.
/// This type is what the *policy* layer raises, and it serialises to
/// `{"code", "message", "status"}` exactly as agenthicc's does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionServiceError {
    /// Stable machine-readable code, e.g. `forbidden` or `not_found`.
    pub code: String,
    /// Human-readable message; the CLI prints this verbatim.
    pub message: String,
    /// HTTP-shaped status, carried for the future `session serve` transport.
    pub status: u16,
}

impl SessionServiceError {
    /// The caller lacks `capability`.
    pub fn forbidden(capability: &str) -> Self {
        Self {
            code: "forbidden".to_string(),
            message: format!("client lacks '{capability}' capability"),
            status: 403,
        }
    }

    /// No such session.
    pub fn not_found(session_id: &str) -> Self {
        Self {
            code: "not_found".to_string(),
            message: format!("Session not found: {session_id}"),
            status: 404,
        }
    }

    /// The requested command kind is not one the service implements.
    pub fn unknown_kind(kind: &str) -> Self {
        Self {
            code: "unknown_command".to_string(),
            message: format!("unknown session command: {kind}"),
            status: 400,
        }
    }

    /// The envelope is malformed (missing id, empty text, bad payload shape).
    pub fn invalid_command(message: &str) -> Self {
        Self {
            code: "invalid_command".to_string(),
            message: message.to_string(),
            status: 400,
        }
    }

    /// The caller's `expected_sequence` is behind the session's.
    pub fn stale_sequence(expected: u64, current: u64) -> Self {
        Self {
            code: "stale_sequence".to_string(),
            message: format!("expected sequence {expected}, current is {current}"),
            status: 409,
        }
    }

    /// The session's state does not admit the requested command.
    pub fn invalid_state(message: &str) -> Self {
        Self {
            code: "invalid_state".to_string(),
            message: message.to_string(),
            status: 409,
        }
    }
}

impl std::fmt::Display for SessionServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionServiceError {}

// ── Models ───────────────────────────────────────────────────────────────────

/// Lifecycle states visible to every client adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Freshly created, no work submitted.
    Created,
    /// Between turns.
    Idle,
    /// A turn is executing.
    Running,
    /// Blocked on a tool-approval prompt.
    WaitingApproval,
    /// Blocked on an agent question.
    WaitingQuestion,
    /// Detached and running as a background pipeline.
    Background,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled by a client.
    Cancelled,
    /// Archived and no longer accepting input.
    Archived,
}

impl SessionState {
    /// The wire name (matches agenthicc's `StrEnum` values).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::WaitingQuestion => "waiting_question",
            Self::Background => "background",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Archived => "archived",
        }
    }
}

/// Whether an event is retained for replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDurability {
    /// Retained in the event log and replayable.
    Durable,
    /// Not retained; projected to attached clients but never persisted.
    Ephemeral,
}

impl EventDurability {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Ephemeral => "ephemeral",
        }
    }
}

/// A policy-filtered, client-neutral read model for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Schema version of this document (always 1).
    pub schema_version: u32,
    /// Stable session identifier.
    pub session_id: String,
    /// Absolute project root this session is bound to.
    pub project_root: String,
    /// Creation time, seconds since the Unix epoch.
    pub created_at: f64,
    /// Last mutation time, seconds since the Unix epoch.
    pub updated_at: f64,
    /// Current lifecycle state.
    pub state: SessionState,
    /// The turn currently executing, if any.
    pub active_turn_id: Option<String>,
    /// Parent session for a forked session, if any.
    pub parent_session_id: Option<String>,
    /// Workflow descriptor; only populated for workspace-capable callers.
    pub workflow: BTreeMap<String, serde_json::Value>,
    /// Agent descriptor; only populated for workspace-capable callers.
    pub agent: BTreeMap<String, serde_json::Value>,
    /// Queue descriptor; carries at least `depth` and `accepting_input`.
    pub queue: BTreeMap<String, serde_json::Value>,
    /// Pending tool approvals.
    pub approvals_pending: u32,
    /// Pending agent questions.
    pub questions_pending: u32,
    /// Running background pipelines.
    pub background_jobs_running: u32,
    /// Running background terminals.
    pub terminals_running: u32,
    /// Sequence of the most recent durable event.
    pub last_event_sequence: u64,
    /// Capabilities the session was created with, sorted.
    pub capabilities: Vec<String>,
}

impl SessionSnapshot {
    /// A fresh snapshot in the `created` state with an empty event log.
    pub fn new(
        session_id: impl Into<String>,
        project_root: impl Into<String>,
        capabilities: &CapabilitySet,
        now: f64,
    ) -> Self {
        let mut queue = BTreeMap::new();
        queue.insert("depth".to_string(), serde_json::json!(0));
        queue.insert("accepting_input".to_string(), serde_json::json!(true));
        Self {
            schema_version: 1,
            session_id: session_id.into(),
            project_root: project_root.into(),
            created_at: now,
            updated_at: now,
            state: SessionState::Created,
            active_turn_id: None,
            parent_session_id: None,
            workflow: BTreeMap::new(),
            agent: BTreeMap::new(),
            queue,
            approvals_pending: 0,
            questions_pending: 0,
            background_jobs_running: 0,
            terminals_running: 0,
            last_event_sequence: 0,
            capabilities: capabilities.sorted(),
        }
    }

    /// Serialise to a JSON value.
    ///
    /// `serde_json::Value`'s object is a `BTreeMap`, so the emitted key order is
    /// sorted — the property `sort_keys=True` gives agenthicc, without relying
    /// on struct field declaration order.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Drop the workspace-scoped payloads unless the caller may see them.
    ///
    /// This is the read side of the capability model. A `read`-only caller
    /// still learns that a session exists and what state it is in, but not
    /// which workflow or agent it was bound to.
    fn projected_for(&self, caps: &CapabilitySet) -> Self {
        let mut out = self.clone();
        if !(caps.holds(CAP_WORKSPACE) || caps.holds(CAP_ADMIN)) {
            out.workflow = BTreeMap::new();
            out.agent = BTreeMap::new();
        }
        // The echoed capability list is the session's, not the caller's, but a
        // non-workspace caller must not be able to enumerate it either.
        if !caps.holds(CAP_WORKSPACE) && !caps.holds(CAP_ADMIN) {
            out.capabilities = Vec::new();
        }
        out
    }
}

/// A versioned event envelope delivered to every attached client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Schema version of this envelope (always 1).
    pub schema_version: u32,
    /// Globally unique event identifier.
    pub event_id: String,
    /// Monotonic, gap-free within a session, starting at 1.
    pub sequence: u64,
    /// The session this event belongs to.
    pub session_id: String,
    /// The turn this event belongs to, if any.
    pub turn_id: Option<String>,
    /// Which component emitted the event.
    pub source: String,
    /// Event kind, e.g. `session_created`.
    pub kind: String,
    /// Seconds since the Unix epoch.
    pub occurred_at: f64,
    /// Whether the event is retained for replay.
    pub durability: EventDurability,
    /// Visibility scope; `session` means session-scoped.
    pub visibility: String,
    /// Event body.
    pub payload: BTreeMap<String, serde_json::Value>,
}

impl SessionEvent {
    /// Build a durable, session-scoped event with a fresh id.
    pub fn create(
        session_id: &str,
        sequence: u64,
        source: &str,
        kind: &str,
        payload: BTreeMap<String, serde_json::Value>,
        now: f64,
    ) -> Self {
        Self {
            schema_version: 1,
            event_id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
            sequence,
            session_id: session_id.to_string(),
            turn_id: None,
            source: source.to_string(),
            kind: kind.to_string(),
            occurred_at: now,
            durability: EventDurability::Durable,
            visibility: "session".to_string(),
            payload,
        }
    }

    /// Serialise to a JSON value with sorted keys.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

// ── Command envelope ───────────────────────────────────────────────────────

/// A mutation submitted by any client adapter.
///
/// This is the type agenthicc names `SessionCommand` in
/// `session_service/models.py:273`. It is **not** xaft's `xaft_cli::args::
/// SessionCommand`, which is a clap subcommand enum: this one is a value that
/// travels through [`SessionService::submit`], and it is the envelope that
/// carries the capability check — the service reads
/// [`SessionCommand::capabilities`], not the transport that built it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCommand {
    /// The operation. An open string: `submit_message`, `cancel`, `resume`,
    /// `retry`, `archive`, `fork`, `delete`, `attach`, `approve`, `reject`,
    /// `answer` and `invoke_command` are implemented, and an unknown kind is
    /// an error rather than a silently ignored no-op.
    pub kind: String,
    /// Target session. `None` is rejected — the CLI always names one.
    pub session_id: Option<String>,
    /// Which client issued the command.
    pub client_id: String,
    /// Stable id for this command, minted when the caller did not supply one.
    pub command_id: String,
    /// Deduplication key. Defaults to [`SessionCommand::command_id`], so a
    /// resubmitted command replays its original result instead of applying
    /// twice.
    pub idempotency_key: String,
    /// Optimistic-concurrency cursor. When set and stale, the command is
    /// refused with `stale_sequence` rather than applied.
    pub expected_sequence: Option<u64>,
    /// The operation's arguments.
    pub payload: BTreeMap<String, serde_json::Value>,
    /// The caller's capabilities — the envelope is what authorisation reads.
    pub capabilities: CapabilitySet,
}

impl SessionCommand {
    /// A command against `session_id`, with fresh `command_id` and
    /// `idempotency_key`, as agenthicc's `__post_init__` mints them.
    pub fn new(
        kind: impl Into<String>,
        session_id: impl Into<String>,
        client_id: impl Into<String>,
        payload: BTreeMap<String, serde_json::Value>,
        capabilities: CapabilitySet,
    ) -> Self {
        let command_id = format!("cmd_{}", uuid::Uuid::new_v4().simple());
        Self {
            kind: kind.into(),
            session_id: Some(session_id.into()),
            client_id: client_id.into(),
            idempotency_key: command_id.clone(),
            command_id,
            expected_sequence: None,
            payload,
            capabilities,
        }
    }

    /// Serialise to a JSON value with sorted keys.
    pub fn to_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("schema_version".into(), serde_json::json!(1));
        map.insert("kind".into(), serde_json::json!(self.kind));
        map.insert("session_id".into(), serde_json::json!(self.session_id));
        map.insert("client_id".into(), serde_json::json!(self.client_id));
        map.insert("command_id".into(), serde_json::json!(self.command_id));
        map.insert(
            "idempotency_key".into(),
            serde_json::json!(self.idempotency_key),
        );
        map.insert(
            "expected_sequence".into(),
            serde_json::json!(self.expected_sequence),
        );
        map.insert(
            "payload".into(),
            serde_json::to_value(&self.payload).unwrap_or(serde_json::Value::Null),
        );
        serde_json::Value::Object(map)
    }
}

impl CapabilitySet {
    /// Serialise a capability set as a JSON array of its sorted names.
    fn to_value(&self) -> serde_json::Value {
        serde_json::json!(self.sorted())
    }
}

/// The receipt returned after a command is accepted.
///
/// Ported from agenthicc's `CommandResult` (`session_service/models.py:342`),
/// including the `replayed` flag that marks a deduplicated resubmission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    /// Whether the command was applied.
    pub ok: bool,
    /// The command's id.
    pub command_id: String,
    /// The session it applied to.
    pub session_id: Option<String>,
    /// Stable machine-readable outcome code (`ok` today).
    pub code: String,
    /// Human-readable detail; empty on success.
    pub message: String,
    /// The operation's result payload.
    pub data: BTreeMap<String, serde_json::Value>,
    /// True when this is a replay of an earlier identical command.
    pub replayed: bool,
}

impl CommandResult {
    /// A successful receipt for `command`.
    pub fn ok(command: &SessionCommand, data: BTreeMap<String, serde_json::Value>) -> Self {
        Self {
            ok: true,
            command_id: command.command_id.clone(),
            session_id: command.session_id.clone(),
            code: "ok".to_string(),
            message: String::new(),
            data,
            replayed: false,
        }
    }

    /// Serialise to a JSON value with sorted keys.
    ///
    /// `serde_json::Map` is a `BTreeMap` unless the `preserve_order` feature is
    /// enabled (it is not, in this workspace), so the emitted key order is
    /// sorted without depending on field declaration order.
    pub fn to_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("code".into(), serde_json::json!(self.code));
        map.insert("command_id".into(), serde_json::json!(self.command_id));
        map.insert(
            "data".into(),
            serde_json::to_value(&self.data).unwrap_or(serde_json::Value::Null),
        );
        map.insert("message".into(), serde_json::json!(self.message));
        map.insert("ok".into(), serde_json::json!(self.ok));
        map.insert("replayed".into(), serde_json::json!(self.replayed));
        map.insert("session_id".into(), serde_json::json!(self.session_id));
        serde_json::Value::Object(map)
    }
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Durable storage for service snapshots and their event logs.
///
/// Opens the **same** `sessions.db` that [`crate::SessionManager`] uses and
/// adds two tables. Sharing the file is what keeps the local-log commands and
/// the service commands describing one set of sessions.
#[derive(Clone, Debug)]
pub struct ServiceStore {
    pool: Arc<SqlitePool>,
}

impl ServiceStore {
    /// Open (or create) the service tables in `data_dir/sessions.db`.
    pub async fn open(data_dir: &Path) -> Result<Self, SessionError> {
        tokio::fs::create_dir_all(data_dir).await?;
        let path = data_dir.join("sessions.db");
        let url = format!("sqlite:{}", path.display());
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(opts)
            .await
            .map_err(SessionError::Database)?;
        let store = Self {
            pool: Arc::new(pool),
        };
        store.migrate().await?;
        Ok(store)
    }

    /// An in-memory store (for tests — no persistence).
    pub async fn in_memory() -> Result<Self, SessionError> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .map_err(SessionError::Database)?;
        let store = Self {
            pool: Arc::new(pool),
        };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), SessionError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS service_sessions (
                session_id  TEXT PRIMARY KEY,
                project_root TEXT   NOT NULL,
                payload     TEXT    NOT NULL
            );
            CREATE TABLE IF NOT EXISTS service_events (
                session_id TEXT    NOT NULL,
                sequence   INTEGER NOT NULL,
                payload    TEXT    NOT NULL,
                PRIMARY KEY (session_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS service_commands (
                session_id      TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                payload         TEXT NOT NULL,
                PRIMARY KEY (session_id, idempotency_key)
            );
            CREATE INDEX IF NOT EXISTS idx_service_events_session
                ON service_events(session_id, sequence);
            CREATE INDEX IF NOT EXISTS idx_service_sessions_root
                ON service_sessions(project_root);
            "#,
        )
        .execute(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;

        // `deleted` arrived with the `submit` envelope, after databases had
        // already been created without it. Adding it unconditionally would
        // abort the migration on those files, so probe first.
        let columns = sqlx::query("SELECT name FROM pragma_table_info('service_sessions')")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        let has_deleted = columns
            .iter()
            .any(|r| r.get::<String, _>("name") == "deleted");
        if !has_deleted {
            sqlx::query(
                "ALTER TABLE service_sessions ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
            )
            .execute(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        }
        Ok(())
    }

    /// Whether a `delete` command has retired this session.
    ///
    /// A deleted session keeps its snapshot and its event log — that history
    /// is what `session export` still serves — but refuses every further
    /// command, exactly as agenthicc's `runtime.deleted` does.
    async fn is_deleted(&self, session_id: &str) -> Result<bool, SessionError> {
        let row = sqlx::query("SELECT deleted FROM service_sessions WHERE session_id = ?1")
            .bind(session_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        Ok(row
            .map(|r| r.get::<i64, _>("deleted") != 0)
            .unwrap_or(false))
    }

    async fn mark_deleted(&self, session_id: &str) -> Result<(), SessionError> {
        sqlx::query("UPDATE service_sessions SET deleted = 1 WHERE session_id = ?1")
            .bind(session_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        Ok(())
    }

    async fn get_command(
        &self,
        session_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<CommandResult>, SessionError> {
        let row = sqlx::query(
            "SELECT payload FROM service_commands WHERE session_id = ?1 AND idempotency_key = ?2",
        )
        .bind(session_id)
        .bind(idempotency_key)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        match row {
            Some(r) => Ok(Some(serde_json::from_str(
                r.get::<String, _>("payload").as_str(),
            )?)),
            None => Ok(None),
        }
    }

    async fn put_command(
        &self,
        session_id: &str,
        idempotency_key: &str,
        result: &CommandResult,
    ) -> Result<(), SessionError> {
        sqlx::query(
            "INSERT OR REPLACE INTO service_commands (session_id, idempotency_key, payload) \
             VALUES (?1, ?2, ?3)",
        )
        .bind(session_id)
        .bind(idempotency_key)
        .bind(serde_json::to_string(result)?)
        .execute(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        Ok(())
    }

    async fn put_snapshot(&self, snap: &SessionSnapshot) -> Result<(), SessionError> {
        let payload = serde_json::to_string(snap)?;
        sqlx::query(
            "INSERT INTO service_sessions (session_id, project_root, payload) VALUES (?1, ?2, ?3) \
             ON CONFLICT(session_id) DO UPDATE SET payload = excluded.payload",
        )
        .bind(&snap.session_id)
        .bind(&snap.project_root)
        .bind(payload)
        .execute(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        Ok(())
    }

    async fn get_snapshot(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionSnapshot>, SessionError> {
        let row = sqlx::query("SELECT payload FROM service_sessions WHERE session_id = ?1")
            .bind(session_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        match row {
            Some(r) => Ok(Some(serde_json::from_str(
                r.get::<String, _>("payload").as_str(),
            )?)),
            None => Ok(None),
        }
    }

    async fn list_snapshots(
        &self,
        project_root: Option<&str>,
    ) -> Result<Vec<SessionSnapshot>, SessionError> {
        let rows = match project_root {
            Some(root) => sqlx::query(
                "SELECT payload FROM service_sessions WHERE project_root = ?1 ORDER BY session_id",
            )
            .bind(root)
            .fetch_all(self.pool.as_ref())
            .await,
            None => {
                sqlx::query("SELECT payload FROM service_sessions ORDER BY session_id")
                    .fetch_all(self.pool.as_ref())
                    .await
            }
        }
        .map_err(SessionError::Database)?;
        rows.into_iter()
            .map(|r| {
                Ok(serde_json::from_str(
                    r.get::<String, _>("payload").as_str(),
                )?)
            })
            .collect()
    }

    /// The next sequence number for `session_id` (1 on a fresh log).
    async fn next_sequence(&self, session_id: &str) -> Result<u64, SessionError> {
        let row = sqlx::query("SELECT COALESCE(MAX(sequence), 0) AS max_seq FROM service_events WHERE session_id = ?1")
            .bind(session_id)
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(SessionError::Database)?;
        let max: i64 = row.get("max_seq");
        Ok(max as u64 + 1)
    }

    async fn append_event(&self, event: &SessionEvent) -> Result<(), SessionError> {
        let payload = serde_json::to_string(event)?;
        sqlx::query(
            "INSERT OR REPLACE INTO service_events (session_id, sequence, payload) VALUES (?1, ?2, ?3)",
        )
        .bind(&event.session_id)
        .bind(event.sequence as i64)
        .bind(payload)
        .execute(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        Ok(())
    }

    async fn events_after(
        &self,
        session_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let rows = sqlx::query(
            "SELECT payload FROM service_events WHERE session_id = ?1 AND sequence > ?2 ORDER BY sequence",
        )
        .bind(session_id)
        .bind(after_sequence as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(SessionError::Database)?;
        rows.into_iter()
            .map(|r| {
                Ok(serde_json::from_str(
                    r.get::<String, _>("payload").as_str(),
                )?)
            })
            .collect()
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

/// The client-neutral session service.
///
/// Every read is capability-filtered and every mutation appends to a durable
/// event log, so a client that reconnects can resume exactly where it left off
/// by passing the last sequence it observed.
#[derive(Clone, Debug)]
pub struct SessionService {
    store: ServiceStore,
}

impl SessionService {
    /// Build a service over `store`.
    ///
    /// There is no default capability set: every capability-gated call takes
    /// its `&CapabilitySet` explicitly, so a caller cannot accidentally be
    /// authorised by a default it never chose.
    pub fn new(store: ServiceStore) -> Self {
        Self { store }
    }

    /// Open a service over `data_dir/sessions.db`.
    pub async fn open(data_dir: &Path) -> Result<Self, SessionError> {
        Ok(Self::new(ServiceStore::open(data_dir).await?))
    }

    /// An in-memory service (for tests).
    pub async fn in_memory() -> Result<Self, SessionError> {
        Ok(Self::new(ServiceStore::in_memory().await?))
    }

    /// Create a session and return its policy-filtered snapshot.
    ///
    /// Requires `control`. The first durable event is appended here, so a
    /// freshly created session always has `last_event_sequence == 1` and its
    /// log always begins with `session_created`.
    pub async fn create_session(
        &self,
        project_root: &Path,
        client_id: &str,
        caps: &CapabilitySet,
        agent: BTreeMap<String, serde_json::Value>,
        workflow: BTreeMap<String, serde_json::Value>,
    ) -> Result<SessionSnapshot, SessionServiceError> {
        caps.require(CAP_CONTROL)?;
        let now = now_epoch();
        let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
        let root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf())
            .display()
            .to_string();

        let mut snap = SessionSnapshot::new(&session_id, &root, caps, now);
        snap.agent = agent;
        snap.workflow = workflow;

        // Persist before the event so a crash between the two leaves an
        // empty-but-valid session rather than an event for a session that does
        // not exist.
        self.store
            .put_snapshot(&snap)
            .await
            .map_err(|e| store_error(&e))?;

        let mut payload = BTreeMap::new();
        payload.insert("project_root".to_string(), serde_json::json!(root));
        payload.insert("client_id".to_string(), serde_json::json!(client_id));
        payload.insert("capabilities".to_string(), serde_json::json!(caps.sorted()));
        let event = SessionEvent::create(
            &session_id,
            1,
            "session_service",
            "session_created",
            payload,
            now,
        );
        self.store
            .append_event(&event)
            .await
            .map_err(|e| store_error(&e))?;

        snap.last_event_sequence = event.sequence;
        snap.updated_at = now;
        self.store
            .put_snapshot(&snap)
            .await
            .map_err(|e| store_error(&e))?;
        Ok(snap.projected_for(caps))
    }

    /// One session's policy-filtered snapshot. Requires `read`.
    pub async fn snapshot(
        &self,
        session_id: &str,
        caps: &CapabilitySet,
    ) -> Result<SessionSnapshot, SessionServiceError> {
        caps.require(CAP_READ)?;
        let snap = self
            .store
            .get_snapshot(session_id)
            .await
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| SessionServiceError::not_found(session_id))?;
        Ok(snap.projected_for(caps))
    }

    /// Every session's policy-filtered snapshot, optionally scoped to a root.
    /// Requires `read`.
    pub async fn list_sessions(
        &self,
        project_root: Option<&str>,
        caps: &CapabilitySet,
    ) -> Result<Vec<SessionSnapshot>, SessionServiceError> {
        caps.require(CAP_READ)?;
        let snaps = self
            .store
            .list_snapshots(project_root)
            .await
            .map_err(|e| store_error(&e))?;
        Ok(snaps.iter().map(|s| s.projected_for(caps)).collect())
    }

    /// Replay durable events with `sequence > after_sequence`. Requires `read`.
    pub async fn events(
        &self,
        session_id: &str,
        after_sequence: u64,
        caps: &CapabilitySet,
    ) -> Result<Vec<SessionEvent>, SessionServiceError> {
        caps.require(CAP_READ)?;
        // Distinguish "no such session" from "no events yet": a client
        // replaying a typo'd id must get not_found, not an empty list.
        if self
            .store
            .get_snapshot(session_id)
            .await
            .map_err(|e| store_error(&e))?
            .is_none()
        {
            return Err(SessionServiceError::not_found(session_id));
        }
        self.store
            .events_after(session_id, after_sequence)
            .await
            .map_err(|e| store_error(&e))
    }

    /// Apply one command envelope, returning the receipt.
    ///
    /// Ported from agenthicc's `SessionService.submit`
    /// (`session_service/service.py:740`), reduced to the kinds that are
    /// user-visible without a live turn runtime. Both `session send` and
    /// `session control` route through here, which is what makes the
    /// capability gate the same rule for both.
    ///
    /// The order of checks is agenthicc's and is load-bearing: a resubmitted
    /// command replays before the deleted check, an archived session accepts
    /// only `attach`/`resume`, and a stale `expected_sequence` is a conflict
    /// rather than a silent overwrite.
    pub async fn submit(
        &self,
        command: &SessionCommand,
    ) -> Result<CommandResult, SessionServiceError> {
        let session_id = command
            .session_id
            .clone()
            .ok_or_else(|| SessionServiceError::invalid_command("session_id is required"))?;

        // `attach` is the one kind a read-only client may submit; every other
        // kind mutates something and needs `control`.
        let required = if command.kind == "attach" {
            CAP_READ
        } else {
            CAP_CONTROL
        };
        command.capabilities.require(required)?;

        let mut snap = self
            .store
            .get_snapshot(&session_id)
            .await
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| SessionServiceError::not_found(&session_id))?;

        if let Some(prior) = self
            .store
            .get_command(&session_id, &command.idempotency_key)
            .await
            .map_err(|e| store_error(&e))?
        {
            return Ok(CommandResult {
                replayed: true,
                ..prior
            });
        }

        if self
            .store
            .is_deleted(&session_id)
            .await
            .map_err(|e| store_error(&e))?
        {
            return Err(SessionServiceError::not_found(&session_id));
        }

        if let Some(expected) = command.expected_sequence {
            if expected != snap.last_event_sequence {
                return Err(SessionServiceError::stale_sequence(
                    expected,
                    snap.last_event_sequence,
                ));
            }
        }

        if snap.state == SessionState::Archived
            && !matches!(command.kind.as_str(), "attach" | "resume")
        {
            return Err(SessionServiceError::invalid_state(
                "archived sessions accept only attach or resume",
            ));
        }

        let now = now_epoch();
        let mut data: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        match command.kind.as_str() {
            "attach" => {
                data.insert(
                    "snapshot".to_string(),
                    snap.projected_for(&command.capabilities).to_value(),
                );
            }
            "submit_message" => {
                let text = command
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| {
                        SessionServiceError::invalid_command(
                            "submit_message requires non-empty text",
                        )
                    })?;
                let turn_id = format!("turn_{}", uuid::Uuid::new_v4().simple());
                self.append(
                    &mut snap,
                    "turn_queued",
                    BTreeMap::from([
                        ("text".to_string(), serde_json::json!(text)),
                        (
                            "client_id".to_string(),
                            serde_json::json!(command.client_id),
                        ),
                    ]),
                    now,
                )
                .await?;
                snap.active_turn_id = Some(turn_id.clone());
                snap.state = SessionState::Idle;
                bump_queue_depth(&mut snap, 1);
                data.insert("accepted".to_string(), serde_json::json!(true));
                data.insert("turn_id".to_string(), serde_json::json!(turn_id));
            }
            "cancel" | "interrupt" => {
                self.append(
                    &mut snap,
                    "cancel_requested",
                    BTreeMap::from([(
                        "client_id".to_string(),
                        serde_json::json!(command.client_id),
                    )]),
                    now,
                )
                .await?;
                snap.state = SessionState::Cancelled;
                set_accepting_input(&mut snap, false);
                data.insert("cancel_requested".to_string(), serde_json::json!(true));
            }
            "approve" | "reject" => {
                self.append(&mut snap, "approval_resolved", command.payload.clone(), now)
                    .await?;
                snap.approvals_pending = snap.approvals_pending.saturating_sub(1);
                data.insert("resolved".to_string(), serde_json::json!(true));
            }
            "answer" => {
                self.append(&mut snap, "answer_resolved", command.payload.clone(), now)
                    .await?;
                snap.questions_pending = snap.questions_pending.saturating_sub(1);
                data.insert("resolved".to_string(), serde_json::json!(true));
            }
            "resume" | "retry" => {
                let event_kind = if command.kind == "resume" {
                    "session_resumed"
                } else {
                    "retry_scheduled"
                };
                self.append(&mut snap, event_kind, command.payload.clone(), now)
                    .await?;
                snap.state = SessionState::Idle;
                snap.active_turn_id = None;
                set_accepting_input(&mut snap, true);
                data.insert("state".to_string(), serde_json::json!(snap.state.as_str()));
            }
            "archive" => {
                self.append(&mut snap, "session_archived", BTreeMap::new(), now)
                    .await?;
                snap.state = SessionState::Archived;
                set_accepting_input(&mut snap, false);
                data.insert("state".to_string(), serde_json::json!("archived"));
            }
            "invoke_command" => {
                self.append(&mut snap, "command_invoked", command.payload.clone(), now)
                    .await?;
                data.insert("accepted".to_string(), serde_json::json!(true));
            }
            "fork" => {
                let child = self.fork(&snap, &command.capabilities, now).await?;
                data.insert("session_id".to_string(), serde_json::json!(child));
            }
            "delete" => {
                self.append(
                    &mut snap,
                    "session_deleted",
                    BTreeMap::from([(
                        "client_id".to_string(),
                        serde_json::json!(command.client_id),
                    )]),
                    now,
                )
                .await?;
                self.store
                    .mark_deleted(&session_id)
                    .await
                    .map_err(|e| store_error(&e))?;
                data.insert("deleted".to_string(), serde_json::json!(true));
            }
            other => return Err(SessionServiceError::unknown_kind(other)),
        }

        let result = CommandResult::ok(command, data);

        // Every accepted command leaves a receipt event on the log, so a
        // client that missed the response can still learn what happened.
        self.append(
            &mut snap,
            "command_accepted",
            BTreeMap::from([
                (
                    "idempotency_key".to_string(),
                    serde_json::json!(command.idempotency_key),
                ),
                ("result".to_string(), result.to_value()),
            ]),
            now,
        )
        .await?;

        self.store
            .put_command(&session_id, &command.idempotency_key, &result)
            .await
            .map_err(|e| store_error(&e))?;

        Ok(result)
    }

    /// Append one durable event and advance the snapshot's cursor.
    ///
    /// The sequence is read from the log rather than tracked in memory, so two
    /// processes writing the same session cannot mint the same sequence.
    async fn append(
        &self,
        snap: &mut SessionSnapshot,
        kind: &str,
        payload: BTreeMap<String, serde_json::Value>,
        now: f64,
    ) -> Result<SessionEvent, SessionServiceError> {
        let sequence = self
            .store
            .next_sequence(&snap.session_id)
            .await
            .map_err(|e| store_error(&e))?;
        let event = SessionEvent::create(
            &snap.session_id,
            sequence,
            "session_service",
            kind,
            payload,
            now,
        );
        self.store
            .append_event(&event)
            .await
            .map_err(|e| store_error(&e))?;
        snap.last_event_sequence = sequence;
        snap.updated_at = now;
        self.store
            .put_snapshot(snap)
            .await
            .map_err(|e| store_error(&e))?;
        Ok(event)
    }

    /// Create a child session sharing this one's project root.
    ///
    /// Ported from agenthicc's `_fork_locked`: the child gets its own creation
    /// event carrying `forked_from_sequence`, which is how a reader tells a
    /// fork from an ordinary create.
    async fn fork(
        &self,
        parent: &SessionSnapshot,
        caps: &CapabilitySet,
        now: f64,
    ) -> Result<String, SessionServiceError> {
        let child_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
        let mut child = SessionSnapshot::new(&child_id, &parent.project_root, caps, now);
        child.parent_session_id = Some(parent.session_id.clone());
        self.store
            .put_snapshot(&child)
            .await
            .map_err(|e| store_error(&e))?;
        let event = SessionEvent::create(
            &child_id,
            1,
            "session_service",
            "session_created",
            BTreeMap::from([
                (
                    "project_root".to_string(),
                    serde_json::json!(parent.project_root),
                ),
                (
                    "parent_session_id".to_string(),
                    serde_json::json!(parent.session_id),
                ),
                (
                    "forked_from_sequence".to_string(),
                    serde_json::json!(parent.last_event_sequence),
                ),
                ("capabilities".to_string(), caps.to_value()),
            ]),
            now,
        );
        self.store
            .append_event(&event)
            .await
            .map_err(|e| store_error(&e))?;
        child.last_event_sequence = 1;
        self.store
            .put_snapshot(&child)
            .await
            .map_err(|e| store_error(&e))?;
        Ok(child_id)
    }
}

/// Record that the session still (or no longer) accepts input.
fn set_accepting_input(snap: &mut SessionSnapshot, accepting: bool) {
    snap.queue
        .insert("accepting_input".to_string(), serde_json::json!(accepting));
}

/// Move the queue depth by `delta`, never below zero.
fn bump_queue_depth(snap: &mut SessionSnapshot, delta: i64) {
    let current = snap
        .queue
        .get("depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    snap.queue.insert(
        "depth".to_string(),
        serde_json::json!((current + delta).max(0)),
    );
}

fn store_error(e: &SessionError) -> SessionServiceError {
    SessionServiceError {
        code: "store_error".to_string(),
        message: e.to_string(),
        status: 500,
    }
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_map() -> BTreeMap<String, serde_json::Value> {
        BTreeMap::new()
    }

    /// Submit `kind` to `session_id` as a full-capability CLI client.
    async fn submit(
        svc: &SessionService,
        session_id: &str,
        kind: &str,
    ) -> Result<CommandResult, SessionServiceError> {
        svc.submit(&SessionCommand::new(
            kind,
            session_id,
            "cli",
            empty_map(),
            CapabilitySet::cli(),
        ))
        .await
    }

    /// Submit `kind` to `session_id` as a read-only client.
    async fn submit_read_only(
        svc: &SessionService,
        session_id: &str,
        kind: &str,
    ) -> Result<CommandResult, SessionServiceError> {
        svc.submit(&SessionCommand::new(
            kind,
            session_id,
            "cli",
            empty_map(),
            CapabilitySet::read_only(),
        ))
        .await
    }

    #[tokio::test]
    async fn create_requires_control() {
        let svc = SessionService::in_memory().await.unwrap();
        let err = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::read_only(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "forbidden");
        assert_eq!(err.status, 403);
    }

    #[tokio::test]
    async fn create_produces_sequence_one_creation_event() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        assert_eq!(snap.last_event_sequence, 1);
        assert_eq!(snap.state, SessionState::Created);
        assert!(snap.session_id.starts_with("sess_"));

        let events = svc
            .events(&snap.session_id, 0, &CapabilitySet::read_only())
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[0].kind, "session_created");
    }

    #[tokio::test]
    async fn events_after_cursor_returns_only_newer() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        submit(&svc, &snap.session_id, "cancel").await.unwrap();

        // `cancel` writes two events: the operation itself and the
        // `command_accepted` receipt.
        let caps = CapabilitySet::read_only();
        let all = svc.events(&snap.session_id, 0, &caps).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[1].kind, "cancel_requested");
        assert_eq!(all[2].kind, "command_accepted");
        let after_one = svc.events(&snap.session_id, 1, &caps).await.unwrap();
        assert_eq!(after_one.len(), 2);
        assert_eq!(after_one[0].sequence, 2);
        assert!(
            svc.events(&snap.session_id, 3, &caps)
                .await
                .unwrap()
                .is_empty(),
            "a cursor at the head replays nothing"
        );
    }

    #[tokio::test]
    async fn a_read_only_caller_is_denied_control() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let err = submit_read_only(&svc, &snap.session_id, "cancel")
            .await
            .unwrap_err();
        assert_eq!(err.code, "forbidden");
        // The denial must not have mutated the session.
        let after = svc
            .snapshot(&snap.session_id, &CapabilitySet::read_only())
            .await
            .unwrap();
        assert_eq!(after.state, SessionState::Created);
    }

    #[tokio::test]
    async fn a_read_only_caller_cannot_see_workflow_or_agent_payloads() {
        let svc = SessionService::in_memory().await.unwrap();
        let mut agent = BTreeMap::new();
        agent.insert("name".to_string(), serde_json::json!("coder"));
        let mut workflow = BTreeMap::new();
        workflow.insert("name".to_string(), serde_json::json!("standard"));
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                agent,
                workflow,
            )
            .await
            .unwrap();
        assert!(!snap.agent.is_empty(), "cli caps include workspace");

        // A caller with `read` but no `workspace` sees neither payload.
        let read_only = CapabilitySet::new([CAP_READ]);
        let filtered = svc.snapshot(&snap.session_id, &read_only).await.unwrap();
        assert!(filtered.agent.is_empty());
        assert!(filtered.workflow.is_empty());
        assert!(filtered.capabilities.is_empty());
        // …but still learns the session exists and what state it is in.
        assert_eq!(filtered.session_id, snap.session_id);
        assert_eq!(filtered.state, SessionState::Created);
    }

    #[tokio::test]
    async fn snapshot_of_a_missing_session_is_not_found() {
        let svc = SessionService::in_memory().await.unwrap();
        let err = svc
            .snapshot("sess_nope", &CapabilitySet::read_only())
            .await
            .unwrap_err();
        assert_eq!(err.code, "not_found");
        assert_eq!(err.status, 404);
        assert!(err.message.contains("sess_nope"));
    }

    #[tokio::test]
    async fn events_of_a_missing_session_is_not_found_not_empty() {
        let svc = SessionService::in_memory().await.unwrap();
        let err = svc
            .events("sess_nope", 0, &CapabilitySet::read_only())
            .await
            .unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[tokio::test]
    async fn control_rejects_an_unknown_kind_and_leaves_state_alone() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let err = submit(&svc, &snap.session_id, "not-a-real-kind")
            .await
            .unwrap_err();
        assert_eq!(err.code, "unknown_command");
        assert_eq!(err.message, "unknown session command: not-a-real-kind");
        let after = svc
            .snapshot(&snap.session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(after.state, SessionState::Created);
        assert_eq!(
            after.last_event_sequence, 1,
            "a rejected command must not append an event"
        );
    }

    #[tokio::test]
    async fn control_changes_state_and_bumps_the_sequence() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let result = submit(&svc, &snap.session_id, "cancel").await.unwrap();
        assert!(result.ok);
        assert_eq!(result.data["cancel_requested"], serde_json::json!(true));
        let cancelled = svc
            .snapshot(&snap.session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(cancelled.state, SessionState::Cancelled);
        let events = svc
            .events(&snap.session_id, 0, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(events[1].kind, "cancel_requested");
    }

    #[tokio::test]
    async fn admin_satisfies_every_capability() {
        let admin = CapabilitySet::new([CAP_ADMIN]);
        assert!(admin.require(CAP_CONTROL).is_ok());
        assert!(admin.require(CAP_READ).is_ok());
        assert!(admin.require(CAP_WORKSPACE).is_ok());
        let plain = CapabilitySet::new([CAP_READ]);
        assert!(plain.require(CAP_CONTROL).is_err());
    }

    /// Assert that every JSON object in `v` has its keys in sorted order.
    ///
    /// agenthicc emits `sort_keys=True`; `serde_json::Value`'s object is a
    /// `BTreeMap`, so sorting is a property of the value rather than of struct
    /// field order. This is checked at every nesting level, not just the top.
    fn assert_keys_sorted(v: &serde_json::Value, path: &str) {
        match v {
            serde_json::Value::Object(map) => {
                let keys: Vec<&String> = map.keys().collect();
                let mut sorted = keys.clone();
                sorted.sort();
                assert_eq!(keys, sorted, "unsorted object keys at {path}: {keys:?}");
                for (k, child) in map {
                    assert_keys_sorted(child, &format!("{path}.{k}"));
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    assert_keys_sorted(child, &format!("{path}[{i}]"));
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn snapshot_and_event_json_keys_are_sorted() {
        let svc = SessionService::in_memory().await.unwrap();
        let mut agent = BTreeMap::new();
        agent.insert("name".to_string(), serde_json::json!("coder"));
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                agent,
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let caps = CapabilitySet::cli();
        assert_keys_sorted(&snap.to_value(), "snapshot");
        for event in svc.events(&snap.session_id, 0, &caps).await.unwrap() {
            assert_keys_sorted(&event.to_value(), "event");
        }
    }

    #[tokio::test]
    async fn the_event_log_survives_a_service_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (session_id, last_seq) = {
            let svc = SessionService::open(dir.path()).await.unwrap();
            let snap = svc
                .create_session(
                    Path::new("."),
                    "cli",
                    &CapabilitySet::cli(),
                    empty_map(),
                    empty_map(),
                )
                .await
                .unwrap();
            submit(&svc, &snap.session_id, "archive").await.unwrap();
            (snap.session_id.clone(), 3)
        };
        // A fresh process must see the same session and the same log.
        let svc = SessionService::open(dir.path()).await.unwrap();
        let snap = svc
            .snapshot(&session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(snap.state, SessionState::Archived);
        assert_eq!(snap.last_event_sequence, last_seq);
        let events = svc
            .events(&session_id, 0, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(events.len(), 3, "create + archive + command receipt");
    }

    #[tokio::test]
    async fn submit_message_queues_a_turn_and_acknowledges_it() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();

        let result = svc
            .submit(&SessionCommand::new(
                "submit_message",
                &snap.session_id,
                "cli",
                BTreeMap::from([("text".to_string(), serde_json::json!("do the thing"))]),
                CapabilitySet::cli(),
            ))
            .await
            .unwrap();

        assert!(result.ok);
        assert_eq!(result.data["accepted"], serde_json::json!(true));
        let turn_id = result.data["turn_id"].as_str().unwrap();
        assert!(turn_id.starts_with("turn_"));

        // The receipt names the turn; the durable `turn_queued` event is the
        // record of the message itself, and is what a reconnecting client
        // replays.
        let caps = CapabilitySet::cli();
        let after = svc.snapshot(&snap.session_id, &caps).await.unwrap();
        assert_eq!(after.active_turn_id.as_deref(), Some(turn_id));
        assert_eq!(after.queue["depth"], serde_json::json!(1));

        let events = svc.events(&snap.session_id, 0, &caps).await.unwrap();
        let queued = events.iter().find(|e| e.kind == "turn_queued").unwrap();
        assert_eq!(queued.payload["text"], serde_json::json!("do the thing"));
        assert_eq!(queued.payload["client_id"], serde_json::json!("cli"));
    }

    #[tokio::test]
    async fn submit_message_without_the_control_capability_is_denied() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();

        let err = svc
            .submit(&SessionCommand::new(
                "submit_message",
                &snap.session_id,
                "cli",
                BTreeMap::from([("text".to_string(), serde_json::json!("hello"))]),
                CapabilitySet::read_only(),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.code, "forbidden");
        assert_eq!(err.status, 403);

        // The denial must be inert, not merely reported.
        let caps = CapabilitySet::cli();
        let after = svc.snapshot(&snap.session_id, &caps).await.unwrap();
        assert_eq!(after.queue["depth"], serde_json::json!(0));
        assert!(after.active_turn_id.is_none());
        assert!(
            !svc.events(&snap.session_id, 0, &caps)
                .await
                .unwrap()
                .iter()
                .any(|e| e.kind == "turn_queued")
        );
    }

    #[tokio::test]
    async fn submit_message_requires_non_empty_text() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let err = svc
            .submit(&SessionCommand::new(
                "submit_message",
                &snap.session_id,
                "cli",
                BTreeMap::from([("text".to_string(), serde_json::json!("   "))]),
                CapabilitySet::cli(),
            ))
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_command");
        assert_eq!(err.message, "submit_message requires non-empty text");
    }

    #[tokio::test]
    async fn cancel_is_visible_in_the_next_snapshot() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let result = submit(&svc, &snap.session_id, "cancel").await.unwrap();
        assert_eq!(result.data["cancel_requested"], serde_json::json!(true));
        let after = svc
            .snapshot(&snap.session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(after.state, SessionState::Cancelled);
        assert_eq!(after.queue["accepting_input"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn archive_and_fork_both_work() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();

        let forked = submit(&svc, &snap.session_id, "fork").await.unwrap();
        let child_id = forked.data["session_id"].as_str().unwrap();
        assert_ne!(child_id, snap.session_id);
        let child = svc.snapshot(child_id, &CapabilitySet::cli()).await.unwrap();
        assert_eq!(
            child.parent_session_id.as_deref(),
            Some(snap.session_id.as_str())
        );
        assert_eq!(child.project_root, snap.project_root);
        let child_events = svc
            .events(child_id, 0, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(
            child_events[0].payload["forked_from_sequence"],
            serde_json::json!(1),
            "a fork records where it branched from"
        );

        let archived = submit(&svc, &snap.session_id, "archive").await.unwrap();
        assert_eq!(archived.data["state"], serde_json::json!("archived"));
        let after = svc
            .snapshot(&snap.session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(after.state, SessionState::Archived);
    }

    #[tokio::test]
    async fn an_archived_session_accepts_only_attach_or_resume() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        submit(&svc, &snap.session_id, "archive").await.unwrap();

        let err = submit(&svc, &snap.session_id, "cancel").await.unwrap_err();
        assert_eq!(err.code, "invalid_state");
        assert_eq!(
            err.message,
            "archived sessions accept only attach or resume"
        );

        assert!(submit(&svc, &snap.session_id, "attach").await.is_ok());
        assert!(submit(&svc, &snap.session_id, "resume").await.is_ok());
    }

    #[tokio::test]
    async fn attach_is_the_one_kind_a_read_only_client_may_submit() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let result = submit_read_only(&svc, &snap.session_id, "attach")
            .await
            .unwrap();
        assert_eq!(
            result.data["snapshot"]["session_id"],
            serde_json::json!(snap.session_id)
        );
        // `attach` appends nothing but its receipt — it is a read.
        let events = svc
            .events(&snap.session_id, 0, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(events.len(), 2, "creation + command_accepted");
        assert_eq!(events[1].kind, "command_accepted");
    }

    #[tokio::test]
    async fn resubmitting_the_same_idempotency_key_replays_the_result() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let command = SessionCommand::new(
            "submit_message",
            &snap.session_id,
            "cli",
            BTreeMap::from([("text".to_string(), serde_json::json!("once"))]),
            CapabilitySet::cli(),
        );
        let first = svc.submit(&command).await.unwrap();
        assert!(!first.replayed);
        let second = svc.submit(&command).await.unwrap();
        assert!(second.replayed);
        assert_eq!(second.command_id, first.command_id);
        // Replay must not apply the command twice.
        let caps = CapabilitySet::cli();
        let events = svc.events(&snap.session_id, 0, &caps).await.unwrap();
        assert_eq!(events.iter().filter(|e| e.kind == "turn_queued").count(), 1);
    }

    #[tokio::test]
    async fn a_stale_expected_sequence_is_a_conflict() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let mut command = SessionCommand::new(
            "cancel",
            &snap.session_id,
            "cli",
            empty_map(),
            CapabilitySet::cli(),
        );
        command.expected_sequence = Some(99);
        let err = svc.submit(&command).await.unwrap_err();
        assert_eq!(err.code, "stale_sequence");
        assert_eq!(err.status, 409);
        let after = svc
            .snapshot(&snap.session_id, &CapabilitySet::cli())
            .await
            .unwrap();
        assert_eq!(after.state, SessionState::Created);
    }

    #[tokio::test]
    async fn delete_retires_the_session_but_keeps_its_history() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let result = submit(&svc, &snap.session_id, "delete").await.unwrap();
        assert_eq!(result.data["deleted"], serde_json::json!(true));

        // Further commands are refused...
        let err = submit(&svc, &snap.session_id, "resume").await.unwrap_err();
        assert_eq!(err.code, "not_found");
        // ...but the history a client may already be holding is still there.
        let caps = CapabilitySet::cli();
        let events = svc.events(&snap.session_id, 0, &caps).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "session_deleted"));
    }

    #[tokio::test]
    async fn a_command_without_a_session_id_is_invalid() {
        let svc = SessionService::in_memory().await.unwrap();
        let mut command =
            SessionCommand::new("cancel", "", "cli", empty_map(), CapabilitySet::cli());
        command.session_id = None;
        let err = svc.submit(&command).await.unwrap_err();
        assert_eq!(err.code, "invalid_command");
        assert_eq!(err.message, "session_id is required");
    }

    #[tokio::test]
    async fn command_result_json_keys_are_sorted() {
        let svc = SessionService::in_memory().await.unwrap();
        let snap = svc
            .create_session(
                Path::new("."),
                "cli",
                &CapabilitySet::cli(),
                empty_map(),
                empty_map(),
            )
            .await
            .unwrap();
        let result = submit(&svc, &snap.session_id, "cancel").await.unwrap();
        assert_keys_sorted(&result.to_value(), "result");

        let command = SessionCommand::new(
            "cancel",
            &snap.session_id,
            "cli",
            BTreeMap::from([("zzz".to_string(), serde_json::json!("x"))]),
            CapabilitySet::cli(),
        );
        assert_keys_sorted(&command.to_value(), "command");
    }

    #[tokio::test]
    async fn list_is_scoped_by_project_root() {
        let svc = SessionService::in_memory().await.unwrap();
        svc.create_session(
            Path::new("/tmp/alpha"),
            "cli",
            &CapabilitySet::cli(),
            empty_map(),
            empty_map(),
        )
        .await
        .unwrap();
        svc.create_session(
            Path::new("/tmp/beta"),
            "cli",
            &CapabilitySet::cli(),
            empty_map(),
            empty_map(),
        )
        .await
        .unwrap();
        let caps = CapabilitySet::read_only();
        assert_eq!(svc.list_sessions(None, &caps).await.unwrap().len(), 2);
        assert_eq!(
            svc.list_sessions(Some("/tmp/alpha"), &caps)
                .await
                .unwrap()
                .len(),
            1,
            "roots are canonicalised, so an exact-path filter must match one"
        );
    }
}
