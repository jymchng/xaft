# Your first task

This guide walks you through running xaft end-to-end: launching it, giving it
a task, watching it plan and execute, and handling approvals. Every flag is
verified against `crates/xaft-cli/src/args.rs`.

## Launching

### Interactive TUI (bare `xaft`)

```bash
xaft
```

With no subcommand, xaft opens the interactive TUI. Type a task in the input
bar and press Enter. (Verified: `XaftCli.command` is `Option<Commands>` — bare
`xaft` runs the TUI.)

### One-shot task (`xaft run`)

```bash
xaft run "Add pagination to the /users API endpoint"
```

`run` (alias `r`) executes a task through the plan → code → verify → commit
pipeline. The task is a natural-language description — be specific about
files, modules, or acceptance criteria:

```bash
xaft run "Fix the connection pool leak in src/db/pool.rs"
xaft run "Write unit tests for all public functions in src/math.rs"
xaft run "Migrate from reqwest 0.11 to 0.12" --model claude-3-opus
```

## What happens when you run a task

1. **Plan** — xaft reads your codebase, classifies the task (informational vs
   coding), and produces a numbered implementation plan.
2. **Approve / execute** — each step runs through the approval gate (see
   [Security](10-security.md)); file edits go through a transactional
   workspace and git operations use an isolated worktree.
3. **Verify** — the QA agent reviews the diff against the plan; the Fixer
   addresses gaps.
4. **Commit** — on success, changes are committed in the session's git
   worktree; on failure the worktree is rolled back.

## Plan without executing (`--dry-run`)

```bash
xaft run "Add rate limiting to the API" --dry-run
```

The agent plans and prints what it *would* do, but **no files are modified
and no shell commands run**. Perfect for reviewing an approach before
committing to it.

## Headless / CI mode

For pipelines and scripting, disable the TUI:

```bash
xaft run "task" --headless            # plain output, no TUI
xaft run "task" --json                # newline-delimited JSON events
```

`--json` implies `--headless` and emits structured events you can pipe into
other tools.

## Approvals

By default, shell commands and destructive operations require confirmation:

```bash
xaft run "task"                    # you approve commands as they come
xaft run "task" --auto-approve     # -y / --yes: auto-approve all prompts
```

`--auto-approve` is equivalent to setting `guardrail.command_approval = false`
in config. Use with care.

To skip **all** approval gates (shell commands, deletions, etc.):

```bash
xaft run "task" --dangerously-skip-permissions
```

In TUI mode a danger warning is displayed and you must explicitly confirm
before the run proceeds. **Use with extreme caution.**

## Model / provider overrides

```bash
xaft run "task" --model claude-3-5-sonnet-20241022
xaft run "task" --provider openai
xaft run "task" --max-turns 50
xaft run "task" --temperature 0.0
xaft run "task" --agent default        # use a named agent preset
```

(Verified in `RunArgs`: `--model/-m`, `--provider`, `--max-turns`,
`--temperature`, `--agent/-a`.)

## Config / project

```bash
xaft run "task" --config ./my-xaft.toml        # explicit config file
xaft run "task" --project-dir /path/to/repo    # override project root
```

## Log level / telemetry

```bash
xaft run "task" --log-level debug     # more verbose output
xaft run "task" --no-telemetry        # disable telemetry for this run
```

(Verified: `--log-level` accepts trace/debug/info/warn/error.)

## Expected output shape

In headless mode you'll see the plan, tool calls, and results stream by. In
the TUI, output renders into the scroll buffer with the live status bar
showing mode, tokens, and cost. When the run finishes, the exit summary shows:

```text
────────────────────────────────────────────────
  ✻ Worked for 1m 5s
  ✾ Total wall clock time since last IDLE: 1m 5s
  Tokens: 12.4k in / 3.1k out  ·  Cost: $0.4200
  Session: <id>
  Resume:  xaft --resume <id>
────────────────────────────────────────────────
```

## Next

- [The TUI walkthrough →](04-tui.md)
- [Modes →](05-modes.md)
- [Sessions →](07-sessions.md)
