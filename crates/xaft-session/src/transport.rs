//! Local authenticated HTTP/SSE attachment transport for the session service.
//!
//! This is xaft's port of agenthicc's `session_service/transport.py`
//! (`LocalSessionServer`), reduced to the routes that make sense without
//! agenthicc's background-inbox machinery. It is what `xaft session serve`
//! (parity matrix row 13) runs.
//!
//! # Why a hand-rolled HTTP/1.1 server
//!
//! xaft has no HTTP dependency, and adding one — or enabling `agtrs-runtime`'s
//! optional `axum` feature — is an authorization decision that had not been
//! answered when this module was written. The transport is therefore built on
//! [`tokio::net::TcpListener`], which the workspace already has through
//! `tokio = { features = ["full"] }`: **zero new crates**, at the cost of
//! owning the request parsing.
//!
//! It speaks the subset of HTTP/1.1 the adapter needs — a request line,
//! headers, an optional `Content-Length` body, and `Connection: close`
//! framing — and is deliberately *not* a general-purpose server. Requests are
//! capped at [`MAX_REQUEST_BYTES`] and every connection serves exactly one
//! request.
//!
//! # Security properties (each pinned by a test)
//!
//! * Loopback by default: [`LocalSessionServer::new`] **refuses** a
//!   non-loopback `host` that has no `auth_token`. See
//!   [`TransportError::NonLoopbackWithoutToken`].
//! * When a token is configured, **every** route — including `/health` —
//!   rejects a request whose `Authorization` header is not exactly
//!   `Bearer <token>` with a `401` and agenthicc's error envelope.
//! * The transport never reads the configuration, so it cannot echo a secret
//!   out of it. Everything it serialises comes from the service's own read
//!   model ([`SessionSnapshot::to_value`], [`SessionEvent::to_value`],
//!   [`CommandResult::to_value`]), which the service core documents as carrying
//!   no provider credential. A test plants the auth token and asserts it never
//!   appears in a served payload.
//!
//! # Divergence from agenthicc's `/stream`
//!
//! agenthicc's `_stream` iterates a `SessionSubscription` — a push
//! subscription the in-process service hands out. xaft's service core has no
//! `subscribe()`: it is durable-first, and the durable event log *is* the
//! source of truth (see the [`crate::service`] module docs). This route
//! therefore **polls** [`SessionService::events`] with the caller's cursor
//! every [`POLL_INTERVAL`]. That is a deliberate semantic difference from
//! push, and it is also a capability: because the log is durable, the stream
//! can carry events appended by a *different* process, which an in-process
//! broadcast could not. The frame format (`id:`, `event: session`, `data:`)
//! matches agenthicc's byte for byte.
//!
//! Not ported, and why: agenthicc also serves `/v1/inbox*` (a background
//! interaction inbox) and enforces `cors_origins`. xaft has no background
//! inbox, and its `cors_origins` is always empty, so `OPTIONS` follows
//! agenthicc's *empty-set* behaviour — `204` with no `Origin`, `403` with one.
//!
//! # Where the bytes differ from agenthicc, on purpose
//!
//! * `/health` reports `"service": "xaft-session"`. The field, its type, and
//!   the rest of the document are agenthicc's; only the product name is xaft's,
//!   because a server should name itself.
//! * The client-id header is `X-Xaft-Client`, and `X-Agenthicc-Client` is
//!   accepted as a fallback so an agenthicc client needs no xaft branch. Both
//!   names are advertised in the `OPTIONS` preflight.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::service::{
    CapabilitySet, CommandResult, SessionCommand, SessionEvent, SessionService,
    SessionServiceError, SessionSnapshot,
};

/// The largest request — head plus body — the transport will buffer.
///
/// agenthicc sets `client_max_size=1_048_576` on its `aiohttp` application;
/// this is the same number, applied to the raw byte stream rather than to a
/// parsed body.
pub const MAX_REQUEST_BYTES: usize = 1_048_576;

/// How often the `/stream` route polls the durable event log.
///
/// See the module docs: xaft's stream is a durable-log poll, not a push
/// subscription. 200 ms keeps a local client's latency imperceptible without
/// turning the poll into a busy loop.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Whether `host` names a loopback interface.
///
/// The same three names agenthicc's `_loopback` accepts. Anything else — a
/// LAN address, `0.0.0.0`, a hostname — is treated as non-loopback, so the
/// loopback check fails closed.
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Errors the transport itself raises, before or outside a request.
///
/// Request-scoped failures are [`SessionServiceError`] and become the JSON
/// error envelope; these two are process-level and are what makes
/// `xaft session serve` exit non-zero instead of half-starting.
#[derive(Debug, Error)]
pub enum TransportError {
    /// A non-loopback `host` was requested with no auth token.
    ///
    /// Refusing here rather than binding is the whole point of the check: a
    /// server reachable off-host that serves whole session histories to
    /// anything that can open a socket is a data-exposure bug, and it cannot
    /// be fixed after the fact.
    #[error(
        "non-loopback session servers require an auth token: \
         refusing to bind {0} without --auth-token"
    )]
    NonLoopbackWithoutToken(String),

    /// The listening socket could not be bound.
    #[error("could not bind {host}:{port}: {source}")]
    Bind {
        /// Requested host.
        host: String,
        /// Requested port.
        port: u16,
        /// The bind failure.
        source: std::io::Error,
    },
}

