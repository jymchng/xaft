cat > ./01_plugin_system.md << 'EOF'
# Plugin System

## Design Goals

The `xaft` plugin system allows teams to extend the tool registry, add domain-specific agents, and integrate with proprietary internal services without modifying the `xaft` core.

## Plugin Trait

```rust
// xaft-plugin/src/plugin.rs
#[async_trait]
pub trait XaftPlugin: Send + Sync {
    /// Unique plugin identifier (e.g., "my-company/db-tools")
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn description(&self) -> &str;

    /// Called once at session startup — register tools, agents, hooks
    async fn initialize(&self, registry: &mut PluginRegistry) -> Result<(), PluginError>;

    /// Called once at session teardown
    async fn shutdown(&self) -> Result<(), PluginError>;

    /// Optional: validate configuration before use
    fn validate_config(&self, config: &toml::Value) -> Result<(), PluginError> { Ok(()) }
}

pub struct PluginRegistry {
    tools: Vec<(String, Arc<ErasedTool>)>,
    agents: Vec<Arc<dyn Agent + Send + Sync>>,
    hooks: Vec<Arc<dyn ToolHook>>,
    signal_handlers: Vec<Box<dyn FnMut(&dyn Any) + Send>>,
}

impl PluginRegistry {
    pub fn register_tool(&mut self, name: &str, tool: Arc<ErasedTool>) {
        self.tools.push((name.to_string(), tool));
    }

    pub fn register_global_hook(&mut self, hook: Arc<dyn ToolHook>) {
        self.hooks.push(hook);
    }

    pub fn register_agent(&mut self, agent: Arc<dyn Agent + Send + Sync>) {
        self.agents.push(agent);
    }
}
```

## Plugin Loading

Plugins load in order:
1. Built-in plugins (always loaded)
2. User plugins from `~/.config/xaft/plugins/`
3. Project plugins from `.xaft/plugins/`

```toml
# .xaft/config.toml
[[plugins]]
id = "my-company/db-tools"
path = ".xaft/plugins/db-tools"    # local plugin
config = { connection_string = "postgres://localhost/mydb" }

[[plugins]]
id = "xaft-github"
version = "^1.0"                    # from plugin registry (future)
config = { token_env = "GITHUB_TOKEN" }
```

## Native Rust Plugin (dylib)

For maximum performance, plugins compile to a shared library:

```rust
// my_plugin/src/lib.rs
use xaft_plugin::prelude::*;

pub struct MyDbPlugin {
    pool: sqlx::PgPool,
}

#[async_trait]
impl XaftPlugin for MyDbPlugin {
    fn id(&self) -> &str { "my-company/db-tools" }
    fn version(&self) -> &str { "1.0.0" }
    fn description(&self) -> &str { "Database inspection tools for PostgreSQL" }

    async fn initialize(&self, registry: &mut PluginRegistry) -> Result<(), PluginError> {
        registry.register_tool("query_db", Arc::new(QueryDbTool::new(self.pool.clone())));
        registry.register_tool("describe_table", Arc::new(DescribeTableTool::new(self.pool.clone())));
        registry.register_tool("list_tables", Arc::new(ListTablesTool::new(self.pool.clone())));
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        self.pool.close().await;
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn xaft_plugin_create(config: *const c_char) -> *mut dyn XaftPlugin {
    let config_str = unsafe { CStr::from_ptr(config).to_str().unwrap_or("{}") };
    let config: PluginConfig = serde_json::from_str(config_str).unwrap_or_default();
    Box::into_raw(Box::new(MyDbPlugin::new(config)))
}
```

## Script Plugin (subprocess)

Simpler plugins run as subprocesses and communicate via stdin/stdout JSON protocol:

```bash
#!/usr/bin/env python3
# .xaft/plugins/my-script-plugin/plugin.py
# Protocol: JSON-RPC 2.0 over stdin/stdout

import json, sys

def handle_tool_call(name, input):
    if name == "my_tool":
        return {"result": f"processed: {input['query']}"}
    raise ValueError(f"unknown tool: {name}")

for line in sys.stdin:
    req = json.loads(line)
    if req["method"] == "list_tools":
        print(json.dumps({"result": [{"name": "my_tool", "description": "..."}]}))
    elif req["method"] == "call_tool":
        result = handle_tool_call(req["params"]["name"], req["params"]["input"])
        print(json.dumps({"result": result}))
    sys.stdout.flush()
```

