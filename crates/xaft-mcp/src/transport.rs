//! MCP client transports.
//!
//! The four transports agenthicc's bridge (via `lauren_mcp`) speaks:
//!
//! | transport | wire |
//! | --- | --- |
//! | `stdio` | newline-delimited JSON-RPC over a child process's stdin/stdout |
//! | `streamable_http` | one `POST` per message; the reply is JSON or an SSE stream (MCP 2025-03-26+) |
//! | `sse` | the legacy HTTP+SSE transport: a `GET` event stream plus `POST`s to the advertised endpoint |
//! | `ws` | one text frame per message, subprotocol `mcp` |
//!
//! All four share one request/response core ([`Core`]): requests get a numeric
//! id and a pending slot; whatever reads the wire routes each incoming message
//! into the core, which completes the matching slot. Server-initiated requests
//! are answered (`ping` with `{}`, anything else with JSON-RPC "method not
//! found"), so a server that pings its client mid-handshake is not left
//! waiting.
//!
//! A `stdio` child is spawned with `kill_on_drop`, in the workspace root,
//! without a shell; its stderr is kept as a bounded tail for diagnostics, the
//! same `stderr: …` suffix agenthicc appends to a failed start.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::spec::ServerSpec;

/// How much of a stdio server's stderr is kept for error messages.
const STDERR_TAIL_BYTES: usize = 4096;

/// A transport-level failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    /// The transport could not be opened, or broke.
    Transport(String),
    /// The server answered with a JSON-RPC error.
    Rpc {
        /// JSON-RPC error code.
        code: i64,
        /// Server-supplied message.
        message: String,
    },
    /// No answer arrived in time.
    Timeout(String),
    /// The connection is closed.
    Closed(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) | Self::Closed(message) => f.write_str(message),
            Self::Rpc { code, message } => write!(f, "MCP error {code}: {message}"),
            Self::Timeout(what) => write!(f, "timed out waiting for {what}"),
        }
    }
}

impl std::error::Error for McpError {}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, McpError>>>;

/// The request/response bookkeeping every transport shares.
#[derive(Default)]
struct Core {
    pending: std::sync::Mutex<Pending>,
    next_id: AtomicU64,
    closed: AtomicBool,
    /// Where server notifications (`notifications/tools/list_changed`, …)
    /// are reported, by method name. `None` drops them, as before.
    listener: std::sync::Mutex<Option<mpsc::UnboundedSender<String>>>,
}

impl Core {
    fn register(&self) -> Result<(u64, oneshot::Receiver<Result<Value, McpError>>), McpError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(McpError::Closed("MCP connection is closed".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id, tx);
        Ok((id, rx))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn is_pending(&self, id: u64) -> bool {
        self.lock().contains_key(&id)
    }

    fn forget(&self, id: u64) {
        self.lock().remove(&id);
    }

    /// Route one incoming message; return the replies it requires.
    fn route(&self, message: Value) -> Vec<Value> {
        match message {
            Value::Array(items) => items.into_iter().flat_map(|m| self.route(m)).collect(),
            Value::Object(map) => {
                let method = map.get("method").and_then(Value::as_str);
                let id = map.get("id").filter(|id| !id.is_null());
                match (method, id) {
                    (None, Some(id)) => {
                        if let Some(sender) = id.as_u64().and_then(|key| self.lock().remove(&key)) {
                            let outcome = match map.get("error") {
                                Some(error) => Err(McpError::Rpc {
                                    code: error
                                        .get("code")
                                        .and_then(Value::as_i64)
                                        .unwrap_or(-32603),
                                    message: error
                                        .get("message")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                }),
                                None => Ok(map.get("result").cloned().unwrap_or(Value::Null)),
                            };
                            let _ = sender.send(outcome);
                        }
                        Vec::new()
                    }
                    (Some(method), None) => {
                        // A notification: report it, never answer it.
                        if let Some(listener) = self
                            .listener
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_ref()
                        {
                            let _ = listener.send(method.to_string());
                        }
                        Vec::new()
                    }
                    (Some(method), Some(id)) => {
                        let reply = if method == "ping" {
                            json!({"jsonrpc": "2.0", "id": id, "result": {}})
                        } else {
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {"code": -32601, "message": format!("Method not found: {method}")},
                            })
                        };
                        vec![reply]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// Close the core and fail every waiting request with `reason`.
    fn close(&self, reason: &str) {
        self.closed.store(true, Ordering::Release);
        self.listener
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let drained: Vec<_> = self.lock().drain().collect();
        for (_, sender) in drained {
            let _ = sender.send(Err(McpError::Closed(reason.to_string())));
        }
    }
}

/// An incremental Server-Sent Events parser.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    /// Feed bytes; return every `(event, data)` completed by them.
    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(position) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            let mut line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    let event = std::mem::take(&mut self.event);
                    let data = std::mem::take(&mut self.data).join("\n");
                    events.push((
                        if event.is_empty() {
                            "message".into()
                        } else {
                            event
                        },
                        data,
                    ));
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (
                    field.to_string(),
                    value.strip_prefix(' ').unwrap_or(value).to_string(),
                ),
                None => (line.clone(), String::new()),
            };
            match field.as_str() {
                "event" => self.event = value,
                "data" => self.data.push(value),
                _ => {}
            }
        }
        events
    }
}

