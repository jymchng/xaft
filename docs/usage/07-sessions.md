# Sessions

Sessions are the unit of durable, resumable work. xaft persists sessions and
conversations so you can pause, resume, and replay at any time.

## Session CLI

```bash
xaft session list              # list recent sessions (table format)
xaft session list --all        # include all sessions (not just active)
xaft session list -n 50        # show up to 50 sessions (default 20)
xaft session list -f json      # output format: table | json
xaft session show <id>         # show a session's details
xaft session resume <id>       # resume a session (-y to skip confirmation)
xaft session cancel <id>       # cancel a running session (-f to force)
```

(Verified: `SessionSubcommand` = List / Show / Resume / Cancel; `SessionListArgs`
has `-a/--all`, `-n/--number` default 20, `-f/--format`; `SessionShowArgs` has
`-f/--format` pretty|json; `SessionResumeArgs` has `-y`; `SessionCancelArgs`
has `-f`.)

## Resuming from the CLI

```bash
xaft --resume <session-id>          # resume a specific session
xaft --continue                     # -k: resume the most recent session
                                    # in the current directory
```

- `--resume <id>` (alias `-r`) loads the prior conversation context and picks
  up where the previous session left off, including pending file edits.
- `--continue` (`-k`) automatically finds and resumes the last active session
  for the project directory.
- `--session <id>` (`-s`) is a deprecated alias for `--resume`.

On resume, the newest 20 turns are replayed into the transcript with a
`Loading transcript…` label (see [TUI](04-tui.md)).

## How sessions work

- Each session has a status lifecycle: Active → Completed / Failed /
  Cancelled.
- Conversations are stored per agent under
  `<session>::workflow::<agent>` keys (planner, coder, qa, fixer), plus a
  session-level workflow key.
- On resume, xaft loads the planner conversation **and** the delegated agent
  conversations, splitting the transcript at handoff points with section
  headers.
- The session store is SQLite (behind the `session` feature in
  `xaft-session`), rooted at `[core].data_dir`.

## Tips

- Resume chaining: after a task completes, the TUI stores the completed
  session so the **next** task can pass `resume_session_id` and get full
  prior context.
- Use `xaft session cancel <id> -f` to force-cancel a stuck session.
- `xaft session show <id> -f json` is handy for scripting.

## Next

- [Memory →](08-memory.md)
- [TUI →](04-tui.md)
