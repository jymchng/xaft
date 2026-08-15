# Workflows and modes

Modes gate what the agent can do; workflows shape *how* it does a task.

## Modes

The interactive cycle is **Safe → Plan → Yolo** (agenthicc parity):

| Slot | Mode | Effect |
|---|---|---|
| Safe | `safe` | Read-only sandbox, minimal tool surface, no network |
| Plan | `plan` | Read-only, produces a numbered implementation plan |
| Yolo | `auto` | Full capabilities, no restrictions (default) |

Aliases: `yolo`→`auto`, `ask`/`guard`→`safe`, `review`→`plan`. `debug` is
rejected via `/mode`. Shift+Tab cycles; `/mode <name>` switches directly.

## Workflows

The standard workflow is Plan → Code → Verify → Commit:

1. **Plan** — classify the task (informational vs coding), produce a
   step-shaped plan.
2. **Code** — walk the steps with the planned tools.
3. **Verify** — QA reviews the diff against the plan.
4. **Fix** — the Fixer addresses feedback (up to 14 handoffs, cycle-detected).
5. **Commit** — commit the session worktree on success; roll back on failure.

Custom planners return the same step shape, so the Coder/QA pipeline is
unchanged.

## Mode plumbing

`ModeManager::apply_to_run_request` sets `RunRequest.mode_system_patch` and
`RunRequest.mode_tool_filter` from the active mode before each run.

## Related

- [Modes guide](../docs/guides/modes.md)
- [Workflows guide](../docs/guides/workflows.md)
- PRD-64 / PRD-65 in `prds/`