/// An open connection to one MCP server.
pub struct Transport {
    core: Arc<Core>,
    kind: Kind,
}

enum Kind {
    Stdio(StdioLink),
    Http(HttpLink),
    Sse(SseLink),
    Ws(WsLink),
}

struct StdioLink {
    outbound: mpsc::UnboundedSender<String>,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    stderr: Arc<std::sync::Mutex<String>>,
    tasks: Vec<JoinHandle<()>>,
}

struct HttpLink {
    client: reqwest::Client,
    url: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    session_id: std::sync::Mutex<Option<String>>,
    protocol_version: std::sync::Mutex<Option<String>>,
}

struct SseLink {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    reader: JoinHandle<()>,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct WsLink {
    outbound: mpsc::UnboundedSender<String>,
    tasks: Vec<JoinHandle<()>>,
}

fn header_map(spec: &ServerSpec) -> Result<reqwest::header::HeaderMap, McpError> {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in spec.resolved_headers() {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| McpError::Transport(format!("invalid MCP header name: '{name}'")))?;
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| McpError::Transport(format!("invalid value for MCP header '{name}'")))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, McpError> {
    reqwest::Client::builder()
        .connect_timeout(timeout)
        .build()
        .map_err(|e| McpError::Transport(format!("could not build HTTP client: {e}")))
}

fn http_status_error(status: reqwest::StatusCode) -> McpError {
    McpError::Transport(format!(
        "HTTP {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    ))
}

impl Transport {
    /// Open the transport `spec` names. Nothing is sent yet.
    pub async fn open(
        spec: &ServerSpec,
        workspace_root: &Path,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let core = Arc::new(Core::default());
        let kind = match spec.transport().as_str() {
            "stdio" => Kind::Stdio(open_stdio(spec, workspace_root, Arc::clone(&core))?),
            "streamable_http" => Kind::Http(HttpLink {
                client: http_client(timeout)?,
                url: parse_url(&spec.resolved_url())?,
                headers: header_map(spec)?,
                session_id: std::sync::Mutex::new(None),
                protocol_version: std::sync::Mutex::new(None),
            }),
            "sse" => Kind::Sse(open_sse(spec, Arc::clone(&core), timeout).await?),
            "ws" => Kind::Ws(open_ws(spec, Arc::clone(&core), timeout).await?),
            other => {
                return Err(McpError::Transport(format!(
                    "unsupported MCP transport: '{other}'"
                )));
            }
        };
        Ok(Self { core, kind })
    }

    /// Send a request and wait up to `timeout` for its result.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let (id, receiver) = self.core.register()?;
        let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        let exchange = async {
            self.send(&message, Some(id)).await?;
            receiver
                .await
                .map_err(|_| McpError::Closed("MCP connection closed before replying".into()))?
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(outcome) => {
                self.core.forget(id);
                outcome
            }
            Err(_) => {
                self.core.forget(id);
                Err(McpError::Timeout(format!("'{method}'")))
            }
        }
    }

    /// Send a notification.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.send(&message, None).await
    }

    /// Report every server notification's method to `listener`.
    ///
    /// The session manager listens for `notifications/tools/list_changed`
    /// and refreshes the server's catalog — agenthicc's bridge installs the
    /// same callback (`on_list_changed`).
    pub fn set_notification_listener(&self, listener: mpsc::UnboundedSender<String>) {
        *self.core.listener.lock().unwrap_or_else(|p| p.into_inner()) = Some(listener);
    }

    /// Whether the connection has closed: the stdio process exited, the
    /// stream ended, or [`Transport::close`] ran. A closed transport fails
    /// every request; the session manager reconnects it.
    pub fn is_closed(&self) -> bool {
        self.core.closed.load(Ordering::Acquire)
    }

    /// Record the negotiated protocol version (sent as a header over HTTP).
    pub fn set_protocol_version(&self, version: &str) {
        if let Kind::Http(link) = &self.kind {
            *link
                .protocol_version
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(version.to_string());
        }
    }

    /// The last few KiB a stdio server wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        match &self.kind {
            Kind::Stdio(link) => link
                .stderr
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            _ => String::new(),
        }
    }

    /// Close the connection: kill a stdio child, end an HTTP session.
    pub async fn close(&self) {
        self.core.close("MCP connection closed");
        match &self.kind {
            Kind::Stdio(link) => {
                if let Some(mut child) = link.child.lock().await.take() {
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                }
                for task in &link.tasks {
                    task.abort();
                }
            }
            Kind::Http(link) => {
                let session = link
                    .session_id
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(session) = session {
                    let request = link
                        .client
                        .delete(link.url.clone())
                        .headers(link.headers.clone())
                        .header("Mcp-Session-Id", session);
                    let _ = tokio::time::timeout(Duration::from_secs(2), request.send()).await;
                }
            }
            Kind::Sse(link) => link.reader.abort(),
            Kind::Ws(link) => {
                for task in &link.tasks {
                    task.abort();
                }
            }
        }
    }

    async fn send(&self, message: &Value, awaiting: Option<u64>) -> Result<(), McpError> {
        let text = serde_json::to_string(message)
            .map_err(|e| McpError::Transport(format!("could not encode message: {e}")))?;
        match &self.kind {
            Kind::Stdio(link) => link
                .outbound
                .send(text)
                .map_err(|_| McpError::Closed("MCP server process has exited".into())),
            Kind::Ws(link) => link
                .outbound
                .send(text)
                .map_err(|_| McpError::Closed("MCP WebSocket is closed".into())),
            Kind::Sse(link) => post_json(&link.client, &link.endpoint, &link.headers, text, &[])
                .await
                .map(|_| ()),
            Kind::Http(link) => self.send_http(link, text, awaiting).await,
        }
    }

    async fn send_http(
        &self,
        link: &HttpLink,
        text: String,
        awaiting: Option<u64>,
    ) -> Result<(), McpError> {
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some(session) = link
            .session_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            extra.push(("Mcp-Session-Id", session));
        }
        if let Some(version) = link
            .protocol_version
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            extra.push(("MCP-Protocol-Version", version));
        }
        let mut response = post_json(&link.client, &link.url, &link.headers, text, &extra).await?;
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *link.session_id.lock().unwrap_or_else(|p| p.into_inner()) = Some(session.to_string());
        }
        let Some(id) = awaiting else {
            return Ok(());
        };
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.starts_with("text/event-stream") {
            let mut parser = SseParser::default();
            while self.core.is_pending(id) {
                let chunk = response
                    .chunk()
                    .await
                    .map_err(|e| McpError::Transport(format!("HTTP stream failed: {e}")))?;
                let Some(chunk) = chunk else {
                    break;
                };
                for (_, data) in parser.feed(&chunk) {
                    if let Ok(message) = serde_json::from_str::<Value>(&data) {
                        for reply in self.core.route(message) {
                            let text = reply.to_string();
                            let _ = post_json(&link.client, &link.url, &link.headers, text, &extra)
                                .await;
                        }
                    }
                }
            }
            return Ok(());
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| McpError::Transport(format!("HTTP response failed: {e}")))?;
        let message: Value = serde_json::from_slice(&body).map_err(|_| {
            McpError::Transport(format!(
                "MCP server returned a non-JSON response ({content_type})"
            ))
        })?;
        self.core.route(message);
        Ok(())
    }
}

