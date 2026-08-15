# Security

xaft treats every mutation as reversible and every action as observable.

## Approval gates

- **Auto-approve policy**: Low-risk read-only tools auto-approve; medium-risk
  (edits, builds) auto-approve unless strict mode; high-risk (writes, network)
  require explicit approval.
- **Three-tier system**: per-tool confirmation, TUI approval dialogs, and
  auto-approve gates.
- `/permissions` lists the current tool permissions.

## Sandboxing

- Git worktree isolation per session — your working tree is never modified
  directly.
- Path-traversal protection on all file operations.
- Shell command sandboxing with configurable execution policy.
- `--dangerously-skip-permissions` requires a terminal confirmation dialog
  before the run starts.

## Modes

Mode selection (Safe/Plan/Yolo) is a first-line security control: Safe mode
hard-blocks writes, commands, and network; Plan mode is read-only. See
[Modes](modes.md).

## Secrets

API keys are read from environment variables; provider credentials never
appear in the transcript. Use `/config` to inspect the resolved config
(secrets are redacted).

## Related

- [MCP](mcp.md) — external tools inherit the same gates
- [Reference: repository state](../reference/repository-state.md)
