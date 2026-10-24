//! The session-scoped MCP server manager.
//!
//! Port of agenthicc's `McpSessionManager` (`tools/mcp_manager.py`): one
//! manager owns every configured server for one session — the TUI's, or a
//! headless run's — and is shared by every agent turn, every phased-workflow
//! phase and the `/mcp` command. `xaft mcp connect|refresh|doctor` build a
//! short-lived one, act, and shut it down (agenthicc's `_live_cli_manager`).
//!
//! What it owns, per server:
//!
//! * **The typed status record.** [`ServerStatus::to_json`] has exactly
//!   agenthicc's ten keys (`name`, `status`, `transport`, `tool_count`,
//!   `catalog_revision`, `last_error`, `started_at`, `last_success_at`,
//!   `auth_state`, `generation_id`) and its twelve state names
//!   (`configured`, `disabled`, `starting`, `ready`, `refreshing`, `degraded`,
//!   `failed`, `authenticating`, `needs_auth`, `cancelled`, `stopping`,
//!   `stopped`).
//! * **The connection,** opened with agenthicc's reconnect policy: up to
//!   `reconnect_attempts` retries after the first attempt, the delay doubling
//!   from `reconnect_delay_seconds`, all inside the 10 s startup deadline; a
//!   connection that drops later (the stdio process exits, the stream ends)
//!   is relaunched on the next call, at most `reconnect_attempts` times over
//!   its life — the budget `lauren_mcp`'s clients spend on a lost process.
//! * **An immutable catalog snapshot** with agenthicc's canonical SHA-256 and
//!   revision rule (an unchanged catalog keeps its revision), refreshed on
//!   demand and whenever the server sends `notifications/tools/list_changed`.
//! * **Its tools,** bound to provider-safe names `mcp__<server>__<tool>` —
//!   [`McpSessionManager::tools`] hands them to a run as agtrs tools. A call
//!   made through a binding whose catalog has since been replaced is refused
//!   as stale (agenthicc's `McpStaleCatalogError`), never sent to a tool that
//!   may no longer mean the same thing.
//!
//! # The network guard
//!
//! A remote server's URL is checked against `[security]` before anything is
//! opened ([`NetworkGuard`]). agenthicc builds its guard only when
//! `network_allow_list` is non-empty, so an empty list allows every host;
//! xaft's `SecurityConfig` documents the opposite — with `sandbox_mode` on, an
//! empty list means *no* outbound access — and the guard follows xaft's
//! contract, so one key cannot mean two things. `sandbox_mode = false` lifts
//! the check. A refused host fails that one server's start with agenthicc's
//! message; it is never retried, because it cannot succeed.
//!
//! # Deliberate divergences
//!
//! * **One bad stanza does not take down the others.** agenthicc's CLI
//!   registers every server with `allow_invalid=False`, so a single malformed
//!   entry makes `connect`/`refresh`/`doctor` fail for *every* server. Here an
//!   invalid entry is registered as `failed` with its validation message (the
//!   `allow_invalid=True` path agenthicc uses for sessions) and the rest work.
//! * **A startup deadline names itself.** agenthicc's `asyncio.TimeoutError`
//!   stringifies to the empty string, so a hung server's `last_error` is `""`.
//!   Here it is `MCP server '<name>' did not finish starting within 10s`.
//! * **A server whose relaunch budget is spent is `failed`.** agenthicc's
//!   bridge keeps reporting `ready` after its client gave up relaunching a
//!   dead process, while every call fails; here the status says so and the
//!   server's tools leave the catalog until `/mcp connect` or `/mcp reload`.
//! * **Starts are per server.** agenthicc's generation counter is global, so
//!   `/mcp connect a` while startup is still running orphans every other
//!   server's in-flight start; here each server's lifecycle is serialised by
//!   its own lock, and `generation_id` is only reported.
//! * **No `required` servers.** xaft's `[[mcp.client]]` has no `required`,
//!   `startup_timeout_s`, `tool_timeout_s` or tool filters, so every server
//!   is optional (a failure never blocks a turn) and agenthicc's defaults
//!   apply: 10 s to start, 60 s per call, every tool visible.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agtrs_runtime::tool::ErasedTool;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use xaft_config::{ToolPolicy, XaftConfig};

use crate::client::{ServerHello, call_tool, initialize, list_optional, list_tools};
use crate::entry::McpEntry;
use crate::pyjson;
use crate::spec::{STARTUP_TIMEOUT, ServerSpec, TOOL_TIMEOUT};
use crate::tool::{McpTool, McpToolBinding, extract_text_content, extract_tool_content};
use crate::transport::{McpError, Transport};

/// agenthicc's `_MAX_TOOL_COUNT`.
const MAX_TOOL_COUNT: usize = 10_000;
/// agenthicc's `_MAX_SCHEMA_BYTES`.
const MAX_SCHEMA_BYTES: usize = 1_000_000;
/// agenthicc's `_MAX_INSTRUCTION_CHARS`.
const MAX_INSTRUCTION_CHARS: usize = 32_000;
/// How much stderr is appended to a failure (agenthicc: the last 4096 chars).
const STDERR_IN_ERROR: usize = 4096;
/// agenthicc's `refresh_debounce_s` default.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(50);

/// Observable lifecycle state of one server (agenthicc's `McpServerState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Registered, never started.
    Configured,
    /// `enabled = false`.
    Disabled,
    /// A start is in progress.
    Starting,
    /// Connected with a published catalog.
    Ready,
    /// A refresh is in progress.
    Refreshing,
    /// A refresh failed; the previous catalog stands.
    Degraded,
    /// The last start failed.
    Failed,
    /// Authentication is in progress.
    Authenticating,
    /// A credential the server needs is missing or was rejected.
    NeedsAuth,
    /// The start was cancelled.
    Cancelled,
    /// Shutting down.
    Stopping,
    /// Shut down.
    Stopped,
}

impl ServerState {
    /// The wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Disabled => "disabled",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Refreshing => "refreshing",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Authenticating => "authenticating",
            Self::NeedsAuth => "needs_auth",
            Self::Cancelled => "cancelled",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

/// Redacted, serialisable status of one server.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerStatus {
    /// Server name.
    pub name: String,
    /// Lifecycle state.
    pub state: ServerState,
    /// Normalised transport.
    pub transport: String,
    /// Tools in the published catalog.
    pub tool_count: usize,
    /// Revision of the published catalog.
    pub catalog_revision: u64,
    /// Last failure, redacted.
    pub last_error: Option<String>,
    /// When the last start began (epoch seconds).
    pub started_at: Option<f64>,
    /// When the server last became ready (epoch seconds).
    pub last_success_at: Option<f64>,
    /// `not_required`, `authenticated` or `needs_auth`.
    pub auth_state: String,
    /// Which lifecycle generation last touched this status.
    pub generation_id: u64,
}

impl ServerStatus {
    /// agenthicc's `McpServerStatus.to_dict()`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "status": self.state.as_str(),
            "transport": self.transport,
            "tool_count": self.tool_count,
            "catalog_revision": self.catalog_revision,
            "last_error": self.last_error,
            "started_at": self.started_at,
            "last_success_at": self.last_success_at,
            "auth_state": self.auth_state,
            "generation_id": self.generation_id,
        })
    }
}