fn parse_url(url: &str) -> Result<reqwest::Url, McpError> {
    reqwest::Url::parse(url).map_err(|_| McpError::Transport(format!("invalid MCP URL: {url}")))
}

async fn post_json(
    client: &reqwest::Client,
    url: &reqwest::Url,
    headers: &reqwest::header::HeaderMap,
    body: String,
    extra: &[(&str, String)],
) -> Result<reqwest::Response, McpError> {
    let mut request = client
        .post(url.clone())
        .headers(headers.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(body);
    for (name, value) in extra {
        request = request.header(*name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| McpError::Transport(format!("HTTP request failed: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status));
    }
    Ok(response)
}

fn open_stdio(
    spec: &ServerSpec,
    workspace_root: &Path,
    core: Arc<Core>,
) -> Result<StdioLink, McpError> {
    let argv = spec.resolved_command();
    let Some((program, args)) = argv.split_first() else {
        return Err(McpError::Transport(format!(
            "MCP stdio server '{}' has no command",
            spec.name()
        )));
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| McpError::Transport(format!("failed to start '{program}': {e}")))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (outbound, mut queue) = mpsc::unbounded_channel::<String>();
    let mut tasks = Vec::new();

    if let Some(mut stdin) = stdin {
        let writer_core = Arc::clone(&core);
        tasks.push(tokio::spawn(async move {
            while let Some(line) = queue.recv().await {
                let written = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if written.await.is_err() {
                    writer_core.close("MCP server process has exited");
                    break;
                }
            }
        }));
    }

    if let Some(stdout) = stdout {
        let reader_core = Arc::clone(&core);
        let replies = outbound.clone();
        tasks.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // Servers sometimes log to stdout; a line that is not JSON is
                // noise, not a protocol error.
                if let Ok(message) = serde_json::from_str::<Value>(line) {
                    for reply in reader_core.route(message) {
                        let _ = replies.send(reply.to_string());
                    }
                }
            }
            reader_core.close("MCP server closed its output");
        }));
    }

    let tail = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(stderr) = stderr {
        let sink = Arc::clone(&tail);
        tasks.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = sink.lock().unwrap_or_else(|p| p.into_inner());
                if !tail.is_empty() {
                    tail.push('\n');
                }
                tail.push_str(&line);
                if tail.len() > STDERR_TAIL_BYTES {
                    let mut cut = tail.len() - STDERR_TAIL_BYTES;
                    while !tail.is_char_boundary(cut) {
                        cut += 1;
                    }
                    tail.drain(..cut);
                }
            }
        }));
    }

    Ok(StdioLink {
        outbound,
        child: tokio::sync::Mutex::new(Some(child)),
        stderr: tail,
        tasks,
    })
}

