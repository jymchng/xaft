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

Safe is agenthicc's Safe, decided by each tool's **capability**. Read, search
and git-read tools (`read_file`, `grep`, `search_files`, `git_status`,
`git_diff`, `git_log`, …) and workflow control (`ask_user`, handoffs, workflow
transitions) run directly. Every tool with a restricted capability stays
available but must be approved before **each** call: file writes and edits
(`write`), git writes (`git_write`), `bash_exec`, the other execution tools and
the background-terminal tools (`execute`), `read_url`, browsers and MCP tools
(`network`), memory writes (`memory`), and any tool xaft does not recognise
(`undeclared` — dynamic tools fail closed).

## Plan: read-only

Plan hard-blocks exactly the capabilities Safe asks about — `write`,
`git_write`, `execute`, `network`, `memory` and `undeclared` — and keeps the
rest: reads, searches, git reads, and workflow control, so the agent can still
ask you questions while it plans.

The gate is enforced by the runtime, not by the prompt: the active mode's
approval requirement travels with the run request, each matching tool is
marked as requiring confirmation, and the approval prompt appears exactly as it
does for a shell command. Safe-mode approvals are never answered by the
auto-approve policy — a write that would be waved through as low-risk in
another mode still needs your `a`/`y`. That holds for a run you moved to the
background with Ctrl+B as well, and the mode that counts is the one the run was
submitted under: switching modes, or queueing another task behind it, does not
change it.

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
