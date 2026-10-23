//! Handler tests for `xaft mcp connect` / `refresh` / `doctor` (parity rows
//! 36, 40 and 41).
//!
//! These commands open real connections, so every test here runs a real MCP
//! server — no transport is mocked:
//!
//! * **stdio**: a small POSIX `sh` script speaking newline-delimited JSON-RPC,
//!   written into a temp dir. It answers `initialize`, `tools/list` (two
//!   pages, to prove `nextCursor` is followed) and `prompts/list`.
//! * **Streamable HTTP**, **legacy HTTP+SSE** and **WebSocket**: in-process
//!   servers bound to `127.0.0.1:0`, so nothing leaves the machine.
//!
//! Every test passes `--config` pointing at a temp file holding the
//! `[[mcp.client]]` stanzas and, for the HTTP tests, `[security]
//! sandbox_mode = false` (the loopback host would otherwise be refused by the
//! fail-closed network guard — which `remote_hosts_are_refused_by_default`
//! pins on its own).
//!
//! Output is asserted on the exact shape agenthicc prints:
//! `json.dumps(status, sort_keys=True)` for `connect`, the stable snapshot
//! dict for `refresh`, and the per-server report list for `doctor`.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use xaft_cli::args::{McpDoctorArgs, McpLiveArgs, McpSubcommand};
use xaft_cli::commands::mcp::render_mcp;
use xaft_runtime::ExitCode;

/// A stdio MCP server in POSIX sh.
const FAKE_SERVER: &str = r#"#!/bin/sh
echo "fake server starting" >&2
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  [ -z "$id" ] && continue
  case "$method" in
    initialize)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{},"prompts":{}},"serverInfo":{"name":"fake","version":"1.0"},"instructions":"Use carefully."}}\n' "$id" ;;
    tools/list)
      case "$line" in
        *'"cursor"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"add","inputSchema":{"type":"object","properties":{"a":{"type":"number"}}}}]}}\n' "$id" ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}],"nextCursor":"page-2"}}\n' "$id" ;;
      esac ;;
    prompts/list)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"prompts":[{"name":"greet"}]}}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
  esac
done
"#;

/// A stdio server that complains and dies before the handshake.
const DYING_SERVER: &str = "#!/bin/sh\necho 'fatal: database is locked' >&2\nexit 3\n";

struct Fixture {
    _dir: tempfile::TempDir,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        Self {
            _dir: dir,
            dir: path,
        }
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn config(&self, body: &str) -> PathBuf {
        let path = self.dir.join("xaft.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A config with one stdio server running [`FAKE_SERVER`].
    fn stdio(&self) -> PathBuf {
        let script = self.script("server.sh", FAKE_SERVER);
        self.config(&format!(
            "[[mcp.client]]\nname = \"fake\"\ncommand = \"{}\"\nreconnect_attempts = 0\n",
            script.display()
        ))
    }
}

async fn run(subcommand: McpSubcommand) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_mcp(&subcommand, &mut out).await.unwrap();
    (code, out)
}

fn connect(name: &str, config: &Path) -> McpSubcommand {
    McpSubcommand::Connect(McpLiveArgs {
        name: name.into(),
        config: Some(config.to_path_buf()),
    })
}

fn refresh(name: &str, config: &Path) -> McpSubcommand {
    McpSubcommand::Refresh(McpLiveArgs {
        name: name.into(),
        config: Some(config.to_path_buf()),
    })
}

fn doctor(name: Option<&str>, json: bool, config: &Path) -> McpSubcommand {
    McpSubcommand::Doctor(McpDoctorArgs {
        name: name.map(str::to_string),
        json,
        config: Some(config.to_path_buf()),
    })
}

fn parse(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"))
}

// ── connect ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn connect_prints_the_ready_status_in_pythons_compact_json() {
    let f = Fixture::new();
    let config = f.stdio();
    let (code, out) = run(connect("fake", &config)).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out.lines().count(), 1, "{out}");
    // `json.dumps(..., sort_keys=True)`: sorted keys, ", " and ": ".
    assert!(
        out.starts_with("{\"auth_state\": \"not_required\", \"catalog_revision\": 1, "),
        "{out}"
    );
    let status = parse(&out);
    assert_eq!(status["name"], "fake");
    assert_eq!(status["status"], "ready");
    assert_eq!(status["transport"], "stdio");
    assert_eq!(
        status["tool_count"], 2,
        "both pages of tools/list were read"
    );
    assert_eq!(status["generation_id"], 1);
    assert_eq!(status["last_error"], Value::Null);
    assert!(status["started_at"].as_f64().unwrap() > 0.0);
    assert!(status["last_success_at"].as_f64().unwrap() >= status["started_at"].as_f64().unwrap());
}

