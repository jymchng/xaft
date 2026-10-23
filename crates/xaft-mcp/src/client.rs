//! The MCP handshake and catalog discovery.
//!
//! `initialize` → `notifications/initialized` → `tools/list` (following
//! `nextCursor`), plus `prompts/list` and `resources/list` when the server
//! advertises those capabilities — the calls agenthicc's
//! `McpSessionManager.start_server` makes through its bridge — and
//! `tools/call`, which the session's `mcp__<server>__<tool>` tools make.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::transport::{McpError, Transport};

/// The protocol version xaft asks for. A server may answer with an older one
/// it supports; that answer is recorded, not rejected.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// At most this many `tools/list` pages are followed.
const MAX_PAGES: usize = 100;

/// What the server said about itself in `initialize`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerHello {
    /// The negotiated protocol version.
    pub protocol_version: String,
    /// `serverInfo`.
    pub server_info: Map<String, Value>,
    /// `capabilities`.
    pub capabilities: Map<String, Value>,
    /// `instructions`, or empty.
    pub instructions: String,
}

/// Run the `initialize` handshake.
pub async fn initialize(transport: &Transport, timeout: Duration) -> Result<ServerHello, McpError> {
    let result = transport
        .request(
            "initialize",
            Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "xaft", "version": env!("CARGO_PKG_VERSION")},
            })),
            timeout,
        )
        .await?;
    let object = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let hello = ServerHello {
        protocol_version: result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        server_info: object("serverInfo"),
        capabilities: object("capabilities"),
        instructions: result
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
    };
    if hello.protocol_version.is_empty() {
        return Err(McpError::Transport(
            "MCP server did not report a protocol version".into(),
        ));
    }
    transport.set_protocol_version(&hello.protocol_version);
    transport.notify("notifications/initialized", None).await?;
    Ok(hello)
}

/// `tools/call`: invoke `name` with `arguments` and return the raw
/// `CallToolResult` (`content`, `structuredContent`, `isError`).
///
/// A tool that ran and failed is *not* an `Err`: the server reports it with
/// `isError: true` in an ordinary result. `Err` means the call itself did not
/// complete — the transport broke, the server answered with a JSON-RPC
/// error, or no answer came within `timeout`.
pub async fn call_tool(
    transport: &Transport,
    name: &str,
    arguments: Value,
    timeout: Duration,
) -> Result<Value, McpError> {
    transport
        .request(
            "tools/call",
            Some(json!({ "name": name, "arguments": arguments })),
            timeout,
        )
        .await
}

/// Every tool the server lists, following pagination.
pub async fn list_tools(transport: &Transport, timeout: Duration) -> Result<Vec<Value>, McpError> {
    list_all(transport, "tools/list", "tools", timeout).await
}

/// `prompts/list` or `resources/list`, if `capability` is advertised.
///
/// A server that advertises a capability and then fails the call is treated
/// as having none: the catalog is still useful without its prompts.
pub async fn list_optional(
    transport: &Transport,
    hello: &ServerHello,
    capability: &str,
    timeout: Duration,
) -> Vec<Value> {
    if !hello.capabilities.contains_key(capability) {
        return Vec::new();
    }
    let method = format!("{capability}/list");
    list_all(transport, &method, capability, timeout)
        .await
        .unwrap_or_default()
}

async fn list_all(
    transport: &Transport,
    method: &str,
    key: &str,
    timeout: Duration,
) -> Result<Vec<Value>, McpError> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
        let page = transport.request(method, params, timeout).await?;
        match page.get(key) {
            Some(Value::Array(list)) => items.extend(list.iter().cloned()),
            Some(_) => {
                return Err(McpError::Transport(format!(
                    "MCP server returned an invalid {key} list"
                )));
            }
            None => {}
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(items)
}
