# Background sessions

Long-running work can be detached from the foreground TUI and re-attached
later, keeping the terminal responsive while the agent keeps working.

## Detaching work

In the TUI, use `/bg` (or `/background`) to move the current task into the
background. The status bar shows the background entry count; the task keeps
streaming into its buffered mutation list.

## Re-attaching

`/bg list` shows running background entries. Re-attach with `/bg <n>` — the
buffered output replays into the scroll buffer and the task resumes foreground
routing.

## From the command line

Durable background sessions (the `xaft jobs` store) can be managed and
re-attached without the TUI's `/bg`:

```
xaft agents                      # interactive manager
xaft agents --json               # the current background sessions as JSON
xaft agents --run RUN --json     # the agents (main session + workers) of one goal run
xaft attach SESSION_ID           # open the TUI on one exact background session
xaft attach SESSION_ID --json    # hand the session off without opening the TUI
xaft attach --run RUN            # open the TUI on a goal run's main session
```

In the manager, `↑`/`↓` select, `Enter` attaches, `c` cancels, `r` resumes,
`a` archives, `d` moves to trash and `q` closes; the list refreshes every
second. Attaching an *active* session first moves it to `cancelled` with the
reason `foreground handoff requested`, so the background record and the
foreground TUI never both own the session; a finished session is handed over
unchanged. The TUI opens in the session's own working directory.

`attach` refuses an id it cannot use: no id, or a `run_…` goal-run id, is a
usage error (exit 2) with a pointer to the right form; an unknown or trashed
session exits 1.

## How it works

- Background entries are stored in `AppState::background_entries` with a
  bounded mutation buffer (`MAX_BUFFERED_MUTATIONS`); overflow is truncated
  with a `[bg] earlier output truncated …` line.
- Approval waits pause the background task; re-attach to answer.
- Session completion stores the session for resume chaining.

## Related

- [Session service](session-service.md)
- [TUI](tui.md)