#[tokio::test]
async fn connect_to_an_unknown_server_prints_agenthiccs_error_line() {
    let f = Fixture::new();
    let config = f.stdio();
    let (code, out) = run(connect("ghost", &config)).await;
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "the mcp family exits 0 on error, like agenthicc"
    );
    assert_eq!(
        out,
        "error: MCP connect failed for 'ghost': No MCP server registered with name 'ghost'\n"
    );
}

#[tokio::test]
async fn a_server_that_dies_reports_failed_with_its_stderr() {
    let f = Fixture::new();
    let script = f.script("dying.sh", DYING_SERVER);
    let config = f.config(&format!(
        "[[mcp.client]]\nname = \"dying\"\ncommand = \"{}\"\nreconnect_attempts = 0\n",
        script.display()
    ));
    let (_, out) = run(connect("dying", &config)).await;
    let status = parse(&out);
    assert_eq!(status["status"], "failed");
    let error = status["last_error"].as_str().unwrap();
    assert!(
        error.starts_with("Failed to connect to MCP server 'dying' after 1 attempts:"),
        "{error}"
    );
    assert!(
        error.ends_with("; stderr: fatal: database is locked"),
        "{error}"
    );
}

#[tokio::test]
async fn a_missing_credential_is_needs_auth_and_nothing_is_spawned() {
    let f = Fixture::new();
    let marker = f.dir.join("spawned");
    let script = f.script(
        "marker.sh",
        &format!("#!/bin/sh\ntouch {}\n", marker.display()),
    );
    let config = f.config(&format!(
        "[[mcp.client]]\nname = \"gh\"\ncommand = \"{}\"\ntoken_env = \"XAFT_MCP_LIVE_TEST_UNSET_TOKEN\"\n",
        script.display()
    ));
    let (_, out) = run(connect("gh", &config)).await;
    let status = parse(&out);
    assert_eq!(status["status"], "needs_auth");
    assert_eq!(status["auth_state"], "needs_auth");
    assert!(
        !marker.exists(),
        "a server missing its credential must not be started"
    );
}

#[tokio::test]
async fn remote_hosts_are_refused_by_default() {
    // No [security] override: sandbox_mode is on and the allow-list is empty,
    // which xaft documents as "no outbound access".
    let f = Fixture::new();
    let config = f.config(
        "[[mcp.client]]\nname = \"remote\"\ntransport = \"http\"\nurl = \"https://mcp.example.com/mcp\"\n",
    );
    let (_, out) = run(connect("remote", &config)).await;
    let status = parse(&out);
    assert_eq!(status["status"], "failed");
    assert_eq!(
        status["last_error"],
        "Failed to connect to MCP server 'remote': Outbound request to 'mcp.example.com' is not on the network allow-list."
    );
}

