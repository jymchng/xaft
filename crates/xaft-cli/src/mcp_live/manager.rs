//! A short-lived MCP session manager for the `mcp connect` / `refresh` /
//! `doctor` commands.
//!
//! Port of the parts of agenthicc's `McpSessionManager`
//! (`tools/mcp_manager.py`) that those commands exercise: registration with
//! validation, the typed status record, explicit start with retries and a
//! startup deadline, immutable catalog snapshots with a content hash and a
//! revision, `doctor`, and shutdown.
//!
//! # The status record
//!
//! [`ServerStatus::to_json`] has exactly agenthicc's ten keys (`name`,
//! `status`, `transport`, `tool_count`, `catalog_revision`, `last_error`,
//! `started_at`, `last_success_at`, `auth_state`, `generation_id`) and its
//! twelve state names (`configured`, `disabled`, `starting`, `ready`,
//! `refreshing`, `degraded`, `failed`, `authenticating`, `needs_auth`,
//! `cancelled`, `stopping`, `stopped`).
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use xaft_config::{ToolPolicy, XaftConfig};

use super::client::{ServerHello, initialize, list_optional, list_tools};
use super::spec::{STARTUP_TIMEOUT, ServerSpec};
use super::transport::Transport;
use crate::mcp::McpEntry;
use crate::pyjson;

/// agenthicc's `_MAX_TOOL_COUNT`.
const MAX_TOOL_COUNT: usize = 10_000;
/// agenthicc's `_MAX_SCHEMA_BYTES`.
const MAX_SCHEMA_BYTES: usize = 1_000_000;
/// agenthicc's `_MAX_INSTRUCTION_CHARS`.
const MAX_INSTRUCTION_CHARS: usize = 32_000;
/// How much stderr is appended to a failure (agenthicc: the last 4096 chars).
const STDERR_IN_ERROR: usize = 4096;

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

/// Seconds since the epoch.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn sha256_hex(text: &str) -> String {
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

/// The manager.
pub struct McpSessionManager {
    specs: BTreeMap<String, ServerSpec>,
    statuses: BTreeMap<String, ServerStatus>,
    registration_errors: BTreeMap<String, String>,
    snapshots: BTreeMap<String, CatalogSnapshot>,
    connections: BTreeMap<String, Transport>,
    invalid_entries: Vec<String>,
    revision: u64,
    generation_id: u64,
    workspace_root: PathBuf,
    guard: Option<NetworkGuard>,
    startup_timeout: Duration,
}

impl McpSessionManager {
    /// Register every entry of a config file (as read by
    /// [`crate::mcp::read_mcp_servers`]) without starting anything.
    pub fn new(entries: Vec<McpEntry>, workspace_root: &Path, guard: Option<NetworkGuard>) -> Self {
        let mut manager = Self {
            specs: BTreeMap::new(),
            statuses: BTreeMap::new(),
            registration_errors: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            connections: BTreeMap::new(),
            invalid_entries: Vec::new(),
            revision: 0,
            generation_id: 0,
            workspace_root: workspace_root.to_path_buf(),
            guard,
            startup_timeout: STARTUP_TIMEOUT,
        };
        for entry in entries {
            match entry {
                McpEntry::Valid(config) => manager.register(ServerSpec::new(config)),
                McpEntry::Invalid { name } => manager.invalid_entries.push(name),
            }
        }
        manager
    }

