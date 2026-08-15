# Modes

Modes gate what the agent is allowed to do and shape its system prompt. The
interactive Shift+Tab cycle follows **Safe → Plan → Yolo**; the full registry
keeps six built-in modes for direct `/mode <name>` selection.

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

| Mode | Behaviour |
|---|---|
| `auto` | Default. No restrictions, no tool filter. |
| `plan` | Read-only. Tool filter = read + git-read tools; produces a numbered plan and ends with "Switch to Auto mode (Shift+Tab) to execute this plan." |
| `ask` | Confirmation mode. Agent describes and requests `[Y/n]` approval before every write or exec. |
| `review` | Read-only. Produces a structured code review (Summary / Issues / Suggestions / Assessment). |
| `safe` | Hard sandbox. Tool filter = minimal read-only subset (no network). |
| `debug` | Full capabilities plus a per-response debug footer with token/cost info. |

## Tool filters

Plan and Safe modes restrict which tools the agent may call:

- `plan` allows: `read_file`, `read_url`, `file_exists`, `file_stat`,
  `list_dir`, `tree`, `glob`, `grep_in_file`, `search_code`, `git_status`,
  `git_diff`, `git_log`, `git_show`, `git_blame`, `git_branch`, `git_grep`
- `safe` allows a subset: `read_file`, `file_exists`, `file_stat`, `list_dir`,
  `tree`, `glob`, `grep_in_file`, `search_code`, `git_status`, `git_diff`,
  `git_log`, `git_show`

(Verified in `crates/xaft-tui/src/mode/builtins.rs` — `PLAN_ALLOWED_TOOLS` and
`SAFE_ALLOWED_TOOLS`.)

## Switching modes

```bash
Shift+Tab                # cycle safe → plan → auto
/mode plan               # switch directly
/mode yolo               # alias → auto
/mode debug              # rejected (not in cycle)
/mode                    # list modes + aliases
```

Direct `/mode <name>` can still select any built-in mode; only the *cycle* is
restricted to Safe → Plan → Yolo.

## How modes shape the run

`ModeManager` applies the active mode to each run request:

- `mode_system_patch` — the mode's system prompt prefix (e.g. `[MODE: PLAN]`)
- `mode_tool_filter` — the tool allow-list the runtime enforces

So Plan mode not only *prompts* read-only behaviour — it *enforces* it at the
tool layer.

## Next

- [Slash commands →](06-commands.md)
- [Security →](10-security.md)