// ── refresh ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn refresh_prints_the_stable_catalog_snapshot() {
    let f = Fixture::new();
    let config = f.stdio();
    let (code, out) = run(refresh("fake", &config)).await;
    assert_eq!(code, ExitCode::SUCCESS);
    let snapshot = parse(&out);
    let keys: Vec<&str> = snapshot
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "capabilities",
            "catalog_hash",
            "instructions",
            "prompts",
            "protocol_version",
            "resources",
            "revision",
            "server_info",
            "server_name",
            "tool_count",
            "tool_filter_hash",
            "tools"
        ]
    );
    assert_eq!(snapshot["server_name"], "fake");
    assert_eq!(snapshot["revision"], 1);
    assert_eq!(snapshot["protocol_version"], "2025-06-18");
    assert_eq!(snapshot["server_info"]["name"], "fake");
    assert_eq!(snapshot["instructions"], "Use carefully.");
    assert_eq!(snapshot["tool_count"], 2);
    // Sorted by name, with agenthicc's field names.
    assert_eq!(snapshot["tools"][0]["name"], "add");
    assert_eq!(snapshot["tools"][1]["description"], "Echo text");
    assert_eq!(
        snapshot["tools"][1]["input_schema"],
        json!({"type": "object"})
    );
    assert_eq!(snapshot["prompts"], json!([{"name": "greet"}]));
    assert_eq!(
        snapshot["resources"],
        json!([]),
        "resources capability not advertised"
    );
    assert_eq!(snapshot["catalog_hash"].as_str().unwrap().len(), 64);
    assert_eq!(snapshot["tool_filter_hash"].as_str().unwrap().len(), 16);
}

#[tokio::test]
async fn the_catalog_hash_is_stable_across_refreshes() {
    let f = Fixture::new();
    let config = f.stdio();
    let (_, first) = run(refresh("fake", &config)).await;
    let (_, second) = run(refresh("fake", &config)).await;
    assert_eq!(
        first, second,
        "an unchanged catalog must render byte-identically"
    );
}

#[tokio::test]
async fn refresh_of_an_unknown_or_failing_server_is_unavailable() {
    let f = Fixture::new();
    let config = f.stdio();
    assert_eq!(
        run(refresh("ghost", &config)).await.1,
        "{\"status\": \"unavailable\"}\n"
    );
    let script = f.script("dying.sh", DYING_SERVER);
    let failing = f.config(&format!(
        "[[mcp.client]]\nname = \"dying\"\ncommand = \"{}\"\nreconnect_attempts = 0\n",
        script.display()
    ));
    assert_eq!(
        run(refresh("dying", &failing)).await.1,
        "{\"status\": \"unavailable\"}\n"
    );
}

// ── doctor ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn doctor_reports_every_server_with_its_redacted_config() {
    let f = Fixture::new();
    let script = f.script("server.sh", FAKE_SERVER);
    let config = f.config(&format!(
        "[[mcp.client]]\nname = \"fake\"\ncommand = \"{script}\"\nreconnect_attempts = 0\n\n\
         [[mcp.client]]\nname = \"manual\"\ncommand = \"{script}\"\nauto_connect = false\n\n\
         [[mcp.client]]\nname = \"off\"\ncommand = \"{script}\"\nenabled = false\n\n\
         [[mcp.client]]\nname = \"broken\"\ntransport = \"pigeon\"\n",
        script = script.display()
    ));
    let (code, out) = run(doctor(None, false, &config)).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        out.starts_with("[\n  {\n    \"config\": {"),
        "indent=2, sorted: {out}"
    );
    let report = parse(&out);
    let by_name = |name: &str| {
        report
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["server"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {report}"))
            .clone()
    };
    let broken = by_name("broken");
    assert_eq!(broken["status"], "failed");
    assert_eq!(broken["error"], "unsupported MCP transport: 'pigeon'");
    assert_eq!(by_name("fake")["status"]["status"], "ready");
    assert_eq!(by_name("fake")["config"]["name"], "fake");
    assert_eq!(by_name("manual")["status"]["status"], "configured");
    assert_eq!(by_name("off")["status"]["status"], "disabled");
}

#[tokio::test]
async fn doctor_json_is_compact_and_a_name_narrows_it() {
    let f = Fixture::new();
    let config = f.stdio();
    let (_, out) = run(doctor(Some("fake"), true, &config)).await;
    assert_eq!(out.lines().count(), 1);
    assert!(out.starts_with("[{\"config\": {"), "{out}");
    assert_eq!(parse(&out).as_array().unwrap().len(), 1);

    let (_, missing) = run(doctor(Some("ghost"), true, &config)).await;
    assert_eq!(
        missing,
        "{\"error\": \"No MCP server registered with name 'ghost'\", \"status\": \"invalid\"}\n"
    );
}

