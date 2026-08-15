# Security

xaft treats every mutation as reversible and every action as observable. This
guide covers the safety model, verified against `crates/xaft-config`
(`GuardrailConfig`), `crates/xaft-tui/src/approval.rs`, and the tool crates.

## Approval gates

Tools carry a **risk level**:

| Risk | Policy |
|---|---|
| `LOW` | Always auto-approved (read-only, non-destructive) |
| `MEDIUM` | Auto-approved unless strict mode is on |
| `HIGH` / `CRITICAL` | Always gated to the user |

File edits and build commands are typically medium; deletions, shell
commands, and network access are high/critical.

The approval flow:

1. A tool requests execution with its risk level.
2. The approval queue (`ApprovalQueue`) applies the auto-approve policy.
3. `ToolPendingApproval` events surface as an inline prompt in the TUI.
4. You approve or reject; rejected tools never execute.

CLI controls:

```bash
xaft run "task"                    # approve as you go
xaft run "task" --auto-approve     # -y: auto-approve everything
xaft run "task" --dangerously-skip-permissions   # skip ALL gates
```

`--dangerously-skip-permissions` shows a danger confirmation in the TUI
before proceeding. **Use with extreme caution** — it allows shell commands,
file deletions, and other destructive operations without confirmation.

## Guardrails

`[guardrail]` in `xaft.toml` (verified in `GuardrailConfig`):

```toml
[guardrail]
file_destruction = true     # block destructive file operations
secret_leakage = true       # detect + redact secrets in tool output
cost_limit = true           # enforce token/cost limits
command_approval = true     # require approval before shell commands
```

### Cost limits

```toml
[guardrail.cost_limit_config]
max_spend = 5.0             # USD per session
max_tokens_per_request = 4000
warn_at_percent = 80        # warn at 80% of the limit
```

When `cost_limit` is active, xaft stops spending past `max_spend` and warns
as you approach it.

### Secret leakage

```toml
[guardrail.secret_leakage_config]
patterns = ["AKIA[0-9A-Z]{16}", "sk-[a-zA-Z0-9]{20,}"]
# action on detection: "block" | "redact" | "warn"
```

Detected secrets in tool output are blocked, redacted, or warned on per the
configured action.

## Git worktree isolation

Every session runs in an **isolated git worktree** — your main working tree
is never modified directly. File edits go through a transactional workspace;
on success changes are committed in the session worktree, and on failure the
worktree is rolled back. This makes every run reversible.

## Path-traversal protection

All filesystem tools enforce path guards: `..` escapes, absolute-path
violations, and symlink escapes are rejected unless explicitly allowed.

## Modes as a security control

Modes are a first-line control:

- **Safe** — hard-blocks writes, commands, and network (read-only subset).
- **Plan** — read-only; produces a plan, executes nothing.
- **Yolo/Auto** — full capabilities (default; use with care in untrusted
  repos).

See [Modes](05-modes.md).

## Permissions

- `/permissions` in the TUI lists every tool's permission metadata.
- `[agent.<name>] allowed_tools / denied_tools` restrict which tools a preset
  may call.
- Dynamic tools require `allow_dynamic_tools = true` and can be gated by
  `dynamic_tool_approval`.

## Secrets

- Provider keys are read from environment variables (`ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`) or `api_key_env` — never hardcode them in `xaft.toml`.
- The transcript redacts detected secrets via the secret-leakage guardrail.

## Related

- [Modes →](05-modes.md)
- [Configuration →](02-configuration.md)
- [Tools →](09-tools.md)
