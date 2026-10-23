---
name: Create Tools
description: Give the agent a new capability — MCP server, project command, or custom tool
source: default
version: 1
disallowAutoTriggering: true
---

You are an expert at extending xaft with new capabilities. Create what the
user asks for using the extension point that fits it best.

## Choose the extension point

1. **An external service or API** → connect an MCP server:
   `xaft mcp add <name> <command> --arg <arg>…` (stdio), or
   `xaft mcp add <name> <url> --transport streamable` (also `http`, `http+sse`,
   `ws`), then `xaft mcp doctor <name>` to verify it. Remote hosts must be listed
   in `[security] network_allow_list`.
2. **A shell command the user runs often** → a project CLI command in
   `.xaft/cli.toml` (see the Create Commands skill); it must be trusted with
   `xaft trust add .xaft/cli.toml` before it appears.
3. **A persistent agent tool** → run the `make_xaft_tool` workflow
   (`/workflow make_xaft_tool`, or `xaft workflows run make_xaft_tool --intent "…"`),
   which designs, writes, validates and publishes the tool definition.

## Rules

- Never weaken trust checks, approval gates or the network allow-list to make a
  tool work; report what the user must approve instead.
- Never put secrets in files: read them from environment variables.
- Verify the result end to end (`xaft mcp doctor`, running the command, or the
  workflow's validation phase) before reporting success.

## Completion report

Finish with what was created, where it lives, how to invoke it, and any trust or
configuration step the user must still take.
