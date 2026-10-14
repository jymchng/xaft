# PRD 46 — `/mcp` Command: MCP Server Status

## Status: Draft
## Depends on: PRD 32a–32e

---

## 1. Problem

`/mcp` returns `"No MCP registry connected."`. There is currently no MCP
(Model Context Protocol) server integration in xaft, and no way for users
to inspect which external tool servers are connected or healthy.

This PRD defines both the minimal MCP registry abstraction needed to support
the status command and the `/mcp` command implementation itself. The full
MCP tool-call transport layer is out of scope (separate PRD).

---

## 2. Goals

1. `/mcp` — list all configured MCP servers with their connection status.
2. `/mcp <name>` — show detailed info for one server: tools it exposes,
   transport (stdio / HTTP), last ping latency.
3. `/mcp restart <name>` — reconnect a failed server.
4. Status refreshes within 2 seconds (async ping on each invocation).

### Example: `/mcp`

```
NAME          TRANSPORT   TOOLS   STATUS     LATENCY
──────────────────────────────────────────────────────
filesystem    stdio          12   connected    3 ms
github        http            7   connected   42 ms
database      stdio           4   error       —
```

---

## 3. Non-Goals

- Implementing MCP tool call routing (separate PRD).
- MCP server process management beyond restart.
- OAuth / authentication for HTTP MCP servers in v1.

---

## 4. Technical Design

### 4.1 `McpServerInfo` snapshot type

```rust
#[derive(Clone, Debug)]
pub struct McpServerInfo {
    pub name: String,
    pub transport: McpTransport,   // Stdio | Http(url)
    pub tools: Vec<String>,        // tool names exposed by server
    pub status: McpServerStatus,   // Connected | Connecting | Error(msg) | Disabled
    pub last_ping_ms: Option<u64>,
}

pub enum McpTransport { Stdio, Http(String) }
pub enum McpServerStatus { Connected, Connecting, Error(String), Disabled }
```

### 4.2 `McpRegistry` trait

```rust
#[async_trait]
pub trait McpRegistry: Send + Sync {
    async fn list(&self) -> Vec<McpServerInfo>;
    async fn get(&self, name: &str) -> Option<McpServerInfo>;
    async fn restart(&self, name: &str) -> Result<(), String>;
}
```

`CommandContext` gains:
```rust
pub mcp_registry: Option<Arc<dyn McpRegistry>>,
```

A `NullMcpRegistry` (returns empty list) is the default when no real MCP
servers are configured; it replaces the current hard-coded `"No MCP registry
connected."` message with an empty table + hint.

### 4.3 `McpHandler::execute_async`

```rust
async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let registry = match ctx.mcp_registry {
        Some(r) => r,
        None => return CommandResult::Lines(vec![
            "No MCP servers configured.".into(),
            "Add [mcp.<name>] sections to your xaft.toml.".into(),
        ]),
    };

    let arg = ctx.args.trim();
    if arg.is_empty() {
        // list all
        let servers = registry.list().await;
        if servers.is_empty() {
            return CommandResult::Lines(vec![
                "No MCP servers configured.".into(),
            ]);
        }
        let mut table = CliTable::new(["NAME", "TRANSPORT", "TOOLS", "STATUS", "LATENCY"]);
        for s in &servers {
            table.push_row([
                &s.name,
                &format!("{:?}", s.transport),
                &s.tools.len().to_string(),
                &format!("{:?}", s.status),
                &s.last_ping_ms.map(|ms| format!("{ms} ms"))
                   .unwrap_or_else(|| "—".into()),
            ]);
        }
        return CommandResult::StyledLines(table.render(ctx.terminal_cols));
    }

    if let Some(name) = arg.strip_prefix("restart ") {
        return match registry.restart(name).await {
            Ok(()) => CommandResult::Lines(vec![format!("Restarting {name}…")]),
            Err(e) => CommandResult::Error(e),
        };
    }

    // detail view
    match registry.get(arg).await {
        Some(s) => {
            let lines = vec![
                format!("Server:    {}", s.name),
                format!("Transport: {:?}", s.transport),
                format!("Status:    {:?}", s.status),
                format!("Latency:   {}", s.last_ping_ms.map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "—".into())),
                format!("Tools ({}):", s.tools.len()),
            ].into_iter()
             .chain(s.tools.iter().map(|t| format!("  • {t}")))
             .collect();
            CommandResult::Lines(lines)
        }
        None => CommandResult::Error(format!("Unknown MCP server: {arg}")),
    }
}
```

### 4.4 Config section for MCP servers

```toml
[mcp.filesystem]
transport = "stdio"
command   = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp.github]
transport = "http"
url       = "http://localhost:3000/mcp"
```

`XaftConfig` gains a `mcp: HashMap<String, McpServerConfig>` field
(new config key, default empty map).

---

## 5. Implementation Steps

1. Define `McpServerInfo`, `McpTransport`, `McpServerStatus`, `McpRegistry`
   trait in a new `crates/xaft-runtime/src/mcp.rs`.
2. Implement `NullMcpRegistry`.
3. Add `mcp: HashMap<String, McpServerConfig>` to `XaftConfig`.
4. Add `mcp_registry` field to `CommandContext`.
5. Implement `McpHandler::execute_async` (list / detail / restart paths).
6. Stub `LiveMcpRegistry` that reads config and pings each server (stdio
   process or HTTP GET).
7. Tests (see §6).

---

## 6. Tests

```
test mcp_handler_null_registry_prints_no_servers_hint
test mcp_handler_empty_server_list_prints_hint
test mcp_handler_list_shows_all_servers
test mcp_handler_detail_known_server
test mcp_handler_detail_unknown_server_returns_error
test mcp_handler_restart_known_server
test null_mcp_registry_returns_empty_list
```