/// One tool in a catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchema {
    /// Tool name, unique within its server.
    pub name: String,
    /// Description, or empty.
    pub description: String,
    /// JSON Schema of the arguments (an object).
    pub input_schema: Value,
}

/// An immutable catalog snapshot (agenthicc's `McpCatalogSnapshot`).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSnapshot {
    /// Server name.
    pub server_name: String,
    /// Revision; unchanged when a refresh finds identical content.
    pub revision: u64,
    /// Negotiated protocol version.
    pub protocol_version: String,
    /// `serverInfo`.
    pub server_info: Map<String, Value>,
    /// Negotiated capabilities.
    pub capabilities: Map<String, Value>,
    /// Server instructions (untrusted metadata), at most 32 000 chars.
    pub instructions: String,
    /// Tools, sorted by name.
    pub tools: Vec<ToolSchema>,
    /// Prompts as the server listed them.
    pub prompts: Vec<Value>,
    /// Resources as the server listed them.
    pub resources: Vec<Value>,
    /// Hash of the tool filter (always the empty filter in xaft).
    pub tool_filter_hash: String,
    /// SHA-256 of the canonical catalog.
    pub catalog_hash: String,
    /// When the snapshot was taken (epoch seconds).
    pub captured_at: f64,
}

impl CatalogSnapshot {
    /// agenthicc's `stable_dict()` / default `to_dict()`: everything except
    /// the capture time, so equal catalogs render byte-identically.
    pub fn to_json(&self) -> Value {
        json!({
            "server_name": self.server_name,
            "revision": self.revision,
            "protocol_version": self.protocol_version,
            "server_info": self.server_info,
            "capabilities": self.capabilities,
            "instructions": self.instructions,
            "tools": self.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<_>>(),
            "prompts": self.prompts,
            "resources": self.resources,
            "tool_count": self.tools.len(),
            "tool_filter_hash": self.tool_filter_hash,
            "catalog_hash": self.catalog_hash,
        })
    }
}

/// Decides whether a remote MCP URL may be dialled.
#[derive(Debug, Clone)]
pub struct NetworkGuard {
    policy: ToolPolicy,
}

impl NetworkGuard {
    /// The guard `[security]` describes.
    pub fn from_config(config: &XaftConfig) -> Self {
        Self {
            policy: ToolPolicy::from_config(config),
        }
    }

    /// `Ok` when `url`'s host may be reached; agenthicc's message otherwise.
    pub fn check(&self, url: &str) -> Result<(), String> {
        let host = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()))
            .filter(|h| !h.is_empty())
            .ok_or_else(|| format!("Cannot determine host for URL '{url}'"))?;
        if self.policy.is_host_allowed(&host) {
            Ok(())
        } else {
            Err(format!(
                "Outbound request to '{host}' is not on the network allow-list."
            ))
        }
    }
}

/// A server that could not start, as agenthicc's `McpServerFailed` event
/// reports it. The TUI turns it into agenthicc's transcript line
/// ([`McpServerFailure::transcript_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerFailure {
    /// The server.
    pub server: String,
    /// The redacted error.
    pub error: String,
    /// The lifecycle generation that failed.
    pub generation_id: u64,
}

impl McpServerFailure {
    /// agenthicc's `mcp_server_failure` conversation line for an optional
    /// server (xaft has no required ones).
    pub fn transcript_line(&self) -> String {
        format!(
            "MCP server '{}' is unavailable; continuing without its tools.",
            self.server
        )
    }
}

/// Called once per failed start.
pub type FailureListener = Arc<dyn Fn(&McpServerFailure) + Send + Sync>;

/// The three deadlines a manager applies.
#[derive(Debug, Clone, Copy)]
pub struct McpTimeouts {
    /// Per-server startup deadline (agenthicc's `startup_timeout_s`).
    pub startup: Duration,
    /// Per-call deadline (agenthicc's `tool_timeout_s`).
    pub tool: Duration,
    /// Quiet period before a `list_changed` notification refreshes.
    pub refresh_debounce: Duration,
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            startup: STARTUP_TIMEOUT,
            tool: TOOL_TIMEOUT,
            refresh_debounce: REFRESH_DEBOUNCE,
        }
    }
}

/// Seconds since the epoch.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// agenthicc's `_is_auth_error`.
fn is_auth_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "401",
        "403",
        "unauthorized",
        "authentication",
        "auth required",
    ]
    .iter()
    .any(|token| lower.contains(token))
}

/// One live connection.
struct Connection {
    transport: Arc<Transport>,
    /// What `initialize` answered, kept for refreshes and reconnects.
    hello: ServerHello,
    /// Relaunches spent on this connection (budget: `reconnect_attempts`).
    reconnects: u32,
    /// The last call failed at the connection level; relaunch before the next.
    broken: bool,
}

/// Everything behind the state lock. Never held across an `.await`.
#[derive(Default)]
struct State {
    specs: BTreeMap<String, ServerSpec>,
    statuses: BTreeMap<String, ServerStatus>,
    registration_errors: BTreeMap<String, String>,
    snapshots: BTreeMap<String, CatalogSnapshot>,
    connections: BTreeMap<String, Connection>,
    invalid_entries: Vec<String>,
    revision: u64,
    generation_id: u64,
    /// canonical name (`mcp:<server>:<tool>`) → binding.
    tools: BTreeMap<String, McpToolBinding>,
    /// provider name → canonical name.
    provider_names: BTreeMap<String, String>,
    /// Servers with a debounced refresh already scheduled.
    refresh_pending: BTreeSet<String>,
    closed: bool,
}

/// The manager. Always handled as an `Arc` ([`McpSessionManager::new`]).
pub struct McpSessionManager {
    state: std::sync::Mutex<State>,
    /// One lifecycle lock per configured name: a start, refresh, reconnect or
    /// disconnect of one server never interleaves with another of the same
    /// server, and never waits on a different server.
    locks: BTreeMap<String, tokio::sync::Mutex<()>>,
    workspace_root: PathBuf,
    guard: Option<NetworkGuard>,
    timeouts: McpTimeouts,
    failure_listener: std::sync::Mutex<Option<FailureListener>>,
    this: Weak<McpSessionManager>,
}

