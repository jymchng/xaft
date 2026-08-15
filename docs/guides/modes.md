# Modes

Modes gate what the agent is allowed to do and shape its system prompt. The
interactive Shift+Tab cycle follows agenthicc's **Safe → Plan → Yolo** surface;
the full registry keeps six built-in modes for direct `/mode <name>` selection.

## The interactive cycle

| Cycle slot | Mode | Meaning |
|---|---|---|
| Safe | `safe` | Hard sandbox: read-only, minimal tool surface, no network |
| Plan | `plan` | Read-only: produces a numbered implementation plan |
| Yolo | `auto` | Full capabilities, no restrictions (default) |

Shift+Tab cycles `safe → plan → auto → safe`. The cycle maps the active mode
onto the canonical slot first, so starting from `ask`/`review`/`debug` still
advances predictably.

## Aliases (agenthicc compatibility)

| Alias | Canonical |
|---|---|
| `yolo` | `auto` |
| `ask`, `guard` | `safe` |
| `review` | `plan` |

`debug` is **not** an alias and is rejected via `/mode` (agenthicc parity).

## Full built-in registry

- `auto` — default, no restrictions
- `plan` — read-only plan (tool filter: read/git-read only)
- `ask` — confirmation mode (`[Y/n]` before writes/exec)
- `review` — structured code review (read-only)
- `safe` — hard sandbox (subset of plan tools)
- `debug` — full capabilities + per-response debug footer

## Switching modes

```bash
# TUI
Shift+Tab                # cycle safe → plan → auto
/mode plan               # switch directly
/mode yolo               # alias → auto
/mode debug              # rejected (not in cycle)
/mode                    # list modes + aliases
```

Direct `/mode <name>` can still select any built-in mode; only the *cycle* is
restricted to Safe → Plan → Yolo.