async fn open_sse(
    spec: &ServerSpec,
    core: Arc<Core>,
    timeout: Duration,
) -> Result<SseLink, McpError> {
    let client = http_client(timeout)?;
    let url = parse_url(&spec.resolved_url())?;
    let headers = header_map(spec)?;
    let response = tokio::time::timeout(
        timeout,
        client
            .get(url.clone())
            .headers(headers.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send(),
    )
    .await
    .map_err(|_| McpError::Timeout("the SSE stream to open".into()))?
    .map_err(|e| McpError::Transport(format!("HTTP request failed: {e}")))?;
    if !response.status().is_success() {
        return Err(http_status_error(response.status()));
    }

    let (endpoint_tx, endpoint_rx) = oneshot::channel::<String>();
    let reader_core = Arc::clone(&core);
    let reply_client = client.clone();
    let reply_headers = headers.clone();
    let base = url.clone();
    let reader = tokio::spawn(async move {
        let mut response = response;
        let mut parser = SseParser::default();
        let mut endpoint_tx = Some(endpoint_tx);
        let mut endpoint: Option<reqwest::Url> = None;
        while let Ok(Some(chunk)) = response.chunk().await {
            for (event, data) in parser.feed(&chunk) {
                if event == "endpoint" {
                    endpoint = base.join(data.trim()).ok();
                    if let Some(tx) = endpoint_tx.take() {
                        let _ = tx.send(data.trim().to_string());
                    }
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                for reply in reader_core.route(message) {
                    if let Some(endpoint) = &endpoint {
                        let _ = post_json(
                            &reply_client,
                            endpoint,
                            &reply_headers,
                            reply.to_string(),
                            &[],
                        )
                        .await;
                    }
                }
            }
        }
        reader_core.close("MCP SSE stream ended");
    });

    let advertised = match tokio::time::timeout(timeout, endpoint_rx).await {
        Ok(Ok(endpoint)) => endpoint,
        Ok(Err(_)) => {
            reader.abort();
            return Err(McpError::Transport(
                "MCP SSE stream ended before advertising an endpoint".into(),
            ));
        }
        Err(_) => {
            reader.abort();
            return Err(McpError::Timeout("the MCP SSE endpoint event".into()));
        }
    };
    let endpoint = url.join(&advertised).map_err(|_| {
        McpError::Transport(format!("MCP SSE endpoint is not a valid URL: {advertised}"))
    })?;
    // The MCP SDKs refuse an endpoint on another origin: following one would
    // send this server's credentials to a host the user never configured.
    if endpoint.origin() != url.origin() {
        reader.abort();
        return Err(McpError::Transport(format!(
            "MCP SSE endpoint origin does not match the connection origin: {endpoint}"
        )));
    }
    Ok(SseLink {
        client,
        endpoint,
        headers,
        reader,
    })
}

async fn open_ws(
    spec: &ServerSpec,
    core: Arc<Core>,
    timeout: Duration,
) -> Result<WsLink, McpError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    let url = spec.resolved_url();
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| McpError::Transport(format!("invalid MCP WebSocket URL: {e}")))?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mcp"));
    for (name, value) in spec.resolved_headers() {
        let name = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| McpError::Transport(format!("invalid MCP header name: '{name}'")))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| McpError::Transport(format!("invalid value for MCP header '{name}'")))?;
        request.headers_mut().insert(name, value);
    }
    let (stream, _) = tokio::time::timeout(timeout, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| McpError::Timeout("the WebSocket handshake".into()))?
        .map_err(|e| McpError::Transport(format!("WebSocket connection failed: {e}")))?;
    let (mut sink, mut source) = WsStream::split(stream);
    let (outbound, mut queue) = mpsc::unbounded_channel::<String>();
    let writer_core = Arc::clone(&core);
    let writer = tokio::spawn(async move {
        while let Some(text) = queue.recv().await {
            if sink
                .send(tokio_tungstenite::tungstenite::Message::Text(text))
                .await
                .is_err()
            {
                writer_core.close("MCP WebSocket is closed");
                break;
            }
        }
        let _ = sink.close().await;
    });
    let reader_core = Arc::clone(&core);
    let replies = outbound.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(message)) = source.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    for reply in reader_core.route(value) {
                        let _ = replies.send(reply.to_string());
                    }
                }
            }
        }
        reader_core.close("MCP WebSocket closed");
    });
    Ok(WsLink {
        outbound,
        tasks: vec![writer, reader],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_assemble_across_chunks_and_lines() {
        let mut parser = SseParser::default();
        assert!(parser.feed(b"event: endpoint\nda").is_empty());
        let events =
            parser.feed(b"ta: /messages?id=1\r\n\r\n: comment\ndata: {\"a\":\ndata: 1}\n\n");
        assert_eq!(
            events,
            vec![
                ("endpoint".to_string(), "/messages?id=1".to_string()),
                ("message".to_string(), "{\"a\":\n1}".to_string()),
            ]
        );
    }

    #[test]
    fn responses_complete_their_pending_request_and_errors_carry_the_code() {
        let core = Core::default();
        let (id, mut rx) = core.register().unwrap();
        assert!(
            core.route(json!({"jsonrpc": "2.0", "id": id, "result": {"ok": 1}}))
                .is_empty()
        );
        assert_eq!(rx.try_recv().unwrap().unwrap(), json!({"ok": 1}));

        let (id, mut rx) = core.register().unwrap();
        core.route(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "nope"}}),
        );
        assert_eq!(
            rx.try_recv().unwrap().unwrap_err(),
            McpError::Rpc {
                code: -32601,
                message: "nope".into()
            }
        );
    }

    #[test]
    fn server_requests_are_answered() {
        let core = Core::default();
        let replies = core.route(json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
        assert_eq!(
            replies,
            vec![json!({"jsonrpc": "2.0", "id": 7, "result": {}})]
        );
        let replies = core.route(json!({"jsonrpc": "2.0", "id": "x", "method": "roots/list"}));
        assert_eq!(replies[0]["error"]["code"], -32601);
        assert!(
            core.route(json!({"jsonrpc": "2.0", "method": "notifications/progress"}))
                .is_empty()
        );
    }

    #[test]
    fn notifications_reach_the_listener_by_method_name() {
        let core = Core::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        *core.listener.lock().unwrap() = Some(tx);
        assert!(
            core.route(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}))
                .is_empty(),
            "a notification is never answered"
        );
        assert_eq!(rx.try_recv().unwrap(), "notifications/tools/list_changed");
        core.close("gone");
        assert!(rx.try_recv().is_err(), "closing drops the listener");
    }

    #[test]
    fn closing_fails_every_waiter_and_refuses_new_requests() {
        let core = Core::default();
        let (_, mut rx) = core.register().unwrap();
        core.close("gone");
        assert_eq!(
            rx.try_recv().unwrap().unwrap_err(),
            McpError::Closed("gone".into())
        );
        assert!(core.register().is_err());
    }
}
