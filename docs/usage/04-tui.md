# The TUI

The xaft terminal UI is a **conversational streaming renderer**: agent output,
tool calls, and tokens stream into an append-only transcript in the primary
terminal buffer. History scrolls naturally into scrollback — there is no
alternate screen by default. This guide covers the interactive experience.

## Screen model

```text
terminal
├── scroll buffer
│   ├── agent text
│   ├── tool results and collapsed tool groups
│   └── workflow/system/retry notifications
└── live block
    ├── status component (mode, tokens, cost)
    ├── composer / active overlay
    └── footer
```

## Input triggers

Typing a trigger character opens a picker:

| Trigger | Picker |
|---|---|
| `/` | Slash command picker |
| `@` | File/directory mention picker (workspace-relative) |
| `$` | Skill-only picker (from `xaft-skills`) |
| `#` | Input history recall |

Select with arrow keys + Enter, or keep typing to filter. Trigger selection
may update the input buffer or submit immediately.

## Bracketed paste

Multi-line pastes are kept behind a `[Pasted text #N preview…]` composer
placeholder while you edit:

- `Home` / `End` move within the visible one-line projection
- `Backspace` immediately after the closing `]` deletes the **whole** paste
- `Ctrl+V` reveals the full pasted text (newlines shown as `⏎`)
- `Esc` after the `]` deletes the entire hidden paste
- `Enter` submits the remaining original contents plus your edits

## Modes

Shift+Tab cycles **Safe → Plan → Yolo** (see [Modes](05-modes.md)). `/mode`
switches directly; `/mode` with no args lists the cycle, the full registry,
and aliases. The active mode's badge shows in the status bar (hidden in Auto).

## Approvals

Tools that write, run commands, or touch the network require approval
depending on the active mode and the auto-approve policy. Approval requests
show an inline prompt; the queue routes through the approval system.

While an approval, plan review, or question overlay is pending, the status
bar shows a stable waiting label (no timer redraw).

## Resumed transcript

When you open xaft with an existing session (`--resume <id>` or `--continue`),
the newest 20 complete turns are replayed from the session tail with a
`Loading transcript…` status label. Replay is chunked so the TUI stays
responsive. See [Sessions](07-sessions.md).

## Collapsed tool groups

Contiguous tool completions are collapsed into a group. The overflow count
lives in the live footer while the group is open and is flushed to the scroll
buffer as `…and N more tool calls` at the next conversation boundary — or
immediately when the agent is interrupted.

## Telemetry

After a turn returns to IDLE, the scroll buffer prints:

```text
✻ Worked for 1m 5s
✾ Total wall clock time since last IDLE: 2m 5s
Tokens: 12.4k in / 3.1k out  ·  Cost: $0.4200
```

The total wall-clock span covers the outer user activity across internal LLM
turns and workflow phases.

## Exit summary

On exit, the same telemetry prints as a footer plus the session ID and resume
hint:

```text
────────────────────────────────────────────────
  ✻ Worked for 1m 5s
  ✾ Total wall clock time since last IDLE: 1m 5s
  Tokens: 12.4k in / 3.1k out  ·  Cost: $0.4200
  Session: <id>
  Resume:  xaft --resume <id>
────────────────────────────────────────────────
```

## Background sessions

Long-running work can be moved to the background so the terminal stays
responsive:

- `/bg` (or `/background`) — move the current task into the background
- `/bg list` — list running background entries
- `/bg <n>` — re-attach and replay buffered output

Background entries buffer output (bounded); overflow is truncated with a
`[bg] earlier output truncated …` line.

## TUI config

Tune the TUI in `[tui]` (verified in `TuiConfig`):

```toml
[tui]
theme = "dark"
mouse = false
timestamps = false
conversation_height = 30
preserve_output_on_exit = true     # keep content in scrollback after exit
use_alternate_screen = true        # false renders directly to primary screen
```

## Next

- [Modes →](05-modes.md)
- [Slash commands →](06-commands.md)
- [Sessions →](07-sessions.md)