#[tokio::test]
async fn doctor_with_no_servers_prints_an_empty_list() {
    let f = Fixture::new();
    let config = f.config("");
    assert_eq!(run(doctor(None, false, &config)).await.1, "[]\n");
}

// ── remote transports, against in-process servers ───────────────────────────

/// Answer one JSON-RPC request the way the stdio fixture does.
fn answer(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request["method"].as_str()? {
        "initialize" => json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "remote", "version": "2.0"},
        }),
        "tools/list" => json!({"tools": [{"name": "fetch", "inputSchema": {"type": "object"}}]}),
        "resources/list" => json!({"resources": [{"uri": "file:///a", "name": "a"}]}),
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "nope"}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Read one HTTP/1.1 request: (method, path, headers, body).
async fn read_request(
    stream: &mut tokio::io::BufReader<tokio::net::TcpStream>,
) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut line = String::new();
    if stream.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).await.ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_string());
            if name == "content-length" {
                length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some((method, path, headers, body))
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

/// A Streamable HTTP server. `sse` makes it answer requests as event streams.
async fn streamable_server(sse: bool, seen: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut stream = tokio::io::BufReader::new(socket);
                while let Some((method, _, headers, body)) = read_request(&mut stream).await {
                    let session = headers
                        .iter()
                        .find(|(n, _)| n == "mcp-session-id")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    let auth = headers
                        .iter()
                        .find(|(n, _)| n == "authorization")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    if method != "POST" {
                        let reply = response("405 Method Not Allowed", &[], "");
                        let _ = stream.get_mut().write_all(reply.as_bytes()).await;
                        continue;
                    }
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    seen.lock().await.push(format!(
                        "{} session={session} auth={auth}",
                        request["method"].as_str().unwrap_or("")
                    ));
                    let reply = match answer(&request) {
                        None => response("202 Accepted", &[], ""),
                        Some(answer) if sse => {
                            let body = format!("event: message\ndata: {answer}\n\n");
                            response(
                                "200 OK",
                                &[
                                    ("Content-Type", "text/event-stream"),
                                    ("Mcp-Session-Id", "s-42"),
                                ],
                                &body,
                            )
                        }
                        Some(answer) => response(
                            "200 OK",
                            &[
                                ("Content-Type", "application/json"),
                                ("Mcp-Session-Id", "s-42"),
                            ],
                            &answer.to_string(),
                        ),
                    };
                    let _ = stream.get_mut().write_all(reply.as_bytes()).await;
                }
            });
        }
    });
    format!("http://{address}/mcp")
}

fn remote_config(f: &Fixture, transport: &str, url: &str) -> PathBuf {
    f.config(&format!(
        "[security]\nsandbox_mode = false\n\n[[mcp.client]]\nname = \"remote\"\ntransport = \"{transport}\"\nurl = \"{url}\"\nreconnect_attempts = 0\ntoken = \"${{XAFT_MCP_LIVE_TEST_TOKEN}}\"\n"
    ))
}

#[tokio::test]
async fn streamable_http_with_json_replies_keeps_its_session_and_token() {
    // SAFETY: a variable name unique to this file, written before any reader.
    unsafe { std::env::set_var("XAFT_MCP_LIVE_TEST_TOKEN", "tok-123") };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = streamable_server(false, Arc::clone(&seen)).await;
    let f = Fixture::new();
    let config = remote_config(&f, "http", &url);
    let (_, out) = run(refresh("remote", &config)).await;
    let snapshot = parse(&out);
    assert_eq!(snapshot["protocol_version"], "2025-03-26");
    assert_eq!(snapshot["tools"][0]["name"], "fetch");
    assert_eq!(snapshot["resources"][0]["uri"], "file:///a");
    let seen = seen.lock().await.clone();
    assert_eq!(seen[0], "initialize session= auth=Bearer tok-123");
    assert!(
        seen[1..].iter().all(|line| line.contains("session=s-42")),
        "every request after initialize carries the session id: {seen:?}"
    );
}