    /// Override the per-server startup deadline (tests).
    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    fn register(&mut self, spec: ServerSpec) {
        let name = spec.name().to_string();
        if self.specs.contains_key(&name) {
            let error = format!("MCP server '{name}' is already registered");
            self.registration_errors.insert(name.clone(), error.clone());
            if let Some(status) = self.statuses.get_mut(&name) {
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
                self.registration_errors.insert(name.clone(), error.clone());
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
                    self.registration_errors.insert(name.clone(), error);
                }
                let state = if !spec.enabled() {
                    ServerState::Disabled
                } else if auth_state == "needs_auth" {
                    ServerState::NeedsAuth
                } else {
                    ServerState::Configured
                };
                ServerStatus {
                    name: name.clone(),
                    state,
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
        self.statuses.insert(name.clone(), status);
        self.specs.insert(name, spec);
    }

    fn missing(name: &str) -> String {
        format!("No MCP server registered with name '{name}'")
    }

    /// The status of one server, as agenthicc's `status(name)`.
    pub fn status(&self, name: &str) -> Result<Value, String> {
        self.statuses
            .get(name)
            .map(ServerStatus::to_json)
            .ok_or_else(|| Self::missing(name))
    }

    /// The typed status of one server.
    pub fn server_status(&self, name: &str) -> Option<&ServerStatus> {
        self.statuses.get(name)
    }

    /// The published snapshot of one server.
    pub fn snapshot(&self, name: &str) -> Option<&CatalogSnapshot> {
        self.snapshots.get(name)
    }

    fn begin_generation(&mut self) -> u64 {
        self.generation_id += 1;
        self.generation_id
    }

    /// Start one server explicitly (agenthicc's `connect_server`).
    ///
    /// A start that fails is recorded in the status rather than returned:
    /// `Err` means only that no such server is configured.
    pub async fn connect_server(&mut self, name: &str) -> Result<(), String> {
        if !self.specs.contains_key(name) {
            return Err(Self::missing(name));
        }
        let generation = self.begin_generation();
        self.start_server(name, true, generation).await;
        Ok(())
    }

    async fn start_server(&mut self, name: &str, explicit: bool, generation: u64) {
        let Some(spec) = self.specs.get(name).cloned() else {
            return;
        };
        if self.registration_errors.contains_key(name) && spec.enabled() {
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
        if self.statuses[name].state == ServerState::Ready {
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

        match self.open_and_discover(&spec).await {
            Ok((transport, catalog)) => match self.publish(&spec, catalog) {
                Ok(snapshot) => {
                    let (count, revision) = (snapshot.tools.len(), snapshot.revision);
                    self.connections.insert(name.to_string(), transport);
                    self.update(name, |s| {
                        s.state = ServerState::Ready;
                        s.tool_count = count;
                        s.catalog_revision = revision;
                        s.last_success_at = Some(now());
                        s.last_error = None;
                        s.generation_id = generation;
                    });
                }
                Err(error) => {
                    transport.close().await;
                    self.record_failure(name, &spec, error, generation);
                }
            },
            Err(error) => self.record_failure(name, &spec, error, generation),
        }
    }

    fn record_failure(&mut self, name: &str, spec: &ServerSpec, error: String, generation: u64) {
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
    }

    fn update(&mut self, name: &str, change: impl FnOnce(&mut ServerStatus)) {
        if let Some(status) = self.statuses.get_mut(name) {
            change(status);
        }
    }

    /// Connect (with retries, inside the startup deadline) and list the
    /// catalog. Returns the open transport with what it discovered.
    async fn open_and_discover(
        &self,
        spec: &ServerSpec,
    ) -> Result<(Transport, Discovered), String> {
        let name = spec.name().to_string();
        if spec.transport() != "stdio" {
            if let Some(guard) = &self.guard {
                guard
                    .check(&spec.resolved_url())
                    .map_err(|e| format!("Failed to connect to MCP server '{name}': {e}"))?;
            }
        }
        let attempts = spec.reconnect_attempts() + 1;
        let deadline = self.startup_timeout;
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

    /// Validate and publish a discovered catalog (agenthicc's `_publish`).
    fn publish(
        &mut self,
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
        let revision = match self.snapshots.get(name) {
            Some(previous) if previous.catalog_hash == digest => previous.revision,
            _ => self.revision + 1,
        };
        self.revision = self.revision.max(revision);
        let filter_hash = sha256_hex(&pyjson::dumps(&json!([[], []])));
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
        self.snapshots.insert(name.to_string(), snapshot.clone());
        Ok(snapshot)
    }

    /// Refetch one server's catalog (agenthicc's `refresh_server`).
    ///
    /// `None` when the server is unknown, failed registration, or could not
    /// be started — agenthicc's `{"status": "unavailable"}` case.
    pub async fn refresh_server(&mut self, name: &str) -> Option<CatalogSnapshot> {
        if !self.specs.contains_key(name)
            || (self.registration_errors.contains_key(name) && self.specs[name].enabled())
        {
            return None;
        }
        let spec = self.specs[name].clone();
        if !self.connections.contains_key(name) {
            let generation = self.generation_id;
            self.start_server(name, true, generation).await;
            return self.snapshots.get(name).cloned();
        }
        let prior = self.snapshots.get(name).cloned();
        self.update(name, |s| s.state = ServerState::Refreshing);
        let deadline = self.startup_timeout;
        let outcome = {
            let transport = &self.connections[name];
            match list_tools(transport, deadline).await {
                Ok(tools) => Ok(tools),
                Err(error) => Err(with_stderr(error.to_string(), &transport.stderr_tail())),
            }
        };
        match outcome {
            Ok(tools) => {
                let hello = ServerHello {
                    protocol_version: prior
                        .as_ref()
                        .map(|p| p.protocol_version.clone())
                        .unwrap_or_default(),
                    server_info: prior
                        .as_ref()
                        .map(|p| p.server_info.clone())
                        .unwrap_or_default(),
                    capabilities: prior
                        .as_ref()
                        .map(|p| p.capabilities.clone())
                        .unwrap_or_default(),
                    instructions: prior
                        .as_ref()
                        .map(|p| p.instructions.clone())
                        .unwrap_or_default(),
                };
                let (prompts, resources) = {
                    let transport = &self.connections[name];
                    (
                        list_optional(transport, &hello, "prompts", deadline).await,
                        list_optional(transport, &hello, "resources", deadline).await,
                    )
                };
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
        &mut self,
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
    pub async fn doctor(&mut self, name: Option<&str>) -> Result<Vec<Value>, String> {
        let names: Vec<String> = match name {
            Some(name) => {
                if !self.specs.contains_key(name) {
                    return Err(Self::missing(name));
                }
                vec![name.to_string()]
            }
            None => self.specs.keys().cloned().collect(),
        };
        let mut results = Vec::new();
        for server in names {
            let spec = self.specs[&server].clone();
            let mut item = Map::new();
            item.insert("server".into(), json!(server));
            item.insert("config".into(), spec.redacted());
            let validation = self
                .registration_errors
                .get(&server)
                .cloned()
                .map_or_else(|| spec.validate(), Err);
            match validation {
                Ok(()) => {
                    if spec.enabled() && spec.auto_connect() {
                        let generation = self.generation_id;
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
            for invalid in &self.invalid_entries {
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

    /// Close every connection exactly once.
    pub async fn shutdown(&mut self) {
        let names: Vec<String> = self.connections.keys().cloned().collect();
        for name in names {
            self.update(&name, |s| s.state = ServerState::Stopping);
            if let Some(transport) = self.connections.remove(&name) {
                transport.close().await;
            }
            self.update(&name, |s| {
                s.state = ServerState::Stopped;
                s.tool_count = 0;
            });
        }
        self.snapshots.clear();
    }
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

    #[tokio::test]
    async fn a_missing_credential_is_needs_auth_without_a_connection_attempt() {
        let mut manager = McpSessionManager::new(
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
        let mut manager = McpSessionManager::new(
            vec![entry(
                "name = \"a\"\ncommand = \"/definitely/not/a/program\"\nreconnect_attempts = 1\nreconnect_delay_seconds = 0.0",
            )],
            Path::new("/"),
            None,
        );
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
    }

    #[tokio::test]
    async fn a_refused_host_is_never_dialled() {
        let mut manager = McpSessionManager::new(
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
        let mut manager = McpSessionManager::new(
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
}