/// The state every spawned connection task shares.
#[derive(Debug)]
struct ServerState {
    service: SessionService,
    auth_token: Option<String>,
}

/// A loopback-first HTTP/SSE adapter over [`SessionService`].
///
/// Construct it with [`LocalSessionServer::new`], bring it up with
/// [`LocalSessionServer::start`], and shut it down with
/// [`LocalSessionServer::stop`]. The server never stops on its own, which is
/// what makes `xaft session serve` a foreground process.
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use xaft_session::{LocalSessionServer, SessionService};
///
/// let service = SessionService::open(std::path::Path::new("/tmp/xaft-demo")).await?;
/// let mut server = LocalSessionServer::new(service, "127.0.0.1", 0, None)?;
/// let url = server.start().await?;   // "http://127.0.0.1:54321"
/// println!("{url}");
/// server.stop().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct LocalSessionServer {
    state: Arc<ServerState>,
    host: String,
    port: u16,
    bound: Option<SocketAddr>,
    shutdown: Option<watch::Sender<bool>>,
    accept_task: Option<JoinHandle<()>>,
}

impl LocalSessionServer {
    /// Build a server over `service`.
    ///
    /// Returns [`TransportError::NonLoopbackWithoutToken`] when `host` is not
    /// loopback and `auth_token` is `None` or empty. An empty token is
    /// normalised to `None` (agenthicc does the same: `auth_token or None`),
    /// so `--auth-token ""` cannot mean "authenticate against the empty
    /// string".
    pub fn new(
        service: SessionService,
        host: &str,
        port: u16,
        auth_token: Option<String>,
    ) -> Result<Self, TransportError> {
        let auth_token = auth_token.filter(|t| !t.is_empty());
        if !is_loopback_host(host) && auth_token.is_none() {
            return Err(TransportError::NonLoopbackWithoutToken(host.to_string()));
        }
        Ok(Self {
            state: Arc::new(ServerState {
                service,
                auth_token,
            }),
            host: host.to_string(),
            port,
            bound: None,
            shutdown: None,
            accept_task: None,
        })
    }

    /// Bind, start accepting, and return the base URL.
    ///
    /// `port` `0` binds a free port, and the returned URL carries the port
    /// that was actually bound — which is why the URL, not the requested port,
    /// is what a caller must use.
    pub async fn start(&mut self) -> Result<String, TransportError> {
        if self.accept_task.is_some() {
            return Ok(self.url());
        }
        let listener = TcpListener::bind((self.host.as_str(), self.port))
            .await
            .map_err(|source| TransportError::Bind {
                host: self.host.clone(),
                port: self.port,
                source,
            })?;
        let bound = listener
            .local_addr()
            .map_err(|source| TransportError::Bind {
                host: self.host.clone(),
                port: self.port,
                source,
            })?;
        self.bound = Some(bound);

        let (tx, rx) = watch::channel(false);
        self.shutdown = Some(tx);
        let state = Arc::clone(&self.state);
        self.accept_task = Some(tokio::spawn(accept_loop(listener, state, rx)));
        Ok(self.url())
    }

    /// The base URL, e.g. `http://127.0.0.1:54321`.
    ///
    /// An IPv6 literal is bracketed, which agenthicc does not do; without the
    /// brackets `http://::1:8080` is not parseable.
    pub fn url(&self) -> String {
        let port = self.bound.map(|a| a.port()).unwrap_or(self.port);
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{host}:{port}")
    }

    /// The address actually bound, once [`LocalSessionServer::start`] has run.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.bound
    }

    /// Stop accepting and release the listening port.
    ///
    /// Idempotent, and safe to call when [`LocalSessionServer::start`] was
    /// never called or failed. Dropping the listener is what releases the
    /// port; the accept task is aborted as well so it cannot linger.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(true);
        }
        if let Some(task) = self.accept_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.bound = None;
    }
}

// ── Accept loop ────────────────────────────────────────────────────────────

/// Accept connections until the shutdown signal fires.
async fn accept_loop(
    listener: TcpListener,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        let state = Arc::clone(&state);
                        let shutdown = shutdown.clone();
                        tokio::spawn(async move {
                            serve_connection(state, stream, shutdown).await;
                        });
                    }
                    // A failed accept is not fatal (a client may have gone
                    // away between the poll and the accept); the next loop
                    // iteration re-arms it.
                    Err(_) => continue,
                }
            }
        }
    }
}

