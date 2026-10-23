# Modes

Modes gate what the agent is allowed to do and shape its system prompt. The
interactive Shift+Tab cycle follows agenthicc's **Safe → Plan → Yolo** surface.

## The interactive cycle

| Cycle slot | Mode | Meaning |
|---|---|---|
| Safe | `safe` | Reads run directly; every side-effecting tool needs **explicit approval** |
| Plan | `plan` | Read-only: side effects are hard-blocked; produces a numbered plan |
| Yolo | `auto` | Full capabilities, no restrictions (default) |

Shift+Tab cycles `safe → plan → auto → safe`. The cycle maps the active mode
onto the canonical slot first, so starting from `ask`/`review` still advances
predictably.

## Safe: gate-and-ask

Safe is agenthicc's Safe. Read, search and git-read tools (`read_file`,
`grep`, `search_files`, `git_status`, `git_diff`, `git_log`, …) and workflow
control run directly. Every other tool stays available but must be approved
before **each** call: file writes and edits, git writes, `bash_exec` and the
other execution tools, the background-terminal tools, `read_url` (network),
memory writes, and any tool xaft does not recognise (MCP or dynamic tools fail
closed).

The gate is enforced by the runtime, not by the prompt: the active mode's
approval requirement travels with the run request, each matching tool is
marked as requiring confirmation, and the approval prompt appears exactly as it
does for a shell command. Safe-mode approvals are never answered by the
auto-approve policy — a write that would be waved through as low-risk in
another mode still needs your `a`/`y`.

## Aliases (agenthicc compatibility)

| Alias | Canonical |
|---|---|
| `yolo` | `auto` |
| `ask`, `guard` | `safe` |
| `review` | `plan` |

`debug` is **not** a mode, as in agenthicc: `/mode debug` answers
`Unknown mode 'debug'. Choose one of: auto, plan, safe.`, and no source can
register a mode with that name.

## The internal Replay mode

`replay` is registered but internal (agenthicc's `selectable=False`): it is
never in the cycle or the `/mode` listing, `/mode replay` is refused, and every
tool is blocked while it is active. `/replay` runs under it and restores your
mode afterwards.

## Switching modes

```bash
# TUI
Shift+Tab                # cycle safe → plan → auto
/mode plan               # switch directly
/mode yolo               # alias → auto
/mode debug              # rejected: not a mode
/mode                    # list modes + aliases
```

`/mode <name>` switches the active mode immediately (the footer shows
`✦ Switched to … mode`); the next run uses it.