#[tokio::test]
async fn streamable_http_with_event_stream_replies() {
    // SAFETY: as above.
    unsafe { std::env::set_var("XAFT_MCP_LIVE_TEST_TOKEN", "tok-123") };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = streamable_server(true, seen).await;
    let f = Fixture::new();
    let config = remote_config(&f, "streamable", &url);
    let status = parse(&run(connect("remote", &config)).await.1);
    assert_eq!(status["status"], "ready", "{status}");
    assert_eq!(status["transport"], "streamable_http");
    assert_eq!(status["auth_state"], "authenticated");
}

#[tokio::test]
async fn a_401_marks_the_server_needs_auth() {
    // SAFETY: as above.
    unsafe { std::env::set_var("XAFT_MCP_LIVE_TEST_TOKEN", "tok-123") };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let mut stream = tokio::io::BufReader::new(socket);
            if read_request(&mut stream).await.is_some() {
                let reply = response("401 Unauthorized", &[], "");
                let _ = stream.get_mut().write_all(reply.as_bytes()).await;
            }
        }
    });
    let f = Fixture::new();
    let config = remote_config(&f, "http", &format!("http://{address}/mcp"));
    let status = parse(&run(connect("remote", &config)).await.1);
    assert_eq!(status["status"], "needs_auth");
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("HTTP 401 Unauthorized")
    );
}

/// A legacy HTTP+SSE server: `GET /sse` streams, `POST /messages` enqueues.
async fn legacy_sse_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let events_rx = Arc::new(Mutex::new(Some(events_rx)));
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let events_tx = events_tx.clone();
            let events_rx = Arc::clone(&events_rx);
            tokio::spawn(async move {
                let mut stream = tokio::io::BufReader::new(socket);
                while let Some((method, path, _, body)) = read_request(&mut stream).await {
                    if method == "GET" && path == "/sse" {
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
                        let socket = stream.get_mut();
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket
                            .write_all(b"event: endpoint\ndata: /messages?session=1\n\n")
                            .await;
                        let mut rx = events_rx.lock().await.take().unwrap();
                        while let Some(event) = rx.recv().await {
                            if socket.write_all(event.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    if let Some(answer) = answer(&request) {
                        let _ = events_tx.send(format!("event: message\ndata: {answer}\n\n"));
                    }
                    let reply = response("202 Accepted", &[], "");
                    let _ = stream.get_mut().write_all(reply.as_bytes()).await;
                }
            });
        }
    });
    format!("http://{address}/sse")
}

#[tokio::test]
async fn the_legacy_sse_transport_follows_the_advertised_endpoint() {
    let url = legacy_sse_server().await;
    let f = Fixture::new();
    let config = remote_config(&f, "http+sse", &url);
    let snapshot = parse(&run(refresh("remote", &config)).await.1);
    assert_eq!(snapshot["server_info"]["name"], "remote", "{snapshot}");
    assert_eq!(snapshot["tools"][0]["name"], "fetch");
}

#[tokio::test]
async fn the_websocket_transport_round_trips() {
    use futures_util::{SinkExt as _, StreamExt as _};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(
                    socket,
                    |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        assert_eq!(
                            request.headers().get("Sec-WebSocket-Protocol").unwrap(),
                            "mcp"
                        );
                        response
                            .headers_mut()
                            .insert("Sec-WebSocket-Protocol", "mcp".parse().unwrap());
                        Ok(response)
                    },
                )
                .await
                else {
                    return;
                };
                while let Some(Ok(message)) = ws.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        if let Some(answer) = answer(&request) {
                            let _ = ws
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    answer.to_string(),
                                ))
                                .await;
                        }
                    }
                }
            });
        }
    });
    let f = Fixture::new();
    let config = remote_config(&f, "websocket", &format!("ws://{address}/"));
    let status = parse(&run(connect("remote", &config)).await.1);
    assert_eq!(status["status"], "ready", "{status}");
    assert_eq!(status["transport"], "ws");
    assert_eq!(status["tool_count"], 1);
}