/// Serve exactly one request on `stream`, then close it.
async fn serve_connection(
    state: Arc<ServerState>,
    mut stream: TcpStream,
    shutdown: watch::Receiver<bool>,
) {
    let request = match read_request(&mut stream).await {
        Ok(Some(request)) => request,
        _ => return,
    };

    // CORS preflight is answered before authentication, matching agenthicc,
    // whose `_options` handler is registered without a capability check.
    if request.method == "OPTIONS" {
        let origin = request.headers.get("origin").cloned().unwrap_or_default();
        // agenthicc's `cors_origins` is empty in xaft (there is no setting for
        // it), so any real Origin is refused and a bare preflight is allowed.
        if !origin.is_empty() {
            let _ = write_response(&mut stream, 403, "", &[], b"").await;
            return;
        }
        // A bare preflight gets agenthicc's `_options` headers, so a browser
        // client's preflight succeeds against either server. The allowed-header
        // list names both client-id headers, because both are honoured.
        let extra = [
            ("Access-Control-Allow-Origin", origin.as_str()),
            (
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type, X-Xaft-Client, X-Agenthicc-Client",
            ),
            ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        ];
        let _ = write_response(&mut stream, 204, "", &extra, b"").await;
        return;
    }

    match route(&state, &request).await {
        Ok(Outcome::Json { status, body }) => {
            let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"null".to_vec());
            let _ = write_response(&mut stream, status, "application/json", &[], &bytes).await;
        }
        Ok(Outcome::Stream {
            session_id,
            after,
            capabilities,
            initial,
        }) => {
            let _ = write_stream(
                &state,
                &mut stream,
                &session_id,
                after,
                &capabilities,
                initial,
                shutdown,
            )
            .await;
        }
        Err(error) => {
            let body = error_body(&error);
            let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"null".to_vec());
            let _ =
                write_response(&mut stream, error.status, "application/json", &[], &bytes).await;
        }
    }
}

// ── Routing ────────────────────────────────────────────────────────────────

/// What a routed request resolved to.
enum Outcome {
    /// A single JSON document.
    Json {
        status: u16,
        body: serde_json::Value,
    },
    /// An open SSE stream, already validated so the session exists.
    Stream {
        session_id: String,
        after: u64,
        capabilities: CapabilitySet,
        initial: Vec<SessionEvent>,
    },
}

/// Authenticate, then dispatch on method and path.
///
/// The order matters and mirrors agenthicc: what a caller may *do* is decided
/// by the capability set the request authenticated with, and the operation
/// itself (`create` needs `control`) is then checked again by the service.
/// The transport grants the full CLI set only to an authenticated caller.
async fn route(state: &ServerState, request: &Request) -> Result<Outcome, SessionServiceError> {
    let capabilities = authenticate(state, request)?;
    let segments = path_segments(&request.path);

    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["health"]) => Ok(Outcome::Json {
            status: 200,
            body: serde_json::json!({
                "ok": true,
                "service": "xaft-session",
                "schema_version": 1,
            }),
        }),

        ("POST", ["v1", "sessions"]) => {
            let data = parse_object_body(request)?;
            let project_root = data
                .get("project_root")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            let agent = named_object(data.get("agent"));
            let workflow = named_object(data.get("workflow"));
            // `parent_session_id` is accepted by agenthicc's transport and
            // dropped here: xaft's service core does not model a parent at
            // creation time (`fork` is a command kind instead). Documented
            // rather than silently ignored.
            let snapshot = state
                .service
                .create_session(
                    Path::new(project_root),
                    &client_id(request),
                    &capabilities,
                    agent,
                    workflow,
                )
                .await?;
            Ok(Outcome::Json {
                status: 201,
                body: serde_json::json!({
                    "ok": true,
                    "snapshot": snapshot.to_value(),
                }),
            })
        }

        ("GET", ["v1", "sessions", session_id]) => {
            let snapshot = state.service.snapshot(session_id, &capabilities).await?;
            Ok(Outcome::Json {
                status: 200,
                body: serde_json::json!({
                    "ok": true,
                    "snapshot": snapshot.to_value(),
                }),
            })
        }

        ("GET", ["v1", "sessions", session_id, "events"]) => {
            let after = parse_cursor(request)?;
            let events = state
                .service
                .events(session_id, after, &capabilities)
                .await?;
            Ok(Outcome::Json {
                status: 200,
                body: serde_json::json!({
                    "ok": true,
                    "events": events.iter().map(SessionEvent::to_value).collect::<Vec<_>>(),
                }),
            })
        }

        ("GET", ["v1", "sessions", session_id, "stream"]) => {
            let after = parse_cursor(request)?;
            // Fetched once here so a bad id is a JSON 404 *before* the SSE
            // headers commit the response to a 200.
            let initial = state
                .service
                .events(session_id, after, &capabilities)
                .await?;
            Ok(Outcome::Stream {
                session_id: session_id.to_string(),
                after,
                capabilities,
                initial,
            })
        }

        ("POST", ["v1", "sessions", session_id, "commands"]) => {
            let command = build_command(request, session_id, capabilities)?;
            let result = state.service.submit(&command).await?;
            Ok(Outcome::Json {
                status: 200,
                body: result.to_value(),
            })
        }

        _ => Err(SessionServiceError {
            code: "not_found".to_string(),
            message: format!("no such route: {} {}", request.method, request.path),
            status: 404,
        }),
    }
}

