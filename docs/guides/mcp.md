# Connecting MCP servers

xaft can connect to Model Context Protocol (MCP) servers to extend the tool
surface with external capabilities.

## Configuration

MCP servers are declared in the config file under `[mcp.servers]`:

```toml
[mcp.servers]
[mcp.servers.my-server]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
env = {}
transport = "stdio"        # or "streamable_http" / "sse"
```

## Permissions

MCP tools are treated like any other tool: they are capability-gated and can
require approval depending on the active mode. Add MCP tool names to the
approval allowlist or deny list in `[security.permissions]`.

## Debugging

- `xaft doctor` checks MCP server connectivity and reports startup errors.
- `/mcp` in the TUI lists connected servers and their tool counts.

## Related

- [Tools](tools.md)
- [Security](security.md)
