cat > ./01_cli_ux_design.md << 'EOF'
# CLI UX Design

## Command Structure

```
xaft <COMMAND> [OPTIONS]

Commands:
  run      Execute a coding task autonomously
  chat     Interactive conversation mode with streaming TUI
  search   Search the codebase semantically
  index    Build or update the repository index
  plan     Generate a plan without executing it
  resume   Resume a suspended or interrupted session
  rollback Undo changes from a session
  status   Show active session status
  history  Show session history
  diff     Show staged changes from a session
  serve    Start the remote agent HTTP server
  config   Show or edit configuration
  version  Show version information
```

## run Command

```
xaft run <GOAL> [OPTIONS]

Arguments:
  <GOAL>    Natural language description of the task

Options:
  -c, --constraint <TEXT>   Add a constraint (can repeat)
  -b, --budget <USD>        Cost budget (default: from config)
  -t, --timeout <SECS>      Execution timeout
  -m, --model <MODEL>       Override primary model
      --planner <STRATEGY>  Planner: oneshot|iterative|tree
      --auto-approve <LEVEL> none|low|medium|high
      --no-tui              Plain streaming output (no TUI)
      --dry-run             Plan only, don't execute
      --create-pr           Create a PR after completion
      --worktree <PATH>     Use existing worktree
  -v, --verbose             Verbose logging

Examples:
  xaft run "add JWT authentication to the auth module"
  xaft run "migrate serde_json errors to anyhow" --constraint "no API changes"
  xaft run "add unit tests for the parser module" --budget 0.50
  xaft run "fix the race condition in tests" --auto-approve medium
```

## chat Command (Interactive Mode)

```
xaft chat [OPTIONS]

Options:
  --agent <NAME>    Use specific agent (default: code)
  --no-history      Don't load previous conversation
  --session <ID>    Continue specific session

Interactive commands (while in chat):
  /plan             Show current plan
  /status           Show session status
  /diff             Show staged changes
  /approve          Approve pending tool call
  /deny             Deny pending tool call
  /suspend          Suspend current task
  /resume           Resume suspended task
  /cost             Show cost breakdown
  /help             Show available commands
  /quit             Exit chat
```

## search Command

```
xaft search <QUERY> [OPTIONS]

Options:
  -n, --limit <N>       Max results (default: 10)
  -t, --type <TYPE>     function|struct|enum|trait|all
  -f, --file <GLOB>     Restrict to files matching glob
      --semantic        Force semantic search (embeddings)
      --exact           Exact string match only

Examples:
  xaft search "authentication handler"
  xaft search "impl Error" --type trait
  xaft search --exact "fn process_request"
```

## Terminal UX Principles

### 1. Progressive Disclosure

Simple invocation: `xaft run "goal"` — shows essential TUI with sane defaults.
Advanced users: flags for every knob.

### 2. Keyboard-First

Every action achievable without mouse. Keyboard shortcuts shown in status bar.

### 3. Interruptible

`Ctrl-C` always clean-exits with checkpoint. No abandoned worktrees.

### 4. Transparent

Every agent decision visible in the TUI. No black box. No "AI is thinking..." spinners without content.

### 5. Recoverable

`xaft resume` works after any interruption. Sessions are durable.

## Output Modes

| Mode | Flag | Description |
|---|---|---|
| Full TUI | (default) | Ratatui with all panes |
| Streaming | `--no-tui` | Plain terminal streaming output |
| JSON | `--output json` | Machine-readable JSON stream |
| Quiet | `--quiet` | Only final result |

## Color Themes

```toml
[ui.theme]
name = "dark"    # "dark" | "light" | "high-contrast" | custom path

# Custom theme
[ui.theme.colors]
agent_text = "#e0e0e0"
tool_call = "#61afef"
success = "#98c379"
error = "#e06c75"
warning = "#e5c07b"
cost = "#c678dd"
```

## References

- Next: [UX Philosophy →](02_ux_philosophy.md)
EOF

cat > ./02_ux_philosophy.md << 'EOF'
# UX Philosophy

## Core Principles

### Transparency Over Magic

Users must always understand what `xaft` is doing. The TUI shows:
- The current plan step and why it was chosen
- Every tool call with its arguments
- Every file modification with a live diff
- Every cost incurred
- Every decision pending approval

There are no invisible agent actions.

### Control Is Always Available

The user can:
- Suspend execution at any time: `s`
- Cancel entirely: `Ctrl-C`
- Reject any high-risk tool call: `d`
- Override the plan: `xaft plan --edit`
- Roll back all changes: `xaft rollback {session-id}`

### Local and Trustworthy

- No code is sent to third-party services other than the LLM API
- All edits happen in isolated worktrees, never directly on working files
- Every action is audit-logged locally
- `xaft` can work offline with a local model

### Speed for Expert Users

- Single-command workflow: `xaft run "goal"` does everything
- Auto-approve mode for trusted environments: `--auto-approve medium`
- Keyboard shortcuts for all TUI operations
- Non-interactive mode for CI: `--no-tui --output json`

## Design Anti-Goals

- **Not a chatbot**: `xaft` takes a goal and works toward it. It does not debate the best approach.
- **Not a file watcher**: It's triggered, not ambient.
- **Not deterministic by default**: LLM outputs are inherently stochastic. `xaft` embraces this while providing deterministic structure around it.
- **Not a replacement for understanding**: Users should review changes before merging. `xaft` is a senior engineer collaborator, not an autonomous deployer.

## Example UX Flows

### Happy Path

```
$ xaft run "migrate error handling to anyhow"

Planning...
  ✓ Plan: 7 steps created (OneShotPlanner · Gemini Flash · 0.4s · $0.001)

Executing...
  ✓ Step 1: Index affected files              0.8s  $0.002
  ✓ Step 2: Add anyhow to Cargo.toml         1.2s  $0.006
  ⟳ Step 3: Migrate src/error.rs            ...   ...

[Tab: Output] [Tab: Plan] [Tab: Diff] ...
```

### Approval Flow

```
⚠ Approval Required [HIGH RISK]
  Agent:  code_agent
  Tool:   run_command
  Input:  {"command": "git push origin xaft/task-abc123"}
  Impact: Push branch to remote repository

  [a] Approve   [d] Deny   [e] Edit
  Auto-deny in: 45s ███████████████░░░░░░░
```

### Completion Summary

```
✓ Task Complete

  Goal:    "migrate error handling to anyhow"
  Duration: 4m 32s
  Steps:   7/7 completed
  Files:   12 modified, 0 deleted
  Tests:   cargo test ✓ (all 247 tests pass)
  Cost:    $0.092 / $2.00 budget

  Branch: xaft/task-abc123
  Commit: feat: migrate error handling to anyhow (abc1234)

  Next:
  [p] Create PR   [d] View diff   [r] Rollback   [q] Quit
```

## Accessibility

- All information available as plain text (no TUI-only information)
- Screen reader compatible output via `--no-tui`
- High-contrast theme available
- All interactive elements keyboard-accessible
- Error messages are descriptive and actionable
EOF

echo "UX docs done"