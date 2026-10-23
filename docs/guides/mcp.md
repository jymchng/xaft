# Connecting MCP servers

xaft can connect to Model Context Protocol (MCP) servers to extend the tool
surface with external capabilities.

## Configuration

MCP servers are declared as `[[mcp.client]]` stanzas (agenthicc's
`[[tools.mcp_servers]]` spelling is read too):

```toml
[[mcp.client]]
name = "files"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[[mcp.client]]
name = "github"
transport = "http"                       # streamable HTTP; also "sse" (legacy HTTP+SSE) and "ws"
url = "https://api.githubcopilot.com/mcp/"
token_env = "GITHUB_TOKEN"               # sent as `Authorization: Bearer …`
env_headers = { X-Org = "GITHUB_ORG" }   # header -> environment variable *name*
```

`xaft mcp add`, `list`, `get` and `remove` edit these stanzas without
connecting. Credentials are always referenced by environment-variable name;
`mcp get` and `mcp list --json` never print a token.

## Connecting, refreshing, diagnosing

Three commands start a server, inspect it and shut it down again:

| Command | Prints |
|---|---|
| `xaft mcp connect <name>` | the server's status record: `status` (`ready`, `failed`, `needs_auth`, `disabled`, …), `tool_count`, `catalog_revision`, `last_error`, timing and `auth_state` |
| `xaft mcp refresh <name>` | the catalog snapshot — protocol version, server info, capabilities, instructions, tools, prompts, resources and a SHA-256 `catalog_hash` — or `{"status": "unavailable"}` |
| `xaft mcp doctor [name] [--json]` | for every configured server (or one): its redacted config, then its validation error or its live status |

All three print JSON in exactly agenthicc's form (`sort_keys`, Python's
separators), take `-c PATH` to use a specific config file, and — like the rest
of the `mcp` family — report problems in the JSON rather than through the exit
code. `doctor` validates every server, live-starts only the enabled
`auto_connect` ones, and never invokes a tool.

How a start behaves:

- **Credentials first.** A server whose `token_env`, `${VAR}` token or
  `env_headers` variable is unset is reported `needs_auth` without being
  started.
- **Retries.** The connection is retried `reconnect_attempts` times with a
  doubling `reconnect_delay_seconds`, all inside a 10-second startup deadline.
- **Diagnostics.** A stdio server's last stderr output is appended to the
  error; tokens, header values and the command line are redacted out of it.
- **Network guard.** A remote server's host must be allowed by
  `[security]`: with `sandbox_mode = true` (the default) the host must be on
  `network_allow_list` — an empty list allows **no** remote host — and
  `sandbox_mode = false` lifts the check. A refused host fails with
  `Outbound request to '<host>' is not on the network allow-list.`

```toml
[security]
network_allow_list = ["api.githubcopilot.com"]
```

## Permissions

MCP tools are treated like any other tool: they are capability-gated and can
require approval depending on the active mode. Add MCP tool names to the
approval allowlist or deny list in `[security.permissions]`.

## Debugging

- `xaft mcp doctor` checks every configured server and reports startup errors.
- `/mcp` in the TUI lists connected servers and their tool counts.

## Related

- [Tools](tools.md)
- [Security](security.md)
- [CLI reference](../reference/cli.md)
