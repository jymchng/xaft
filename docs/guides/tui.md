# TUI

The xaft terminal UI is a conversational streaming renderer: agent output,
tool calls, and tokens stream into an append-only transcript in the primary
terminal buffer. History scrolls naturally into scrollback — no alternate
screen.

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

## Modes

Shift+Tab cycles **Safe → Plan → Yolo** (see [Modes](modes.md)). `/mode`
switches directly; `/mode` with no args lists the cycle, the full registry,
and aliases.

## Input triggers

- `/` — slash command picker (backed by the command registry)
- `@` — file/directory mention picker (workspace-relative)
- `$` — skill-only picker (agenthicc parity; backed by `xaft-skills`)
- `#` — input history recall

Trigger selection may update the input buffer or submit immediately.

## Bracketed paste

Multi-line pastes are delivered as a single `Event::Paste` payload and kept
behind a `[Pasted text #N preview…]` composer placeholder while you edit:

- `Home`/`End` operate on the visible one-line projection
- `Backspace` immediately after the closing `]` deletes the whole paste
- `Ctrl+V` reveals the full pasted text (newlines shown as `⏎`)
- `Esc` after the `]` deletes the entire hidden paste
- `Enter` submits the remaining original contents plus edits

## Approvals

Tools that write, run commands, or touch the network require approval
depending on the active mode and the auto-approve policy. Approval requests
show an inline prompt; the queue routes through `ApprovalQueue` and the TUI
renders `ToolPendingApproval` events.

While an approval, plan review, or question overlay is pending, the status bar
shows a stable waiting label (no timer redraw).

## Questions (`ask_user`)

An agent that needs a decision only you can make — a goal, a constraint, a
priority — calls `ask_user` with one or more questions, each with a few
choices. The questions overlay opens below the input bar and owns the
keyboard until you answer:

- `←`/`→` move between questions, `↑`/`↓` between choices (they wrap);
- every question also offers **Other — type your answer**: `Enter` on it
  opens a free-text editor (`Esc` returns to the choices);
- `Enter` confirms a choice and moves to the next unanswered question; on the
  last one it submits every answer at once;
- `Esc` dismisses the round — the agent is told you cancelled.

The title shows how many questions are answered and the seconds left before
`tools.question_timeout_s` (default 300) expires; an unanswered round tells
the agent to proceed on its safest assumption. Your answers are recorded in
the transcript. `ask_user` is available in every turn and every workflow
phase (Plan mode included — asking is not a side effect); a headless run has
nobody to ask, so the agent receives `{"cancelled": true}`.

## Overlays

Some commands open an interactive overlay drawn below the input bar, in the
space the trigger dropdown uses: `/ps` (background terminals — `s` stops the
selected one) and `/loops` (persisted loop jobs — `Enter` runs one now, `d`
deletes after confirmation). While an overlay is open it owns the keyboard —
`↑↓`/`j`/`k` select, `Esc` closes — and it redraws when the data behind it
changes, e.g. when a terminal exits.

## Startup and the status line

Startup is split into timed phases: `configuration`, `runtime`,
`session_service` (degraded, with the reason, when the SQLite store cannot be
opened), `selected_restore` (on `--resume`), `terminal_manager`,
`loop_scheduler`, `first_frame` and `time_to_interactive_input`. On a fresh
session the workspace file index (`@` autocomplete) and skill discovery are
**deferred** until after the first frame, so the prompt appears without
waiting for them; the line above the prompt shows `◌ Startup: loading …` until
they finish. `/startup` prints the full report — `name: state (elapsed)` per
phase, with `deferred` and a redacted error where they apply.

When the session is idle and a `/loop` is live, the same line shows the loop's
status and a live `next in …` countdown.

## Resumed transcript

When you open xaft with an existing session (`--resume <id>`), the newest 20
complete turns are replayed from the session tail with a `Loading transcript…`
status label. Replay is chunked so the TUI stays responsive. Set
`[session] resume_transcript_turns = N` to change the bound (`0` = full).

## Collapsed tool groups

Contiguous tool completions are collapsed into a group. The overflow count
lives in the live footer while the group is open and is flushed to the scroll
buffer as `…and N more tool calls` at the next conversation boundary (or
immediately on interrupt).

## Telemetry

After a turn returns to IDLE the scroll buffer prints:

```text
✻ Worked for 1m 5s
✾ Total wall clock time since last IDLE: 2m 5s
Tokens: 12.4k in / 3.1k out  ·  Cost: $0.4200
```

The total wall-clock span covers the outer user activity across internal LLM
turns and workflow phases.

## Exit summary

The same telemetry prints in the exit summary footer, plus the session ID and
`xaft --resume <id>` hint.

## Related

- [Modes](modes.md) — the Safe → Plan → Yolo cycle
- [Reference: kernel](../reference/kernel.md)
