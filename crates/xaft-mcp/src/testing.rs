//! Stand-in MCP servers for tests — no network, no installed MCP SDK.
//!
//! * [`STDIO_SERVER`] is a POSIX `sh` MCP server speaking newline-delimited
//!   JSON-RPC on stdin/stdout. [`write_stdio_server`] writes it into a
//!   directory the test owns; the script keeps its small amount of state
//!   (`launches`, `grown`) next to itself, so a test can count relaunches.
//! * [`HttpStandIn`] is an in-process Streamable HTTP server bound to
//!   `127.0.0.1:0`: real HTTP, real JSON-RPC, inside the test process.
//!
//! Both serve the same catalog:
//!
//! | tool | does |
//! | --- | --- |
//! | `echo` | returns `echo: <text>` |
//! | `fail` | returns a result with `isError: true` (`something broke`) |
//! | `json` | returns the text `{"answer": 42}`, which a client decodes |
//! | `grow` *(stdio)* | adds the tool `extra` and sends `notifications/tools/list_changed` |
//! | `die` *(stdio)* | exits without answering, as a crashing server does |

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The stand-in stdio server. See the module docs for its tools.
pub const STDIO_SERVER: &str = r#"#!/bin/sh
# xaft stand-in MCP server: newline-delimited JSON-RPC over stdio.
dir=$(dirname "$0")
echo "launched" >> "$dir/launches"
echo "stand-in server starting" >&2
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  [ -z "$id" ] && continue
  case "$method" in
    initialize)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"stand-in","version":"1.0"},"instructions":"Prefer the echo tool for greetings."}}\n' "$id" ;;
    tools/list)
      extra=''
      [ -f "$dir/grown" ] && extra=',{"name":"extra","description":"Added by grow","inputSchema":{"type":"object"}}'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},{"name":"fail","description":"Always fails","inputSchema":{"type":"object"}},{"name":"json","description":"Answer in JSON","inputSchema":{"type":"object"}},{"name":"grow","description":"Add a tool","inputSchema":{"type":"object"}},{"name":"die","description":"Exit without answering","inputSchema":{"type":"object"}}%s]}}\n' "$id" "$extra" ;;
    tools/call)
      tool=$(printf '%s' "$line" | sed -n 's/.*"name":"\([a-z_]*\)".*/\1/p')
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      case "$tool" in
        echo)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: %s"}]}}\n' "$id" "$text" ;;
        fail)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"something broke"}]}}\n' "$id" ;;
        json)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"{\\"answer\\": 42}"}]}}\n' "$id" ;;
        grow)
          touch "$dir/grown"
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"grown"}]}}\n' "$id"
          printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n' ;;
        die)
          exit 0 ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"Unknown tool"}}\n' "$id" ;;
      esac ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
  esac
done
"#;

/// Write [`STDIO_SERVER`] into `dir` as an executable `server.sh` and return
/// its path.
#[cfg(unix)]
pub fn write_stdio_server(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("server.sh");
    std::fs::write(&path, STDIO_SERVER).expect("write the stand-in server");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in server executable");
    path
}

/// How many times the stdio stand-in in `dir` has been launched.
pub fn stdio_launches(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("launches"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// A `[[mcp.client]]` stanza that runs the stdio stand-in at `script`.
pub fn stdio_stanza(name: &str, script: &Path, reconnect_attempts: u32) -> String {
    format!(
        "[[mcp.client]]\nname = \"{name}\"\ncommand = \"{}\"\nreconnect_attempts = {reconnect_attempts}\nreconnect_delay_seconds = 0.0\n",
        script.display()
    )
}

/// An in-process Streamable HTTP MCP server.
pub struct HttpStandIn {
    url: String,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    task: JoinHandle<()>,
}

impl HttpStandIn {
    /// Bind `127.0.0.1:0` and serve until dropped.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the stand-in");
        let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let recorded = Arc::clone(&calls);
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let Some((method, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let (status, reply) = respond(&method, &body, &recorded);
                    let payload = reply.map(|r| r.to_string()).unwrap_or_default();
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nMcp-Session-Id: stand-in\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(payload.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { url, calls, task }
    }

    /// The endpoint, `http://127.0.0.1:<port>/mcp`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every `tools/call` so far: `(tool, arguments)`.
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A `[[mcp.client]]` stanza for this server.
    pub fn stanza(&self, name: &str) -> String {
        format!(
            "[[mcp.client]]\nname = \"{name}\"\ntransport = \"http\"\nurl = \"{}\"\nreconnect_attempts = 0\n",
            self.url
        )
    }
}

impl Drop for HttpStandIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Read one HTTP request: its method and body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let method = head.split_whitespace().next()?.to_string();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some((method, body))
}

/// Answer one request: `(status line, JSON body or none)`.
fn respond(
    method: &str,
    body: &[u8],
    calls: &Mutex<Vec<(String, Value)>>,
) -> (&'static str, Option<Value>) {
    if method != "POST" {
        return ("200 OK", None);
    }
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return ("400 Bad Request", None);
    };
    let Some(id) = message.get("id").cloned() else {
        // A notification.
        return ("202 Accepted", None);
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match message.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "http-stand-in", "version": "1.0"},
        }),
        "tools/list" => json!({"tools": [
            {"name": "echo", "description": "Echo text back",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
            {"name": "fail", "description": "Always fails", "inputSchema": {"type": "object"}},
            {"name": "json", "description": "Answer in JSON", "inputSchema": {"type": "object"}},
        ]}),
        "tools/call" => {
            let tool = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((tool.clone(), arguments.clone()));
            match tool.as_str() {
                "echo" => json!({"content": [{"type": "text", "text": format!(
                    "echo: {}",
                    arguments.get("text").and_then(Value::as_str).unwrap_or("")
                )}]}),
                "fail" => {
                    json!({"isError": true, "content": [{"type": "text", "text": "something broke"}]})
                }
                "json" => json!({"content": [{"type": "text", "text": "{\"answer\": 42}"}]}),
                _ => {
                    return (
                        "200 OK",
                        Some(json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": -32602, "message": "Unknown tool"}})),
                    );
                }
            }
        }
        other => {
            return (
                "200 OK",
                Some(json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("Method not found: {other}")}})),
            );
        }
    };
    (
        "200 OK",
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
    )
}
