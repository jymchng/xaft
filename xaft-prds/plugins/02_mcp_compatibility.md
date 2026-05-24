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
