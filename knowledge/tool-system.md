# Tool system

xaft's tool system (`crates/xaft-tools`) is trait-based, capability-gated, and
registry-driven.

## Tool crates

- `fs/` — read/write/edit/move/copy/delete/list/search/glob/tree/diff tools
  with path-traversal protection and bounded output.
- `git/` — status/diff/log/show/blame/branch/add/commit/push/stash/merge/tag/
  remote/grep, integrated with the per-session worktree.
- `shell/` — `bash_exec` under a configurable execution policy.
- `dynamic/` — a factory for runtime-created (scripted) tools.

## Registry

`registry.rs` maps tool names to implementations with schema, risk level, and
permission metadata. `/permissions` in the TUI lists them. MCP servers
contribute tools through the same registry.

## Safety contracts

- **Capability**: tools declare what they need; the active mode's tool filter
  gates them (Safe = read-only subset).
- **Path**: traversal protection on every fs tool.
- **Approval**: risk levels route through `ApprovalQueue`; low auto-approves,
  high requires a dialog.
- **Bounded output**: reads and command output are capped so the transcript
  never floods.

## Related

- `crates/xaft-tools/src/registry.rs`
- [Tools guide](../docs/guides/tools.md)
- [Security guide](../docs/guides/security.md)
