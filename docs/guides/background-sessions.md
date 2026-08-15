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

## How it works

- Background entries are stored in `AppState::background_entries` with a
  bounded mutation buffer (`MAX_BUFFERED_MUTATIONS`); overflow is truncated
  with a `[bg] earlier output truncated …` line.
- Approval waits pause the background task; re-attach to answer.
- Session completion stores the session for resume chaining.

## Related

- [Session service](session-service.md)
- [TUI](tui.md)