## Built-in Plugin Registry

| Plugin ID | Tools provided | Status |
|---|---|---|
| `xaft-core-tools` | `read_file`, `write_file`, `list_files`, `search_files`, `apply_patch` | Built-in |
| `xaft-git` | `git_status`, `git_diff`, `git_commit`, `git_log`, `git_push` | Built-in |
| `xaft-shell` | `run_cargo`, `run_command` | Built-in |
| `xaft-index` | `search_code`, `find_symbol`, `get_deps` | Built-in |
| `xaft-github` | `create_pr`, `list_issues`, `get_review` | Optional |
| `xaft-linear` | `create_ticket`, `get_tickets` | Optional |
| `xaft-jira` | `create_issue`, `update_issue` | Optional |

## References

- agtrs: `agtrs-runtime/src/tool.rs` (Tool trait)
- Next: [MCP Compatibility →](02_mcp_compatibility.md)
EOF

cat > ./02_mcp_compatibility.md << 'EOF'
# MCP Compatibility

## Model Context Protocol Overview

The Model Context Protocol (MCP) is a standard for exposing tools to LLM clients. `xaft` can act as both an MCP **client** (consuming external tool servers) and an MCP **server** (exposing `xaft` tools to other clients).

## MCP Client Mode

`xaft` can load tools from any MCP-compatible server:

```toml
# .xaft/config.toml
[[mcp_servers]]
name = "filesystem"
url = "stdio://npx @modelcontextprotocol/server-filesystem /workspace"

[[mcp_servers]]
name = "github"
url = "stdio://npx @modelcontextprotocol/server-github"
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }

[[mcp_servers]]
name = "postgres"
url = "tcp://localhost:3001"
```

## MCP Bridge Implementation

```rust
// xaft-plugin/src/mcp_bridge.rs
use mcp_client::{McpClient, McpTool, McpCallResult};

pub struct McpToolAdapter {
    client: Arc<McpClient>,
    tool_def: McpTool,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str { &self.tool_def.name }
    fn description(&self) -> &str { self.tool_def.description.as_deref().unwrap_or("") }

    fn schema(&self) -> serde_json::Value {
        // Convert MCP input schema to agtrs-compatible JSON Schema
        mcp_schema_to_json_schema(&self.tool_def.input_schema)
    }

    async fn call(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled { reason: "MCP call cancelled".into() });
        }

        let result: McpCallResult = self.client
            .call_tool(&self.tool_def.name, input)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: self.tool_def.name.clone(),
                reason: e.to_string(),
            })?;

        let content = result.content.iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error.unwrap_or(false) {
            Ok(ToolResult::error(&content, &ctx.tool_use_id))
        } else {
            Ok(ToolResult::ok(&content, &ctx.tool_use_id))
        }
    }
}
```

## MCP Server Mode (expose xaft tools)

When running `xaft serve --mcp`, `xaft` exposes its tool registry as an MCP server:

```rust
// xaft-server/src/mcp_server.rs
pub async fn run_mcp_server(session: Arc<XaftSession>) -> Result<(), XaftError> {
    let tools: Vec<McpToolDefinition> = session
        .tool_registry
        .all_tools()
        .iter()
        .map(|(name, tool)| McpToolDefinition {
            name: name.clone(),
            description: Some(tool.description().to_string()),
            input_schema: json_schema_to_mcp_schema(tool.schema()),
        })
        .collect();

    let handler = McpRequestHandler { session: Arc::clone(&session), tools };
    mcp_stdio_server::run(handler).await
}
```

## Tool Schema Compatibility

MCP uses JSON Schema for tool input definitions. agtrs tools also use JSON Schema. The bridge performs minor normalization:

```rust
fn mcp_schema_to_json_schema(mcp: &serde_json::Value) -> serde_json::Value {
    // MCP schema is mostly compatible; normalize additionalProperties
    let mut schema = mcp.clone();
    if schema.get("additionalProperties").is_none() {
        schema["additionalProperties"] = serde_json::json!(false);
    }
    schema
}
```

## References

- MCP specification: https://modelcontextprotocol.io/
- agtrs: `agtrs-runtime/src/tool.rs`
EOF

echo "Plugin docs done"