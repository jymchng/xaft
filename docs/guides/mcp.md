# Connecting MCP servers

xaft connects to Model Context Protocol (MCP) servers and gives every agent
turn their tools.

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
transport = "http"                       # streamable HTTP; also "http+sse" (legacy HTTP+SSE) and "ws"
url = "https://api.githubcopilot.com/mcp/"
token_env = "GITHUB_TOKEN"               # sent as `Authorization: Bearer …`
env_headers = { X-Org = "GITHUB_ORG" }   # header -> environment variable *name*
auto_connect = true                      # connect when the session starts (default)
reconnect_attempts = 3                   # retries; also the relaunch budget of a dropped connection
reconnect_delay_seconds = 1.0            # doubles after each failed attempt
```

`xaft mcp add`, `list`, `get` and `remove` edit these stanzas without
connecting. Credentials are always referenced by environment-variable name;
`mcp get` and `mcp list --json` never print a token.

## In a session

When a session starts, every configured server is registered and the
`auto_connect` ones are started:

- **The TUI** starts them in the background after the first frame, so a slow
  server never delays the prompt. A turn uses whatever is ready at that
  moment; a server still starting simply contributes no tools to that turn.
  A server that fails to start is reported in the transcript:
  `MCP server 'github' is unavailable; continuing without its tools.`
- **A headless run** (`xaft run`, the stdin protocol, a background job)
  starts them before the first model call and stops them when the run ends.

Each tool a server lists becomes an agent tool named
`mcp__<server>__<tool>` (characters outside `[A-Za-z0-9_-]` become `_`; a
name over 64 characters is shortened with a hash). Every agent of every
workflow — the planner, coder, QA and fixer, dynamic and meta agents, and
each phase of a phased workflow — is offered them. A server's
`instructions` are added to the system prompt, labelled as untrusted
metadata from that server.

How a server behaves while connected:

- **Catalog changes.** When a server sends
  `notifications/tools/list_changed`, its catalog is re-fetched and
  republished. A call made through a tool from the replaced catalog is
  refused (`… belongs to a stale catalog revision`) instead of being sent to
  a tool that may have changed meaning.
- **Dropped connections.** If a server's process exits or its stream ends,
  the call in flight fails and the next call relaunches the server, at most
  `reconnect_attempts` times over the connection's life. Once that budget is
  spent the server is `failed` and its tools are withdrawn until it is
  connected again.
- **Results.** A tool's `structuredContent.result` is returned when present;
  otherwise its text (decoded when it is JSON). A result marked `isError`
  reaches the model as a failed tool call:
  `MCP server 'github' returned error for 'create_issue': …`.
- **Timeouts.** A server must finish starting within 10 seconds and each
  call within 60 seconds.

## Permissions

MCP tools have the `network` capability:

- **Plan** (and Review) mode never offers them.
- **Safe** mode asks for approval before every MCP call, like any other
  side-effecting tool.
- A **headless** run in Safe mode denies the call unless it was started with
  `-y`/`--auto-approve` or `--dangerously-skip-permissions`.
- A **workflow phase** receives them only when its capabilities allow
  `network` (the default for generic phases; never for a read-only role).

A remote server's host must also pass `[security]`: with
`sandbox_mode = true` (the default) the host must be on `network_allow_list` —
an empty list allows **no** remote host — and `sandbox_mode = false` lifts
the check. A refused host fails with
`Outbound request to '<host>' is not on the network allow-list.`

```toml
[security]
network_allow_list = ["api.githubcopilot.com"]
```

## Managing servers in the TUI: `/mcp`

| Command | Effect |
|---|---|
| `/mcp`, `/mcp status`, `/mcp list` | every server's status record as indented JSON |
| `/mcp connect NAME` | start one server, including an `auto_connect = false` one |
| `/mcp disconnect NAME` | close it; its tools leave the session |
| `/mcp refresh NAME` | re-fetch its catalog |
| `/mcp reload` | reconnect every enabled server and republish every catalog |
| `/mcp doctor [NAME]` | validate and live-start, then print the per-server report |

Actions run in the background: the command answers
`/mcp connect scheduled for github.` at once, and
`/mcp connect complete for github.` (or `/mcp connect failed: …`) follows when
the work ends. A status record has `status` (`configured`, `starting`,
`ready`, `refreshing`, `degraded`, `failed`, `needs_auth`, `disabled`,
`stopped`, …), `tool_count`, `catalog_revision`, `last_error`, `started_at`,
`last_success_at`, `auth_state` and `generation_id`.

## Connecting, refreshing, diagnosing from the shell

Three commands start a server, inspect it and shut it down again:

| Command | Prints |
|---|---|
| `xaft mcp connect <name>` | the server's status record |
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
  doubling `reconnect_delay_seconds`, all inside the 10-second startup deadline.
- **Diagnostics.** A stdio server's last stderr output is appended to the
  error; tokens, header values and the command line are redacted out of it.

## Differences from agenthicc

- Tools are offered as `mcp__<server>__<tool>`, not `mcp_<server>_<tool>`,
  so the capability classifier recognises them by prefix. The canonical
  identity `mcp:<server>:<tool>` is unchanged.
- `[[mcp.client]]` has no `required`, `startup_timeout_s`, `tool_timeout_s`,
  `enabled_tools` or `disabled_tools`: every server is optional and
  agenthicc's defaults apply.
- A server whose relaunch budget is spent reports `failed` (agenthicc keeps
  reporting `ready` while its calls fail).
- `/mcp … failed:` lines carry the message without a Python exception class
  name.

## Related

- [Tools](tools.md)
- [Subagents](subagents.md)
- [Security](security.md)
- [CLI reference](../reference/cli.md)