impl std::fmt::Debug for McpSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSessionManager")
            .field("servers", &self.locks.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl McpSessionManager {
    /// Register every entry without starting anything.
    ///
    /// `workspace_root` is the working directory a `stdio` server is started
    /// in; `guard`, when present, decides which remote hosts may be dialled.
    pub fn new(
        entries: Vec<McpEntry>,
        workspace_root: &Path,
        guard: Option<NetworkGuard>,
    ) -> Arc<Self> {
        Self::with_timeouts(entries, workspace_root, guard, McpTimeouts::default())
    }

    /// [`McpSessionManager::new`] with explicit deadlines (tests).
    pub fn with_timeouts(
        entries: Vec<McpEntry>,
        workspace_root: &Path,
        guard: Option<NetworkGuard>,
        timeouts: McpTimeouts,
    ) -> Arc<Self> {
        let mut state = State::default();
        let mut locks = BTreeMap::new();
        for entry in entries {
            match entry {
                McpEntry::Valid(config) => {
                    let spec = ServerSpec::new(config);
                    locks
                        .entry(spec.name().to_string())
                        .or_insert_with(|| tokio::sync::Mutex::new(()));
                    register(&mut state, spec);
                }
                McpEntry::Invalid { name } => state.invalid_entries.push(name),
            }
        }
        Arc::new_cyclic(|this| Self {
            state: std::sync::Mutex::new(state),
            locks,
            workspace_root: workspace_root.to_path_buf(),
            guard,
            timeouts,
            failure_listener: std::sync::Mutex::new(None),
            this: this.clone(),
        })
    }

    /// Be told about every server that fails to start (agenthicc's
    /// `McpServerFailed` event).
    pub fn set_failure_listener(&self, listener: FailureListener) {
        *self
            .failure_listener
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(listener);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn missing(name: &str) -> String {
        format!("No MCP server registered with name '{name}'")
    }

    /// Whether `name` is configured.
    pub fn contains(&self, name: &str) -> bool {
        self.state().specs.contains_key(name)
    }

    /// Configured server names, sorted.
    pub fn server_names(&self) -> Vec<String> {
        self.state().specs.keys().cloned().collect()
    }

    /// The status of one server, as agenthicc's `status(name)`.
    pub fn status(&self, name: &str) -> Result<Value, String> {
        self.state()
            .statuses
            .get(name)
            .map(ServerStatus::to_json)
            .ok_or_else(|| Self::missing(name))
    }

    /// Every server's status keyed by name, as agenthicc's `status()`.
    pub fn status_all(&self) -> Value {
        Value::Object(
            self.state()
                .statuses
                .iter()
                .map(|(name, status)| (name.clone(), status.to_json()))
                .collect(),
        )
    }

    /// The typed status of one server.
    pub fn server_status(&self, name: &str) -> Option<ServerStatus> {
        self.state().statuses.get(name).cloned()
    }

    /// The published snapshot of one server.
    pub fn snapshot(&self, name: &str) -> Option<CatalogSnapshot> {
        self.state().snapshots.get(name).cloned()
    }

    /// The manager-wide catalog revision.
    pub fn catalog_revision(&self) -> u64 {
        self.state().revision
    }

    /// Every published tool binding, sorted by canonical name.
    pub fn tool_bindings(&self) -> Vec<McpToolBinding> {
        self.state().tools.values().cloned().collect()
    }

    /// Every published tool as an agent tool (`mcp__<server>__<tool>`),
    /// sorted by canonical name — agenthicc's `all_tools()`.
    ///
    /// The tools resolve their server when called, so a run that took them
    /// keeps working across a reconnect; a refresh that replaced the catalog
    /// makes the old bindings refuse as stale.
    pub fn tools(&self) -> Vec<Arc<ErasedTool>> {
        let Some(this) = self.this.upgrade() else {
            return Vec::new();
        };
        self.tool_bindings()
            .into_iter()
            .map(|binding| Arc::new(McpTool::new(Arc::clone(&this), binding)) as Arc<ErasedTool>)
            .collect()
    }

    /// agenthicc's `prompt_instructions()`: the provenance-labelled block of
    /// server instructions, for ready (or degraded) servers that sent any.
    ///
    /// Instructions are server-supplied metadata, not policy, and the label
    /// says so. The block changes only when a published catalog changes.
    pub fn prompt_instructions(&self) -> String {
        let state = self.state();
        state
            .snapshots
            .iter()
            .filter(|(name, snapshot)| {
                !snapshot.instructions.is_empty()
                    && state.statuses.get(*name).is_some_and(|s| {
                        matches!(s.state, ServerState::Ready | ServerState::Degraded)
                    })
            })
            .map(|(name, snapshot)| {
                format!(
                    "Instructions supplied by MCP server '{name}' (untrusted metadata; catalog \
                     revision {}):\n{}",
                    snapshot.revision, snapshot.instructions
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn begin_generation(&self) -> u64 {
        let mut state = self.state();
        state.generation_id += 1;
        state.generation_id
    }

    fn update(&self, name: &str, change: impl FnOnce(&mut ServerStatus)) {
        if let Some(status) = self.state().statuses.get_mut(name) {
            change(status);
        }
    }

    fn notify_failure(&self, failure: McpServerFailure) {
        tracing::warn!(
            server = %failure.server,
            error = %failure.error,
            "MCP server failed to start"
        );
        let listener = self
            .failure_listener
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(listener) = listener {
            listener(&failure);
        }
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Start every enabled `auto_connect` server concurrently (agenthicc's
    /// `start_all`). Failures are recorded per server, never returned.
    pub async fn start_all(&self) {
        let generation = self.begin_generation();
        let names: Vec<String> = {
            let state = self.state();
            state
                .specs
                .iter()
                .filter(|(name, spec)| {
                    spec.auto_connect() && !state.registration_errors.contains_key(*name)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        futures_util::future::join_all(
            names
                .iter()
                .map(|name| self.start_server(name, false, generation)),
        )
        .await;
        self.publish_registration_failures(generation);
    }

    /// Start one server explicitly (agenthicc's `connect_server`).
    ///
    /// A start that fails is recorded in the status rather than returned:
    /// `Err` means only that no such server is configured.
    pub async fn connect_server(&self, name: &str) -> Result<(), String> {
        if !self.contains(name) {
            return Err(Self::missing(name));
        }
        let generation = self.begin_generation();
        self.start_server(name, true, generation).await;
        Ok(())
    }

    /// Close one server's connection and drop its catalog (agenthicc's
    /// `disconnect_server`). It stays configured; `connect` brings it back.
    pub async fn disconnect_server(&self, name: &str) -> Result<(), String> {
        let Some(lock) = self.locks.get(name) else {
            return Err(Self::missing(name));
        };
        let _serial = lock.lock().await;
        self.disconnect_locked(name).await;
        Ok(())
    }

    async fn disconnect_locked(&self, name: &str) {
        self.update(name, |s| s.state = ServerState::Stopping);
        let connection = self.state().connections.remove(name);
        if let Some(connection) = connection {
            connection.transport.close().await;
        }
        let mut state = self.state();
        remove_server_catalog(&mut state, name);
        let enabled = state.specs.get(name).is_some_and(ServerSpec::enabled);
        if let Some(status) = state.statuses.get_mut(name) {
            status.state = if enabled {
                ServerState::Stopped
            } else {
                ServerState::Disabled
            };
            status.tool_count = 0;
        }
    }

    /// Reconnect every enabled server and republish its catalog (agenthicc's
    /// `reload_all`). Unlike [`McpSessionManager::start_all`] this includes
    /// `auto_connect = false` servers: it is an explicit request to reload
    /// *all* of them. A broken server never stops the others from reloading.
    pub async fn reload_all(&self) {
        let generation = self.begin_generation();
        {
            let mut state = self.state();
            state.closed = false;
            state.refresh_pending.clear();
        }
        let names = self.server_names();
        futures_util::future::join_all(names.iter().map(|name| async move {
            if let Some(lock) = self.locks.get(name.as_str()) {
                let _serial = lock.lock().await;
                self.disconnect_locked(name).await;
            }
        }))
        .await;
        let candidates: Vec<String> = {
            let state = self.state();
            state
                .specs
                .iter()
                .filter(|(name, spec)| {
                    spec.enabled() && !state.registration_errors.contains_key(*name)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        futures_util::future::join_all(
            candidates
                .iter()
                .map(|name| self.start_server(name, true, generation)),
        )
        .await;
        self.publish_registration_failures(generation);
    }

    /// Publish malformed configured servers once per generation (agenthicc's
    /// `_publish_registration_failures`).
    fn publish_registration_failures(&self, generation: u64) {
        let failures: Vec<(String, String)> = {
            let mut state = self.state();
            let errors: Vec<(String, String)> = state
                .registration_errors
                .iter()
                .filter(|(name, _)| state.specs.get(*name).is_some_and(ServerSpec::enabled))
                .map(|(n, e)| (n.clone(), e.clone()))
                .collect();
            for (name, error) in &errors {
                if let Some(status) = state.statuses.get_mut(name) {
                    status.generation_id = generation;
                    status.last_error = Some(error.clone());
                }
            }
            errors
        };
        for (server, error) in failures {
            self.notify_failure(McpServerFailure {
                server,
                error,
                generation_id: generation,
            });
        }
    }

    async fn start_server(&self, name: &str, explicit: bool, generation: u64) {
        let Some(lock) = self.locks.get(name) else {
            return;
        };
        let _serial = lock.lock().await;
        let (spec, registration_error, current, alive) = {
            let state = self.state();
            if state.closed {
                return;
            }
            let Some(spec) = state.specs.get(name).cloned() else {
                return;
            };
            (
                spec,
                state.registration_errors.contains_key(name),
                state.statuses.get(name).map(|s| s.state),
                state
                    .connections
                    .get(name)
                    .is_some_and(|c| !c.broken && !c.transport.is_closed()),
            )
        };
        if registration_error && spec.enabled() {
            return;
        }
        if !spec.enabled() {
            self.update(name, |s| {
                s.state = ServerState::Disabled;
                s.last_error = None;
                s.generation_id = generation;
            });
            return;
        }
        if !explicit && !spec.auto_connect() {
            return;
        }
        if current == Some(ServerState::Ready) && alive {
            return;
        }
        let auth_state = spec.auth_state().to_string();
        if auth_state == "needs_auth" {
            self.update(name, |s| {
                s.state = ServerState::NeedsAuth;
                s.auth_state = auth_state.clone();
                s.generation_id = generation;
            });
            return;
        }
        let started_at = now();
        self.update(name, |s| {
            s.state = ServerState::Starting;
            s.last_error = None;
            s.started_at = Some(started_at);
            s.auth_state = auth_state.clone();
            s.generation_id = generation;
        });
        // A previous, dead connection is replaced, not leaked.
        let stale = self.state().connections.remove(name);
        if let Some(stale) = stale {
            stale.transport.close().await;
        }

        let attempts = spec.reconnect_attempts().saturating_add(1);
        match self.open_and_discover(&spec, attempts).await {
            Ok((transport, discovered)) => {
                if self.state().closed {
                    transport.close().await;
                    return;
                }
                let hello = discovered.hello.clone();
                match self.publish(&spec, discovered) {
                    Ok(snapshot) => {
                        let transport = Arc::new(transport);
                        self.watch_notifications(name, &transport);
                        let (count, revision) = (snapshot.tools.len(), snapshot.revision);
                        let mut state = self.state();
                        state.connections.insert(
                            name.to_string(),
                            Connection {
                                transport,
                                hello,
                                reconnects: 0,
                                broken: false,
                            },
                        );
                        if let Some(s) = state.statuses.get_mut(name) {
                            s.state = ServerState::Ready;
                            s.tool_count = count;
                            s.catalog_revision = revision;
                            s.last_success_at = Some(now());
                            s.last_error = None;
                            s.generation_id = generation;
                        }
                    }
                    Err(error) => {
                        transport.close().await;
                        self.record_failure(name, &spec, error, generation);
                    }
                }
            }
            Err(error) => self.record_failure(name, &spec, error, generation),
        }
    }

    fn record_failure(&self, name: &str, spec: &ServerSpec, error: String, generation: u64) {
        let error = spec.redact(&error);
        let state = if is_auth_error(&error) {
            ServerState::NeedsAuth
        } else {
            ServerState::Failed
        };
        self.update(name, |s| {
            s.state = state;
            s.last_error = Some(error.clone());
            s.generation_id = generation;
        });
        self.notify_failure(McpServerFailure {
            server: name.to_string(),
            error,
            generation_id: generation,
        });
    }

    /// Connect (up to `attempts` tries, the delay doubling between them,
    /// inside the startup deadline) and list the catalog. Returns the open
    /// transport with what it discovered.
    async fn open_and_discover(
        &self,
        spec: &ServerSpec,
        attempts: u32,
    ) -> Result<(Transport, Discovered), String> {
        let name = spec.name().to_string();
        if spec.transport() != "stdio" {
            if let Some(guard) = &self.guard {
                guard
                    .check(&spec.resolved_url())
                    .map_err(|e| format!("Failed to connect to MCP server '{name}': {e}"))?;
            }
        }
        let deadline = self.timeouts.startup;
        let connect = async {
            let mut last_error = String::new();
            for attempt in 0..attempts {
                if attempt > 0 {
                    let factor = 2u32.saturating_pow(attempt - 1);
                    tokio::time::sleep(spec.reconnect_delay().saturating_mul(factor)).await;
                }
                match Transport::open(spec, &self.workspace_root, deadline).await {
                    Ok(transport) => match initialize(&transport, deadline).await {
                        Ok(hello) => return Ok((transport, hello)),
                        Err(error) => {
                            last_error = with_stderr(error.to_string(), &transport.stderr_tail());
                            transport.close().await;
                        }
                    },
                    Err(error) => last_error = error.to_string(),
                }
            }
            Err(format!(
                "Failed to connect to MCP server '{name}' after {attempts} attempts: {last_error}"
            ))
        };
        let (transport, hello) = match tokio::time::timeout(deadline, connect).await {
            Ok(outcome) => outcome?,
            Err(_) => {
                return Err(format!(
                    "MCP server '{name}' did not finish starting within {}s",
                    deadline.as_secs_f64()
                ));
            }
        };
        let tools = match list_tools(&transport, deadline).await {
            Ok(tools) => tools,
            Err(error) => {
                let message = with_stderr(error.to_string(), &transport.stderr_tail());
                transport.close().await;
                return Err(message);
            }
        };
        let prompts = list_optional(&transport, &hello, "prompts", deadline).await;
        let resources = list_optional(&transport, &hello, "resources", deadline).await;
        Ok((
            transport,
            Discovered {
                hello,
                tools,
                prompts,
                resources,
            },
        ))
    }

    /// Route `notifications/tools/list_changed` from `transport` to a
    /// debounced refresh of `name` (agenthicc's `_install_change_callback`).
    fn watch_notifications(&self, name: &str, transport: &Transport) {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        transport.set_notification_listener(tx);
        let this = self.this.clone();
        let server = name.to_string();
        tokio::spawn(async move {
            while let Some(method) = rx.recv().await {
                if method != "notifications/tools/list_changed" {
                    continue;
                }
                let Some(manager) = this.upgrade() else {
                    break;
                };
                manager.schedule_refresh(&server);
            }
        });
    }

    /// agenthicc's `notify_tools_changed`: one debounced refresh at a time.
    fn schedule_refresh(&self, name: &str) {
        {
            let mut state = self.state();
            if state.closed || !state.refresh_pending.insert(name.to_string()) {
                return;
            }
        }
        let this = self.this.clone();
        let server = name.to_string();
        let debounce = self.timeouts.refresh_debounce;
        tokio::spawn(async move {
            tokio::time::sleep(debounce).await;
            let Some(manager) = this.upgrade() else {
                return;
            };
            manager.state().refresh_pending.remove(&server);
            manager.refresh_server(&server).await;
        });
    }

    /// Validate and publish a discovered catalog (agenthicc's `_publish`):
    /// the snapshot, and the provider-safe names its tools are bound to.
    ///
    /// Every name is decided before anything is replaced, so a collision
    /// fails this server's candidate without touching any other server's
    /// last-good tools.
    fn publish(
        &self,
        spec: &ServerSpec,
        discovered: Discovered,
    ) -> Result<CatalogSnapshot, String> {
        let name = spec.name();
        if discovered.tools.len() > MAX_TOOL_COUNT {
            return Err(format!("MCP server '{name}' returned too many tools"));
        }
        let mut tools: Vec<ToolSchema> = Vec::with_capacity(discovered.tools.len());
        for raw in &discovered.tools {
            let tool_name = raw
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if tool_name.is_empty() {
                return Err(format!(
                    "MCP server '{name}' returned a tool without a name"
                ));
            }
            if tools.iter().any(|t| t.name == tool_name) {
                return Err(format!(
                    "MCP server '{name}' returned duplicate tool '{tool_name}'"
                ));
            }
            let input_schema = match raw.get("inputSchema") {
                Some(Value::Object(schema)) => Value::Object(schema.clone()),
                _ => json!({}),
            };
            if input_schema.to_string().len() > MAX_SCHEMA_BYTES {
                return Err(format!(
                    "MCP tool '{tool_name}' returned an oversized input schema"
                ));
            }
            tools.push(ToolSchema {
                name: tool_name,
                description: raw
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                input_schema,
            });
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let instructions: String = discovered
            .hello
            .instructions
            .chars()
            .take(MAX_INSTRUCTION_CHARS)
            .collect();
        let canonical = json!({
            "server": name,
            "tools": tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<_>>(),
            "instructions": instructions,
            "capabilities": discovered.hello.capabilities,
            "protocol_version": discovered.hello.protocol_version,
            "server_info": discovered.hello.server_info,
            "prompts": discovered.prompts,
            "resources": discovered.resources,
            "enabled_tools": [],
            "disabled_tools": [],
        });
        let digest = sha256_hex(&pyjson::dumps_with_separators(&canonical, ",", ":"));
        let filter_hash = sha256_hex(&pyjson::dumps(&json!([[], []])));

        let mut state = self.state();
        let revision = match state.snapshots.get(name) {
            Some(previous) if previous.catalog_hash == digest => previous.revision,
            _ => state.revision + 1,
        };

        // Decide every provider name against the other servers' names first.
        let mut provider_names = state.provider_names.clone();
        provider_names.retain(|_, canonical| {
            state
                .tools
                .get(canonical)
                .is_none_or(|binding| binding.server != name)
        });
        let mut bindings: Vec<McpToolBinding> = Vec::with_capacity(tools.len());
        for tool in &tools {
            let canonical_name = format!("mcp:{name}:{}", tool.name);
            let mut provider =
                crate::tool::provider_safe_tool_name(&format!("mcp__{name}__{}", tool.name));
            if provider_names
                .get(&provider)
                .is_some_and(|owner| *owner != canonical_name)
            {
                provider = crate::tool::provider_safe_tool_name(&format!(
                    "mcp__{name}__{}#{}",
                    tool.name,
                    &digest[..12]
                ));
            }
            if provider_names
                .get(&provider)
                .is_some_and(|owner| *owner != canonical_name)
            {
                return Err(format!(
                    "provider tool name collision for '{canonical_name}'"
                ));
            }
            provider_names.insert(provider.clone(), canonical_name.clone());
            bindings.push(McpToolBinding {
                provider_name: provider,
                canonical_name,
                server: name.to_string(),
                tool: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                catalog_revision: revision,
            });
        }

        state.revision = state.revision.max(revision);
        let snapshot = CatalogSnapshot {
            server_name: name.to_string(),
            revision,
            protocol_version: discovered.hello.protocol_version,
            server_info: discovered.hello.server_info,
            capabilities: discovered.hello.capabilities,
            instructions,
            tools,
            prompts: discovered.prompts,
            resources: discovered.resources,
            tool_filter_hash: filter_hash[..16].to_string(),
            catalog_hash: digest,
            captured_at: now(),
        };
        state.tools.retain(|_, binding| binding.server != name);
        for binding in bindings {
            state.tools.insert(binding.canonical_name.clone(), binding);
        }
        state.provider_names = provider_names;
        state.snapshots.insert(name.to_string(), snapshot.clone());
        Ok(snapshot)
    }

    /// Refetch one server's catalog (agenthicc's `refresh_server`).
    ///
    /// `None` when the server is unknown, failed registration, or could not
    /// be started — agenthicc's `{"status": "unavailable"}` case. A server
    /// with no live connection is started instead.
    pub async fn refresh_server(&self, name: &str) -> Option<CatalogSnapshot> {
        let lock = self.locks.get(name)?;
        let serial = lock.lock().await;
        let (spec, live) = {
            let state = self.state();
            if state.closed {
                return None;
            }
            let spec = state.specs.get(name)?.clone();
            if state.registration_errors.contains_key(name) && spec.enabled() {
                return None;
            }
            let live = state
                .connections
                .get(name)
                .filter(|c| !c.broken && !c.transport.is_closed())
                .map(|c| (Arc::clone(&c.transport), c.hello.clone()));
            (spec, live)
        };
        let Some((transport, hello)) = live else {
            // Not connected: a refresh is a start (agenthicc's
            // `start_server(name, explicit=True)`), which takes the lock itself.
            drop(serial);
            let generation = self.state().generation_id;
            self.start_server(name, true, generation).await;
            return self.snapshot(name);
        };
        let prior = self.snapshot(name);
        self.update(name, |s| s.state = ServerState::Refreshing);
        let deadline = self.timeouts.startup;
        let outcome = match list_tools(&transport, deadline).await {
            Ok(tools) => Ok(tools),
            Err(error) => Err(with_stderr(error.to_string(), &transport.stderr_tail())),
        };
        match outcome {
            Ok(tools) => {
                let prompts = list_optional(&transport, &hello, "prompts", deadline).await;
                let resources = list_optional(&transport, &hello, "resources", deadline).await;
                let discovered = Discovered {
                    hello,
                    tools,
                    prompts,
                    resources,
                };
                match self.publish(&spec, discovered) {
                    Ok(snapshot) => {
                        let (count, revision) = (snapshot.tools.len(), snapshot.revision);
                        self.update(name, |s| {
                            s.state = ServerState::Ready;
                            s.tool_count = count;
                            s.catalog_revision = revision;
                            s.last_success_at = Some(now());
                            s.last_error = None;
                        });
                        Some(snapshot)
                    }
                    Err(error) => self.refresh_failed(name, &spec, error, prior),
                }
            }
            Err(error) => self.refresh_failed(name, &spec, error, prior),
        }
    }

    fn refresh_failed(
        &self,
        name: &str,
        spec: &ServerSpec,
        error: String,
        prior: Option<CatalogSnapshot>,
    ) -> Option<CatalogSnapshot> {
        let error = spec.redact(&error);
        let state = if prior.is_some() {
            ServerState::Degraded
        } else {
            ServerState::Failed
        };
        self.update(name, |s| {
            s.state = state;
            s.last_error = Some(error.clone());
        });
        prior
    }

    /// Validate and, for enabled auto-connect servers, live-start each server
    /// (agenthicc's `doctor`). Never invokes a tool.
    pub async fn doctor(&self, name: Option<&str>) -> Result<Vec<Value>, String> {
        let names: Vec<String> = match name {
            Some(name) => {
                if !self.contains(name) {
                    return Err(Self::missing(name));
                }
                vec![name.to_string()]
            }
            None => self.server_names(),
        };
        let mut results = Vec::new();
        for server in names {
            let (spec, registration_error) = {
                let state = self.state();
                (
                    state.specs[&server].clone(),
                    state.registration_errors.get(&server).cloned(),
                )
            };
            let mut item = Map::new();
            item.insert("server".into(), json!(server));
            item.insert("config".into(), spec.redacted());
            match registration_error.map_or_else(|| spec.validate(), Err) {
                Ok(()) => {
                    if spec.enabled() && spec.auto_connect() {
                        let generation = self.state().generation_id;
                        self.start_server(&server, true, generation).await;
                    }
                    item.insert("status".into(), self.status(&server).unwrap_or(Value::Null));
                }
                Err(error) => {
                    item.insert("status".into(), json!("failed"));
                    item.insert("error".into(), json!(spec.redact(&error)));
                }
            }
            results.push(Value::Object(item));
        }
        if name.is_none() {
            // Stanzas the lenient reader could not even build have no spec to
            // validate, but a diagnosis that skipped them would be incomplete.
            let invalid = self.state().invalid_entries.clone();
            for invalid in invalid {
                results.push(json!({
                    "server": invalid,
                    "config": {"name": invalid, "status": "invalid"},
                    "status": "failed",
                    "error": "invalid MCP server configuration",
                }));
            }
        }
        Ok(results)
    }

    // ── Calls ────────────────────────────────────────────────────────────

    /// Call the tool `binding` names and return agenthicc's extracted
    /// payload (`_extract_tool_content`).
    ///
    /// `Err` carries the message the agent sees: a stale binding, a server
    /// that is not connected, a transport failure (`MCP call s/t failed: …`)
    /// or a tool that reported `isError` (`MCP server 's' returned error for
    /// 't': …`) — each redacted of the server's secrets.
    pub async fn call_tool(
        &self,
        binding: &McpToolBinding,
        arguments: Value,
    ) -> Result<Value, String> {
        {
            let state = self.state();
            let current = state
                .tools
                .get(&binding.canonical_name)
                .is_some_and(|b| b.catalog_revision == binding.catalog_revision);
            if !current {
                return Err(format!(
                    "MCP tool '{}' belongs to a stale catalog revision",
                    binding.canonical_name
                ));
            }
        }
        let server = binding.server.as_str();
        let spec = self
            .state()
            .specs
            .get(server)
            .cloned()
            .ok_or_else(|| Self::missing(server))?;
        let transport = self.live_transport(server, &spec).await?;
        match call_tool(&transport, &binding.tool, arguments, self.timeouts.tool).await {
            Ok(result) => {
                if result.get("isError").and_then(Value::as_bool) == Some(true) {
                    return Err(spec.redact(&format!(
                        "MCP server '{server}' returned error for '{}': {}",
                        binding.tool,
                        extract_text_content(&result)
                    )));
                }
                Ok(extract_tool_content(&result))
            }
            Err(error) => {
                if matches!(error, McpError::Closed(_) | McpError::Transport(_)) {
                    if let Some(connection) = self.state().connections.get_mut(server) {
                        if Arc::ptr_eq(&connection.transport, &transport) {
                            connection.broken = true;
                        }
                    }
                }
                Err(spec.redact(&with_stderr(
                    format!("MCP call {server}/{} failed: {error}", binding.tool),
                    &transport.stderr_tail(),
                )))
            }
        }
    }

    /// The live transport for `name`, relaunching a dropped connection when
    /// the server's `reconnect_attempts` budget allows.
    async fn live_transport(
        &self,
        name: &str,
        spec: &ServerSpec,
    ) -> Result<Arc<Transport>, String> {
        let not_connected = || format!("Server '{name}' is not connected");
        let healthy = |state: &State| {
            state
                .connections
                .get(name)
                .filter(|c| !c.broken && !c.transport.is_closed())
                .map(|c| Arc::clone(&c.transport))
        };
        {
            let state = self.state();
            if !state.connections.contains_key(name) {
                return Err(not_connected());
            }
            if let Some(transport) = healthy(&state) {
                return Ok(transport);
            }
        }
        let Some(lock) = self.locks.get(name) else {
            return Err(not_connected());
        };
        let _serial = lock.lock().await;
        // Another call may have relaunched it while this one waited.
        let reconnects = {
            let state = self.state();
            if let Some(transport) = healthy(&state) {
                return Ok(transport);
            }
            match state.connections.get(name) {
                Some(c) => c.reconnects,
                None => return Err(not_connected()),
            }
        };
        let dead = self.state().connections.remove(name);
        if let Some(dead) = dead {
            dead.transport.close().await;
        }
        let generation = self.state().generation_id;
        if reconnects >= spec.reconnect_attempts() {
            let error = format!(
                "MCP server '{name}' disconnected and its {} reconnect attempt(s) are spent",
                spec.reconnect_attempts()
            );
            {
                let mut state = self.state();
                remove_server_catalog(&mut state, name);
                if let Some(s) = state.statuses.get_mut(name) {
                    s.state = ServerState::Failed;
                    s.tool_count = 0;
                    s.last_error = Some(error.clone());
                }
            }
            self.notify_failure(McpServerFailure {
                server: name.to_string(),
                error,
                generation_id: generation,
            });
            return Err(not_connected());
        }
        tracing::warn!(
            server = name,
            attempt = reconnects + 1,
            "MCP server disconnected; relaunching"
        );
        match self.open_and_discover(spec, 1).await {
            Ok((transport, discovered)) => {
                let hello = discovered.hello.clone();
                match self.publish(spec, discovered) {
                    Ok(snapshot) => {
                        let transport = Arc::new(transport);
                        self.watch_notifications(name, &transport);
                        let mut state = self.state();
                        state.connections.insert(
                            name.to_string(),
                            Connection {
                                transport: Arc::clone(&transport),
                                hello,
                                reconnects: reconnects + 1,
                                broken: false,
                            },
                        );
                        if let Some(s) = state.statuses.get_mut(name) {
                            s.state = ServerState::Ready;
                            s.tool_count = snapshot.tools.len();
                            s.catalog_revision = snapshot.revision;
                            s.last_success_at = Some(now());
                            s.last_error = None;
                        }
                        Ok(transport)
                    }
                    Err(error) => {
                        transport.close().await;
                        self.mark_lost(name, spec, error, generation);
                        Err(not_connected())
                    }
                }
            }
            Err(error) => {
                self.mark_lost(name, spec, error, generation);
                Err(not_connected())
            }
        }
    }

    fn mark_lost(&self, name: &str, spec: &ServerSpec, error: String, generation: u64) {
        remove_server_catalog(&mut self.state(), name);
        self.update(name, |s| s.tool_count = 0);
        self.record_failure(name, spec, error, generation);
    }

    /// Close every connection exactly once; nothing restarts afterwards
    /// except an explicit [`McpSessionManager::reload_all`].
    pub async fn shutdown(&self) {
        let connections: Vec<(String, Connection)> = {
            let mut state = self.state();
            state.closed = true;
            state.refresh_pending.clear();
            std::mem::take(&mut state.connections).into_iter().collect()
        };
        for (name, connection) in connections {
            self.update(&name, |s| s.state = ServerState::Stopping);
            connection.transport.close().await;
            self.update(&name, |s| {
                s.state = ServerState::Stopped;
                s.tool_count = 0;
            });
        }
        let mut state = self.state();
        state.snapshots.clear();
        state.tools.clear();
        state.provider_names.clear();
    }
}

/// Register one spec into `state` (validation, duplicate names, initial
/// status) without starting it.
fn register(state: &mut State, spec: ServerSpec) {
    let name = spec.name().to_string();
    if state.specs.contains_key(&name) {
        let error = format!("MCP server '{name}' is already registered");
        state
            .registration_errors
            .insert(name.clone(), error.clone());
        if let Some(status) = state.statuses.get_mut(&name) {
            status.state = ServerState::Failed;
            status.last_error = Some(error);
        }
        return;
    }
    let transport = spec.transport();
    let auth_state = spec.auth_state().to_string();
    let status = match spec.validate() {
        Err(error) if spec.enabled() => {
            let error = spec.redact(&error);
            state
                .registration_errors
                .insert(name.clone(), error.clone());
            ServerStatus {
                name: name.clone(),
                state: ServerState::Failed,
                transport,
                tool_count: 0,
                catalog_revision: 0,
                last_error: Some(error),
                started_at: None,
                last_success_at: None,
                auth_state,
                generation_id: 0,
            }
        }
        outcome => {
            if let Err(error) = outcome {
                // A disabled server is never started, so its validation
                // error only matters to `doctor`, which re-validates.
                state.registration_errors.insert(name.clone(), error);
            }
            let server_state = if !spec.enabled() {
                ServerState::Disabled
            } else if auth_state == "needs_auth" {
                ServerState::NeedsAuth
            } else {
                ServerState::Configured
            };
            ServerStatus {
                name: name.clone(),
                state: server_state,
                transport,
                tool_count: 0,
                catalog_revision: 0,
                last_error: None,
                started_at: None,
                last_success_at: None,
                auth_state: if spec.enabled() {
                    auth_state
                } else {
                    "not_required".into()
                },
                generation_id: 0,
            }
        }
    };
    state.statuses.insert(name.clone(), status);
    state.specs.insert(name, spec);
}

/// Drop `name`'s snapshot and every tool bound to it.
fn remove_server_catalog(state: &mut State, name: &str) {
    let gone: Vec<String> = state
        .tools
        .iter()
        .filter(|(_, b)| b.server == name)
        .map(|(canonical, _)| canonical.clone())
        .collect();
    for canonical in gone {
        if let Some(binding) = state.tools.remove(&canonical) {
            state.provider_names.remove(&binding.provider_name);
        }
    }
    state.snapshots.remove(name);
}

/// Everything one start discovered.
struct Discovered {
    hello: ServerHello,
    tools: Vec<Value>,
    prompts: Vec<Value>,
    resources: Vec<Value>,
}

/// agenthicc's `_bridge_error`: append a bounded stderr tail.
fn with_stderr(error: String, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return error;
    }
    let tail: String = {
        let chars: Vec<char> = stderr.chars().collect();
        let start = chars.len().saturating_sub(STDERR_IN_ERROR);
        chars[start..].iter().collect()
    };
    format!("{error}; stderr: {tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(toml: &str) -> McpEntry {
        McpEntry::Valid(toml::from_str(toml).unwrap())
    }

    #[test]
    fn statuses_start_configured_disabled_or_failed() {
        let manager = McpSessionManager::new(
            vec![
                entry("name = \"ok\"\ncommand = \"srv\""),
                entry("name = \"off\"\ncommand = \"srv\"\nenabled = false"),
                entry("name = \"bad\"\ntransport = \"pigeon\""),
            ],
            Path::new("/"),
            None,
        );
        assert_eq!(manager.status("ok").unwrap()["status"], "configured");
        assert_eq!(manager.status("off").unwrap()["status"], "disabled");
        let bad = manager.status("bad").unwrap();
        assert_eq!(bad["status"], "failed");
        assert_eq!(bad["last_error"], "unsupported MCP transport: 'pigeon'");
        assert_eq!(
            manager.status("nope").unwrap_err(),
            "No MCP server registered with name 'nope'"
        );
        let all = manager.status_all();
        let names: Vec<&String> = all.as_object().unwrap().keys().collect();
        assert_eq!(names, ["bad", "off", "ok"], "keyed and sorted by name");
    }

    #[test]
    fn the_status_record_has_agenthiccs_ten_keys() {
        let manager = McpSessionManager::new(
            vec![entry("name = \"a\"\ncommand = \"srv\"")],
            Path::new("/"),
            None,
        );
        let status = manager.status("a").unwrap();
        let mut keys: Vec<&str> = status
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "auth_state",
                "catalog_revision",
                "generation_id",
                "last_error",
                "last_success_at",
                "name",
                "started_at",
                "status",
                "tool_count",
                "transport"
            ]
        );
    }

    #[test]
    fn a_duplicate_name_fails_the_name_but_not_the_file() {
        let manager = McpSessionManager::new(
            vec![
                entry("name = \"a\"\ncommand = \"one\""),
                entry("name = \"a\"\ncommand = \"two\""),
                entry("name = \"b\"\ncommand = \"three\""),
            ],
            Path::new("/"),
            None,
        );
        assert_eq!(manager.status("a").unwrap()["status"], "failed");
        assert_eq!(manager.status("b").unwrap()["status"], "configured");
    }

    #[test]
    fn the_guard_follows_xafts_fail_closed_contract() {
        let mut config = XaftConfig::default();
        let guard = NetworkGuard::from_config(&config);
        assert_eq!(
            guard.check("https://mcp.example.com/mcp").unwrap_err(),
            "Outbound request to 'mcp.example.com' is not on the network allow-list."
        );
        config.security.network_allow_list = vec!["example.com".into()];
        let guard = NetworkGuard::from_config(&config);
        assert!(guard.check("https://mcp.example.com/mcp").is_ok());
        assert!(guard.check("https://notexample.com/").is_err());
        config.security.network_allow_list.clear();
        config.security.sandbox_mode = false;
        assert!(
            NetworkGuard::from_config(&config)
                .check("https://any.host/")
                .is_ok()
        );
    }

    #[test]
    fn auth_errors_are_recognised_like_agenthicc() {
        assert!(is_auth_error("HTTP 401 Unauthorized"));
        assert!(is_auth_error("authentication required"));
        assert!(!is_auth_error("connection refused"));
    }

    #[test]
    fn stderr_is_appended_and_bounded() {
        assert_eq!(with_stderr("boom".into(), ""), "boom");
        assert_eq!(with_stderr("boom".into(), "trace\n"), "boom; stderr: trace");
        let long = "x".repeat(STDERR_IN_ERROR + 10);
        assert_eq!(
            with_stderr("e".into(), &long).len(),
            "e; stderr: ".len() + STDERR_IN_ERROR
        );
    }

    #[test]
    fn the_failure_line_is_agenthiccs_optional_server_line() {
        let failure = McpServerFailure {
            server: "docs".into(),
            error: "boom".into(),
            generation_id: 1,
        };
        assert_eq!(
            failure.transcript_line(),
            "MCP server 'docs' is unavailable; continuing without its tools."
        );
    }

    #[tokio::test]
    async fn a_missing_credential_is_needs_auth_without_a_connection_attempt() {
        let manager = McpSessionManager::new(
            vec![entry(
                "name = \"a\"\ncommand = \"/definitely/not/run\"\ntoken_env = \"XAFT_MCP_TEST_NEVER_SET\"",
            )],
            Path::new("/"),
            None,
        );
        manager.connect_server("a").await.unwrap();
        let status = manager.status("a").unwrap();
        assert_eq!(status["status"], "needs_auth");
        assert_eq!(status["auth_state"], "needs_auth");
        assert_eq!(status["last_error"], Value::Null);
        assert_eq!(status["generation_id"], 1);
    }

    #[tokio::test]
    async fn a_command_that_cannot_start_fails_with_agenthiccs_message() {
        let manager = McpSessionManager::new(
            vec![entry(
                "name = \"a\"\ncommand = \"/definitely/not/a/program\"\nreconnect_attempts = 1\nreconnect_delay_seconds = 0.0",
            )],
            Path::new("/"),
            None,
        );
        let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&failures);
        manager.set_failure_listener(Arc::new(move |f: &McpServerFailure| {
            sink.lock().unwrap().push(f.server.clone());
        }));
        manager.connect_server("a").await.unwrap();
        let status = manager.status("a").unwrap();
        assert_eq!(status["status"], "failed");
        let error = status["last_error"].as_str().unwrap();
        assert!(
            error.starts_with(
                "Failed to connect to MCP server 'a' after 2 attempts: failed to start '<redacted>'"
            ),
            "{error}"
        );
        assert!(status["started_at"].is_f64());
        assert_eq!(
            *failures.lock().unwrap(),
            ["a"],
            "agenthicc's McpServerFailed"
        );
    }

    #[tokio::test]
    async fn a_refused_host_is_never_dialled() {
        let manager = McpSessionManager::new(
            vec![entry(
                "name = \"r\"\ntransport = \"http\"\nurl = \"https://blocked.example/mcp\"",
            )],
            Path::new("/"),
            Some(NetworkGuard::from_config(&XaftConfig::default())),
        );
        manager.connect_server("r").await.unwrap();
        let status = manager.status("r").unwrap();
        assert_eq!(status["status"], "failed");
        assert_eq!(
            status["last_error"],
            "Failed to connect to MCP server 'r': Outbound request to 'blocked.example' is not on the network allow-list."
        );
    }

    #[tokio::test]
    async fn doctor_reports_unknown_names_and_validation_failures() {
        let manager = McpSessionManager::new(
            vec![
                entry("name = \"bad\"\ntransport = \"pigeon\""),
                entry("name = \"manual\"\ncommand = \"srv\"\nauto_connect = false"),
                McpEntry::Invalid {
                    name: "<invalid>".into(),
                },
            ],
            Path::new("/"),
            None,
        );
        assert_eq!(
            manager.doctor(Some("ghost")).await.unwrap_err(),
            "No MCP server registered with name 'ghost'"
        );
        let report = manager.doctor(None).await.unwrap();
        assert_eq!(report.len(), 3);
        assert_eq!(report[0]["server"], "bad");
        assert_eq!(report[0]["status"], "failed");
        assert_eq!(report[0]["error"], "unsupported MCP transport: 'pigeon'");
        // auto_connect = false: validated, not started.
        assert_eq!(report[1]["status"]["status"], "configured");
        assert_eq!(report[2]["error"], "invalid MCP server configuration");
    }

    #[tokio::test]
    async fn start_all_skips_manual_servers_and_publishes_registration_failures() {
        let manager = McpSessionManager::new(
            vec![
                entry("name = \"bad\"\ntransport = \"pigeon\""),
                entry("name = \"manual\"\ncommand = \"srv\"\nauto_connect = false"),
            ],
            Path::new("/"),
            None,
        );
        let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&failures);
        manager.set_failure_listener(Arc::new(move |f: &McpServerFailure| {
            sink.lock()
                .unwrap()
                .push((f.server.clone(), f.error.clone()));
        }));
        manager.start_all().await;
        assert_eq!(manager.status("manual").unwrap()["status"], "configured");
        assert_eq!(
            *failures.lock().unwrap(),
            [(
                "bad".to_string(),
                "unsupported MCP transport: 'pigeon'".to_string()
            )]
        );
        assert_eq!(manager.status("bad").unwrap()["generation_id"], 1);
        assert!(manager.tools().is_empty());
    }

    #[tokio::test]
    async fn calls_and_refreshes_on_an_unconnected_server_report_it() {
        let manager = McpSessionManager::new(
            vec![entry(
                "name = \"a\"\ncommand = \"srv\"\nauto_connect = false",
            )],
            Path::new("/"),
            None,
        );
        let binding = McpToolBinding {
            provider_name: "mcp__a__x".into(),
            canonical_name: "mcp:a:x".into(),
            server: "a".into(),
            tool: "x".into(),
            description: String::new(),
            input_schema: json!({}),
            catalog_revision: 1,
        };
        assert_eq!(
            manager.call_tool(&binding, json!({})).await.unwrap_err(),
            "MCP tool 'mcp:a:x' belongs to a stale catalog revision",
            "a binding the manager never published is stale"
        );
        manager.shutdown().await;
        assert!(manager.refresh_server("a").await.is_none(), "closed");
    }
}