/// Authenticate a request and return the capability set it carries.
///
/// With no configured token the caller is granted the CLI set — that is
/// agenthicc's behaviour, and it is only reachable on a loopback bind because
/// [`LocalSessionServer::new`] refuses the alternative.
fn authenticate(
    state: &ServerState,
    request: &Request,
) -> Result<CapabilitySet, SessionServiceError> {
    if let Some(expected) = &state.auth_token {
        let supplied = request
            .headers
            .get("authorization")
            .map(String::as_str)
            .unwrap_or_default();
        if supplied != format!("Bearer {expected}") {
            return Err(SessionServiceError {
                code: "unauthorized".to_string(),
                message: "valid bearer token required".to_string(),
                status: 401,
            });
        }
    }
    Ok(CapabilitySet::cli())
}

/// The client-id header, defaulting to `http-client` and capped at 128 chars.
///
/// agenthicc reads `X-Agenthicc-Client`. xaft's own name is tried first, but
/// agenthicc's is accepted as a fallback for the same reason the readiness line
/// keeps agenthicc's `schedule_service` key: a client written against agenthicc
/// should not need a xaft branch to be identified. Nothing else in the protocol
/// depends on the header's name.
///
/// A present-but-empty header falls back to `http-client` instead of
/// propagating the empty string. agenthicc's `[:128]` slice would keep the
/// empty value; an empty client id is not a client id.
fn client_id(request: &Request) -> String {
    ["x-xaft-client", "x-agenthicc-client"]
        .iter()
        .find_map(|name| request.headers.get(*name))
        .map(|value| value.chars().take(128).collect::<String>())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http-client".to_string())
}

/// Parse the `?after=` cursor, defaulting to `0`.
fn parse_cursor(request: &Request) -> Result<u64, SessionServiceError> {
    match request.query.get("after") {
        None => Ok(0),
        Some(raw) => raw.trim().parse::<u64>().map_err(|_| SessionServiceError {
            code: "invalid_request".to_string(),
            message: format!("after must be a non-negative integer, got {raw:?}"),
            status: 400,
        }),
    }
}

/// Decode a JSON object body, rejecting anything that is not one.
fn parse_object_body(
    request: &Request,
) -> Result<BTreeMap<String, serde_json::Value>, SessionServiceError> {
    if request.body.is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&request.body).map_err(|error| SessionServiceError {
            code: "invalid_request".to_string(),
            message: format!("request body must be a JSON object: {error}"),
            status: 400,
        })?;
    match value {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => Err(SessionServiceError {
            code: "invalid_request".to_string(),
            message: "request body must be a JSON object".to_string(),
            status: 400,
        }),
    }
}

/// Build the command envelope for `POST /v1/sessions/{id}/commands`.
///
/// The capability set is the one the *request* authenticated with, not one the
/// body claims — the envelope is what authorisation reads, so letting a body
/// widen it would make the token check decorative.
fn build_command(
    request: &Request,
    session_id: &str,
    capabilities: CapabilitySet,
) -> Result<SessionCommand, SessionServiceError> {
    let data = parse_object_body(request)?;
    let kind = data
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .filter(|kind| !kind.is_empty())
        .ok_or_else(|| SessionServiceError::invalid_command("kind is required"))?
        .to_string();

    let command_id = data
        .get("command_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("cmd_{}", uuid::Uuid::new_v4().simple()));

    let idempotency_key = data
        .get("idempotency_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| command_id.clone());

    let expected_sequence = match data.get("expected_sequence") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            SessionServiceError::invalid_command("expected_sequence must be an integer")
        })?),
    };

    let payload = match data.get("payload") {
        None | Some(serde_json::Value::Null) => BTreeMap::new(),
        Some(serde_json::Value::Object(map)) => {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        Some(_) => {
            return Err(SessionServiceError::invalid_command(
                "payload must be a JSON object",
            ));
        }
    };

    Ok(SessionCommand {
        kind,
        session_id: Some(session_id.to_string()),
        client_id: client_id(request),
        command_id,
        idempotency_key,
        expected_sequence,
        payload,
        capabilities,
    })
}

