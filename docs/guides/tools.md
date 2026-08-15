# Tools

xaft exposes a trait-based tool system covering filesystem, git, and shell
operations, plus dynamic tool creation.

## Filesystem tools

Read, write, edit, move, copy, delete, list, search, glob, tree, diff, and
file metadata tools — all with path-traversal protection and bounded output
(see [Security](security.md)).

## Git tools

Status, diff, log, show, blame, branch, add, commit, push, stash, merge, tag,
remote, grep — integrated with the per-session worktree isolation.

## Shell tools

`bash_exec` runs commands under a configurable execution policy; output is
bounded and stderr is preserved.

## Dynamic tools

`xaft-tools/src/dynamic/` provides a factory for creating tools at runtime
(scripted tools) and the registry that routes tool names to implementations.

## Tool registry

Every tool is registered in `xaft-tools/src/registry.rs` with schema, risk
level, and permission metadata. `/permissions` in the TUI lists them.

## MCP tools

External MCP servers contribute tools through the same registry — see
[MCP](mcp.md).

## Related

- [Security](security.md)
- [Reference: kernel](../reference/kernel.md)
