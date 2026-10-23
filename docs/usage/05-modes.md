# Modes

Modes gate what the agent is allowed to do and shape its system prompt. The
interactive Shift+Tab cycle follows agenthicc's **Safe → Plan → Yolo**.

## The interactive cycle

| Cycle slot | Mode | Meaning |
|---|---|---|
| Safe | `safe` | Reads run directly; every side-effecting tool needs explicit approval |
| Plan | `plan` | Read-only: produces a numbered implementation plan |
| Yolo | `auto` | Full capabilities, no restrictions (default) |

Shift+Tab cycles `safe → plan → auto → safe`. The cycle maps the active mode
onto the canonical slot first, so starting from `ask`/`review` still advances
predictably.

## Aliases (agenthicc compatibility)

| Alias | Canonical |
|---|---|
| `yolo` | `auto` |
| `ask`, `guard` | `safe` |
| `review` | `plan` |

`debug` is **not** a mode (agenthicc parity): `/mode debug` answers
`Unknown mode 'debug'. Choose one of: auto, plan, safe.`

## Full built-in registry

| Mode | Behaviour |
|---|---|
| `auto` | Default. No restrictions, no tool filter. |
| `plan` | Read-only. Tool filter = read + git-read tools; produces a numbered plan and ends with "Switch to Auto mode (Shift+Tab) to execute this plan." |
| `ask` | Confirmation mode. Agent describes and requests `[Y/n]` approval before every write or exec. |
| `review` | Read-only. Produces a structured code review (Summary / Issues / Suggestions / Assessment). |
| `safe` | agenthicc's Safe. No tool filter; every tool outside the read/search/git-read set needs an explicit approval before each call (enforced by the runtime, never auto-approved). |

`ask` and `review` are registered but shadowed by the alias table, so
`/mode ask` lands on `safe` and `/mode review` on `plan`. The registry also
holds agenthicc's internal `replay` mode: it blocks every tool, is never
cycled, listed or selectable, and is what `/replay` runs under.

## Tool filters and approvals

- `plan` **filters** tools to: `read_file`, `read_url`, `file_exists`,
  `file_stat`, `list_dir`, `tree`, `glob`, `grep_in_file`, `search_code`,
  `git_status`, `git_diff`, `git_log`, `git_show`, `git_blame`, `git_branch`,
  `git_grep` — side effects are hard-blocked.
- `safe` **gates** instead of filtering: tools in `SAFE_DIRECT_TOOLS` (reads,
  searches, git reads such as `read_file`, `grep`, `search_files`,
  `git_status`, `git_log`, plus workflow control) run directly; everything
  else — writes, git writes, `bash_exec` and other execution, the terminal
  tools, `read_url`, memory writes, unknown tools — must be approved first.

(Verified in `crates/xaft-tui/src/mode/builtins.rs` — `PLAN_ALLOWED_TOOLS`,
`SAFE_DIRECT_TOOLS`.)

## Switching modes

```bash
Shift+Tab                # cycle safe → plan → auto
/mode plan               # switch directly
/mode yolo               # alias → auto
/mode debug              # rejected: not a mode
/mode                    # list modes + aliases
```

`/mode <name>` switches immediately (the footer shows `✦ Switched to … mode`).

## How modes shape the run

`ModeManager` applies the active mode to each run request:

- `mode_system_patch` — the mode's system prompt prefix (e.g. `[MODE: PLAN]`)
- `mode_tool_filter` — the tool allow-list the runtime enforces (Plan)
- `mode_approval_filter` — the tools that must be approved before each call
  (Safe); the runtime marks them as requiring confirmation, so they block on
  the approval gate like a shell command

So Plan mode not only *prompts* read-only behaviour — it *enforces* it at the
tool layer — and Safe's approvals are a runtime gate, not a request to the
model.

## Next

- [Slash commands →](06-commands.md)
- [Security →](10-security.md)