/// `{"name": {...}}` from an optional body field, or an empty map.
fn named_object(value: Option<&serde_json::Value>) -> BTreeMap<String, serde_json::Value> {
    match value {
        Some(serde_json::Value::Object(map)) => {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        _ => BTreeMap::new(),
    }
}

/// The JSON error envelope: `{"ok": false, "error": {code, message, status}}`.
fn error_body(error: &SessionServiceError) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": serde_json::to_value(error).unwrap_or(serde_json::Value::Null),
    })
}

/// Split a path into its non-empty segments.
///
/// No percent-decoding: every session id the service mints is
/// `sess_<lowercase hex>`, so the only inputs that would need it are ones that
/// name nothing. Documented rather than half-implemented.
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

// ── SSE ────────────────────────────────────────────────────────────────────

/// Stream durable events as Server-Sent Events until the client goes away.
///
/// See the module docs for why this polls rather than subscribing. The error
/// path cannot reach the client as a status code — the 200 has already been
/// written — so a session that disappears mid-stream simply ends it.
async fn write_stream(
    state: &ServerState,
    stream: &mut TcpStream,
    session_id: &str,
    after: u64,
    capabilities: &CapabilitySet,
    initial: Vec<SessionEvent>,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    // No `Content-Length` and no chunked encoding: the body ends when the
    // connection closes, which is what `Connection: close` declares.
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache\r\n\
                Connection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;

    let mut cursor = after;
    let mut pending = initial;
    loop {
        for event in pending.drain(..) {
            let data =
                serde_json::to_string(&event.to_value()).unwrap_or_else(|_| "null".to_string());
            let frame = format!("id: {}\nevent: session\ndata: {}\n\n", event.sequence, data);
            stream.write_all(frame.as_bytes()).await?;
            cursor = event.sequence;
        }
        stream.flush().await?;

        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }

        match state.service.events(session_id, cursor, capabilities).await {
            Ok(events) => pending = events,
            // The session was deleted, or the store went away. End the stream
            // rather than emit a misleading frame.
            Err(_) => return Ok(()),
        }
    }
}

// ── HTTP/1.1 plumbing ──────────────────────────────────────────────────────

/// One parsed request.
#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    /// Header names lower-cased; values trimmed.
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// Read one request, or `Ok(None)` when the client sent nothing usable.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    let head_end = loop {
        if let Some(position) = find_bytes(&buf, b"\r\n\r\n") {
            break position + 4;
        }
        if buf.len() >= MAX_REQUEST_BYTES {
            return Ok(None);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() || !target.starts_with('/') {
        return Ok(None);
    }

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_query(query)),
        None => (target, BTreeMap::new()),
    };

    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let declared = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if declared > MAX_REQUEST_BYTES {
        return Ok(None);
    }

    let mut body = buf[head_end..].to_vec();
    while body.len() < declared {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(declared);

    Ok(Some(Request {
        method,
        path,
        query,
        headers,
        body,
    }))
}

/// Write a complete response with `Content-Length` framing.
async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
    if !content_type.is_empty() {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // A 204 has no body, and RFC 7230 §3.3.2 forbids framing one with
    // `Content-Length`; `aiohttp` omits it too. Every other status this
    // transport emits is `Content-Length`-framed.
    if status != 204 {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");

    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Split a raw query string into a map. Values are not percent-decoded.
fn parse_query(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// First index of `needle` in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The reason phrase for the handful of statuses this transport emits.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    }
}

/// Unused today, but named so the read model is part of this module's contract.
#[allow(dead_code)]
fn snapshot_document(snapshot: &SessionSnapshot) -> serde_json::Value {
    snapshot.to_value()
}

/// Unused today, but named so the receipt type is part of this module's contract.
#[allow(dead_code)]
fn result_document(result: &CommandResult) -> serde_json::Value {
    result.to_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream as ClientStream;

    /// A service over a throwaway directory.
    async fn service(dir: &std::path::Path) -> SessionService {
        SessionService::open(dir).await.expect("open service")
    }

    /// Ask the OS for a free port by binding and immediately dropping.
    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    }

    /// Send one raw request and return `(status_line, body)`.
    async fn request(addr: SocketAddr, raw: &str) -> (String, String) {
        let mut stream = ClientStream::connect(addr).await.expect("connect");
        stream
            .write_all(raw.as_bytes())
            .await
            .expect("write request");
        stream.flush().await.expect("flush");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let text = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = match text.split_once("\r\n\r\n") {
            Some((head, body)) => (head, body),
            None => (text.as_str(), ""),
        };
        let status = head.lines().next().unwrap_or_default().to_string();
        (status, body.to_string())
    }

    /// Send one raw request and return the whole response, head included.
    ///
    /// [`request`] returns only the body; a test that asserts on response
    /// headers — the preflight, for one — needs the head as well.
    async fn request_text(addr: SocketAddr, raw: &str) -> String {
        let mut stream = ClientStream::connect(addr).await.expect("connect");
        stream
            .write_all(raw.as_bytes())
            .await
            .expect("write request");
        stream.flush().await.expect("flush");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Extract the `data: ` payload of the first SSE frame.
    fn first_sse_data(text: &str) -> Option<String> {
        let marker = "data: ";
        let start = text.find(marker)? + marker.len();
        let end = text[start..].find('\n').map(|n| start + n)?;
        Some(text[start..end].to_string())
    }

    #[tokio::test]
    async fn a_non_loopback_host_needs_a_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = LocalSessionServer::new(service(dir.path()).await, "0.0.0.0", 0, None)
            .expect_err("0.0.0.0 without a token must be refused");
        assert!(
            matches!(error, TransportError::NonLoopbackWithoutToken(ref host) if host == "0.0.0.0"),
            "unexpected error: {error}"
        );
        // The same host WITH a token is allowed, so the check is about the
        // token and not a blanket refusal of non-loopback names.
        assert!(
            LocalSessionServer::new(service(dir.path()).await, "0.0.0.0", 0, Some("t".into()))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_empty_token_is_normalised_to_no_token_and_still_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = LocalSessionServer::new(
            service(dir.path()).await,
            "192.168.1.5",
            0,
            Some(String::new()),
        )
        .expect_err("an empty token must not count as a token");
        assert!(matches!(error, TransportError::NonLoopbackWithoutToken(_)));
    }

    #[tokio::test]
    async fn every_loopback_name_is_accepted_without_a_token() {
        for host in ["127.0.0.1", "localhost", "::1"] {
            assert!(is_loopback_host(host), "{host} should be loopback");
            let dir = tempfile::tempdir().expect("tempdir");
            assert!(
                LocalSessionServer::new(service(dir.path()).await, host, 0, None).is_ok(),
                "{host} should bind without a token"
            );
        }
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("example.com"));
    }

    #[tokio::test]
    async fn health_reports_the_service_identity_and_a_bound_port() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        let url = server.start().await.expect("start");
        assert!(url.starts_with("http://127.0.0.1:"), "url was {url}");
        let addr = server.local_addr().expect("bound");
        assert_ne!(addr.port(), 0, "port 0 must resolve to a real port");

        let (status, body) = request(addr, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(status.contains("200"), "status was {status}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(value["service"], serde_json::json!("xaft-session"));
        assert_eq!(value["schema_version"], serde_json::json!(1));

        server.stop().await;
    }

    #[tokio::test]
    async fn a_token_is_required_on_every_route_including_health() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server = LocalSessionServer::new(
            service(dir.path()).await,
            "127.0.0.1",
            0,
            Some("s3cret".into()),
        )
        .unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        let (status, body) = request(addr, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(
            status.contains("401"),
            "unauthenticated status was {status}"
        );
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["ok"], serde_json::json!(false));
        assert_eq!(value["error"]["code"], serde_json::json!("unauthorized"));
        assert!(
            !body.contains("s3cret"),
            "the token must never be echoed: {body}"
        );

        let authorized = "GET /health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer s3cret\r\n\r\n";
        let (status, body) = request(addr, authorized).await;
        assert!(status.contains("200"), "authorized status was {status}");
        assert!(
            !body.contains("s3cret"),
            "a served payload must not carry the token: {body}"
        );

        // A wrong token is refused too — this is not a presence check.
        let wrong = "GET /health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer nope\r\n\r\n";
        let (status, _) = request(addr, wrong).await;
        assert!(status.contains("401"), "wrong-token status was {status}");

        server.stop().await;
    }

    #[tokio::test]
    async fn create_snapshot_events_and_command_round_trip_over_http() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        // The length is computed, not counted: a hand-counted
        // `Content-Length` is a test that passes or fails on arithmetic rather
        // than on the transport, and this one was wrong by four bytes.
        let create_body = r#"{"project_root": ".", "agent": {"na": 1}}"#;
        let create = format!(
            "POST /v1/sessions HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{create_body}",
            create_body.len()
        );
        let (status, body) = request(addr, &create).await;
        assert!(status.contains("201"), "create status was {status}\n{body}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["ok"], serde_json::json!(true));
        let session_id = value["snapshot"]["session_id"]
            .as_str()
            .expect("session_id")
            .to_string();
        assert_eq!(
            value["snapshot"]["last_event_sequence"],
            serde_json::json!(1)
        );

        let snapshot = format!("GET /v1/sessions/{session_id} HTTP/1.1\r\nHost: x\r\n\r\n");
        let (status, body) = request(addr, &snapshot).await;
        assert!(status.contains("200"), "snapshot status was {status}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            value["snapshot"]["session_id"],
            serde_json::json!(session_id)
        );

        let events =
            format!("GET /v1/sessions/{session_id}/events?after=0 HTTP/1.1\r\nHost: x\r\n\r\n");
        let (status, body) = request(addr, &events).await;
        assert!(status.contains("200"), "events status was {status}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        let list = value["events"].as_array().expect("events array");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["sequence"], serde_json::json!(1));
        assert_eq!(list[0]["kind"], serde_json::json!("session_created"));

        // `--after` past the end is an empty replay, not an error.
        let after =
            format!("GET /v1/sessions/{session_id}/events?after=99 HTTP/1.1\r\nHost: x\r\n\r\n");
        let (_, body) = request(addr, &after).await;
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["events"].as_array().expect("array").len(), 0);

        let command_body = r#"{"kind": "submit_message", "payload": {"text": "hello"}}"#;
        let command = format!(
            "POST /v1/sessions/{session_id}/commands HTTP/1.1\r\nHost: x\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{command_body}",
            command_body.len()
        );
        let (status, body) = request(addr, &command).await;
        assert!(
            status.contains("200"),
            "command status was {status}\n{body}"
        );
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(value["data"]["accepted"], serde_json::json!(true));
        assert!(
            value["data"]["turn_id"]
                .as_str()
                .expect("turn_id")
                .starts_with("turn_"),
            "a queued turn must come back with its id: {body}"
        );

        // A command the service refuses keeps the service's own status and
        // vocabulary in the envelope. This is the half a 200-with-`ok: false`
        // or a 500 would break: an out-of-process client has to be able to tell
        // "you asked for something invalid" from "the server broke".
        let refused_body = r#"{"kind": "submit_message", "payload": {"text": "   "}}"#;
        let refused = format!(
            "POST /v1/sessions/{session_id}/commands HTTP/1.1\r\nHost: x\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{refused_body}",
            refused_body.len()
        );
        let (status, body) = request(addr, &refused).await;
        assert!(status.contains("400"), "status was {status}\n{body}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["ok"], serde_json::json!(false));
        assert_eq!(value["error"]["code"], serde_json::json!("invalid_command"));
        assert!(
            value["error"]["message"]
                .as_str()
                .expect("message")
                .contains("non-empty text"),
            "the service's own message must survive the transport: {body}"
        );

        server.stop().await;
    }

    #[tokio::test]
    async fn an_unknown_session_is_a_404_and_an_unknown_route_is_a_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        let (status, body) = request(
            addr,
            "GET /v1/sessions/sess_deadbeef HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(status.contains("404"), "status was {status}");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["error"]["code"], serde_json::json!("not_found"));

        let (status, _) = request(addr, "GET /nope HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(status.contains("404"), "status was {status}");

        // A malformed cursor is a 400, not a silent zero.
        let (status, _) = request(
            addr,
            "GET /v1/sessions/sess_x/events?after=abc HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(status.contains("400"), "status was {status}");

        server.stop().await;
    }

    #[tokio::test]
    async fn the_stream_route_frames_events_without_upgrading_a_bad_id_to_200() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        // A typo'd id must be a JSON 404, never an SSE 200 that never emits.
        let (status, body) = request(
            addr,
            "GET /v1/sessions/sess_missing/stream HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(status.contains("404"), "status was {status}");
        assert!(body.contains("not_found"), "body was {body}");

        // A real session streams its creation event with agenthicc's framing.
        let create = "POST /v1/sessions HTTP/1.1\r\nHost: x\r\n\
                      Content-Length: 2\r\n\r\n{}";
        let (_, body) = request(addr, create).await;
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        let session_id = value["snapshot"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let mut stream = ClientStream::connect(addr).await.expect("connect");
        let raw =
            format!("GET /v1/sessions/{session_id}/stream?after=0 HTTP/1.1\r\nHost: x\r\n\r\n");
        stream.write_all(raw.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");

        // Read until the first frame arrives, then stop. The server keeps the
        // connection open by design, so we must not wait for EOF.
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        let text = loop {
            let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .expect("stream must emit a frame promptly")
                .expect("read");
            if read == 0 {
                break String::from_utf8_lossy(&buffer).into_owned();
            }
            buffer.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&buffer).into_owned();
            if text.contains("\n\n") {
                break text;
            }
        };
        drop(stream);

        assert!(
            text.starts_with("HTTP/1.1 200 OK"),
            "stream head was {:?}",
            text.lines().next()
        );
        assert!(text.contains("Content-Type: text/event-stream"));
        assert!(text.contains("id: 1\n"), "frame was {text:?}");
        assert!(text.contains("event: session"), "frame was {text:?}");
        let data = first_sse_data(&text).expect("a data line");
        let event: serde_json::Value = serde_json::from_str(&data).expect("event json");
        assert_eq!(event["sequence"], serde_json::json!(1));

        server.stop().await;
    }

    /// A bare preflight is answered before authentication, and it must carry
    /// the same CORS headers agenthicc's `_options` sends, so a browser client's
    /// preflight succeeds against either server. A real `Origin` is refused
    /// because xaft's `cors_origins` is empty.
    #[tokio::test]
    async fn options_is_answered_without_a_token_and_refuses_a_real_origin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server = LocalSessionServer::new(
            service(dir.path()).await,
            "127.0.0.1",
            0,
            Some("preflight-token".into()),
        )
        .unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        let (status, _) = request(addr, "OPTIONS /v1/sessions HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(status.contains("204"), "bare preflight was {status}");

        let response = request_text(addr, "OPTIONS /v1/sessions HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(
            response.contains("Access-Control-Allow-Methods: GET, POST, OPTIONS"),
            "the preflight must advertise the methods agenthicc advertises: {response}"
        );
        assert!(
            response.contains(
                "Access-Control-Allow-Headers: \
                 Authorization, Content-Type, X-Xaft-Client, X-Agenthicc-Client"
            ),
            "both client-id headers must be advertised: {response}"
        );
        assert!(
            !response.contains("preflight-token"),
            "a preflight is answered without a token, so it must not leak one: {response}"
        );

        let with_origin = "OPTIONS /v1/sessions HTTP/1.1\r\nHost: x\r\nOrigin: http://evil\r\n\r\n";
        let (status, _) = request(addr, with_origin).await;
        assert!(
            status.contains("403"),
            "an unlisted origin must be refused, was {status}"
        );

        server.stop().await;
    }

    /// agenthicc's client-id header is honoured as a fallback, so a client
    /// written against agenthicc is identified without a xaft branch.
    #[tokio::test]
    async fn the_agenthicc_client_header_is_honoured_as_a_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        let body = "{}";
        let raw = format!(
            "POST /v1/sessions HTTP/1.1\r\nHost: x\r\nX-Agenthicc-Client: legacy\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, response) = request(addr, &raw).await;
        assert!(status.contains("201"), "status was {status}");
        let value: serde_json::Value = serde_json::from_str(&response).expect("json");
        let session_id = value["snapshot"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let events = format!("GET /v1/sessions/{session_id}/events HTTP/1.1\r\nHost: x\r\n\r\n");
        let (_, response) = request(addr, &events).await;
        assert!(
            response.contains("\"legacy\""),
            "the X-Agenthicc-Client header should reach the event payload: {response}"
        );

        server.stop().await;
    }

    #[tokio::test]
    async fn stop_releases_the_port_so_the_address_can_be_rebound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", port, None).unwrap();
        server.start().await.expect("start");
        assert_eq!(server.local_addr().expect("bound").port(), port);
        server.stop().await;

        // A second bind on the same port is the proof the listener is gone.
        let rebound = TcpListener::bind(("127.0.0.1", port)).await;
        assert!(rebound.is_ok(), "port {port} was not released: {rebound:?}");
        drop(rebound);
    }

    #[tokio::test]
    async fn the_request_parser_reads_query_headers_and_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        // The client id header reaches the snapshot via the creation event's
        // payload, which is the only place it is observable over HTTP.
        let body = "{}";
        let raw = format!(
            "POST /v1/sessions?ignored=1 HTTP/1.1\r\nHost: x\r\nX-Xaft-Client: probe\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, response) = request(addr, &raw).await;
        assert!(status.contains("201"), "status was {status}");
        let value: serde_json::Value = serde_json::from_str(&response).expect("json");
        let session_id = value["snapshot"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let events = format!("GET /v1/sessions/{session_id}/events HTTP/1.1\r\nHost: x\r\n\r\n");
        let (_, response) = request(addr, &events).await;
        assert!(
            response.contains("\"probe\""),
            "the X-Xaft-Client header should reach the event payload: {response}"
        );

        server.stop().await;
    }

    #[tokio::test]
    async fn a_body_that_is_not_an_object_is_rejected_before_it_reaches_the_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut server =
            LocalSessionServer::new(service(dir.path()).await, "127.0.0.1", 0, None).unwrap();
        server.start().await.expect("start");
        let addr = server.local_addr().expect("bound");

        let raw = "POST /v1/sessions HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n[]";
        let (status, body) = request(addr, raw).await;
        assert!(status.contains("400"), "status was {status}");
        assert!(body.contains("must be a JSON object"), "body was {body}");

        server.stop().await;
    }

    #[test]
    fn the_summary_helpers_round_trip_the_read_model() {
        let snapshot = SessionSnapshot::new("sess_test", "/tmp", &CapabilitySet::cli(), 0.0);
        assert_eq!(
            snapshot_document(&snapshot)["session_id"],
            serde_json::json!("sess_test")
        );
        let command = SessionCommand::new(
            "submit_message",
            "sess_test",
            "cli",
            BTreeMap::new(),
            CapabilitySet::cli(),
        );
        let result = CommandResult::ok(&command, BTreeMap::new());
        assert_eq!(result_document(&result)["ok"], serde_json::json!(true));
    }
}
