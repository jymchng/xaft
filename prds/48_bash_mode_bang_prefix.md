# PRD 48 — Bash Mode (`!` Prefix)

## Status: Draft
## Depends on: PRD 09, PRD 10 (TUI architecture), PRD 33c (permission cycling)

---

## 1. Problem

xaft's TUI input bar currently accepts two kinds of input:
- **Agent tasks** — free-text sent to the LLM pipeline.
- **Slash commands** — `/clear`, `/cost`, etc., interpreted by `SlashCommandRegistry`.

There is no way to run a shell command directly from the input bar without going
through the LLM. Common developer operations — checking `git status`, grepping a
file, running a quick test — require either spawning a separate terminal or starting
a full agent run just to execute one command.

Claude Code solves this with the `!` prefix:

```
❯ !git status
```

The command runs immediately in the host shell, output streams line-by-line into
the TUI transcript, and the LLM is never involved. This is F4 ("Bash Mode") in the
xaft feature roadmap.

Without this, users switch context constantly. With it, the TUI becomes a complete
developer workspace — both an agent front-end and an interactive shell.

---

## 2. Goals

1. Any input starting with `!` (after optional leading whitespace) is treated as a
   shell command, not an agent task.
2. The command executes in the configured `Sandbox` (same sandbox used by
   `BashExecTool`) with the session's `working_dir` as the CWD.
3. Output streams **line-by-line** into the TUI transcript as it arrives, with
   no buffering until the command exits.
4. `stdout` lines render with a `!` prefix in normal text; `stderr` lines render
   with a `!` prefix in warning/dim colour.
5. On completion, a summary line shows:
   - Exit 0: `  ✓ exit=0  (0.42s)`  — success colour.
   - Exit ≠ 0: `  ✗ exit=1  (0.11s)` — error colour.
6. The approval gate (`TuiApprovalGate` / permission mode from PRD 33c) is
   consulted before execution:
   - `Normal` mode: every `!<cmd>` shows `  ⚠ Run shell command? [a]yes  [r]no`.
   - `AutoApprove` mode: low-risk reads (non-destructive) run without prompting;
     destructive commands (rm, kill, etc.) still prompt.
   - `Bypass` mode: all commands execute without prompting.
7. The execution policy (`ExecutionPolicy`) is the same instance used by the agent's
   `BashExecTool` so deny-listed patterns apply consistently.
8. Commands appear in the session transcript with role `User` (the `!<cmd>` line)
   and role `Tool` (the captured output), matching the existing tool-output format.
9. While a `!<cmd>` is running, the input bar is locked (same as during agent
   execution). `Ctrl+C` sends SIGINT to the child process and surfaces the exit
   code. A second `Ctrl+C` force-kills.
10. `!<cmd>` is orthogonal to background mode (PRD 33d): it always runs in the
    foreground; `Ctrl+B` during a `!<cmd>` is a no-op.
11. Timeout: commands that exceed `ShellToolConfig::timeout_secs` (default 300s) are
    killed and show `  ✗ timed out  (300.0s)`.

---

## 3. Non-Goals

- Multi-line `!` commands (no heredoc / continuation).
- Piping between `!<cmd>` and the agent (`!grep … | agent`).
- Persistent shell state across `!` commands (each invocation is a fresh `bash -c`).
- Interactive commands that require terminal input (e.g., `!vim`, `!ssh`). These
  produce an error line: `  ✗ interactive command not supported in bash mode`.
- Storing `!<cmd>` output in the LLM conversation history as tool-use messages.
  Output is transcript-only unless the user re-submits it via `/compact` or an agent task.
- `/!` (slash then bang) — this is not a valid slash command and falls through to agent.

---

## 4. Architecture

### 4.1 Input Detection

`AppState::handle_submit` currently intercepts `/`-prefixed input before it reaches the
agent. The detection for `!` is inserted immediately before the slash-command check:

```rust
pub fn handle_submit(&mut self, msg: String) {
    let trimmed = msg.trim();

    // ── Bash mode prefix ──────────────────────────────────────────────────
    if let Some(cmd) = trimmed.strip_prefix('!') {
        self.handle_bash_mode(cmd.trim().to_string());
        return;
    }

    // ── Slash command interception ────────────────────────────────────────
    if trimmed.starts_with('/') && !trimmed.contains('\n') { … }

    // … agent path …
}
```

Bare `!` (nothing after the bang) returns a usage error:
```
  ✗ bash mode: command required (e.g. !git status)
```

### 4.2 `BashModeHandler`

A new module `crates/xaft-tui/src/bash_mode.rs` owns the execution logic.

```rust
/// Drives a single `!<cmd>` execution from the TUI.
pub struct BashModeHandler {
    executor: Arc<CommandExecutor>,
    working_dir: PathBuf,
    policy: ExecutionPolicy,
}

impl BashModeHandler {
    pub fn new(executor: Arc<CommandExecutor>, working_dir: PathBuf) -> Self { … }
}
```

`AppState` holds a `bash_mode_handler: Option<Arc<BashModeHandler>>`.
`TuiApp::run_inner` constructs it from the same `CommandExecutor` instance
used for the agent's `BashExecTool` and stores it via `AppState::set_bash_mode(…)`.

### 4.3 Execution flow

Because `execute_streaming` returns a `Stream<Item = CommandEvent>`, and `AppState`
must remain single-threaded, bash-mode execution is **spawned as a tokio task** that
forwards `CommandEvent`s through the existing `TuiEvent` channel:

```
handle_submit("!git status")
    │
    ▼
AppState::handle_bash_mode(cmd)
    ├── permission check (approval_queue / permission_mode)
    ├── emit TuiEvent::BashModeStarted { cmd }
    └── tokio::spawn → bash_mode_task(executor, cmd, event_tx, cancel_token)
            │
            for each CommandEvent:
            ├── Stdout { line }  → event_tx.send(TuiEvent::BashModeLine { line, is_stderr: false })
            ├── Stderr { line }  → event_tx.send(TuiEvent::BashModeLine { line, is_stderr: true })
            ├── Exit { code, duration } → event_tx.send(TuiEvent::BashModeExit { code, duration })
            └── Error { message }       → event_tx.send(TuiEvent::BashModeError { message })
```

The `CancellationToken` passed to the task is stored in `AppState::bash_cancel`:
```rust
pub bash_cancel: Option<CancellationToken>,
```
Pressing `Ctrl+C` once calls `bash_cancel.cancel()`, which sends SIGINT to the child
process (handled inside `agtrs_shell::executor::spawn_streaming`). Pressing `Ctrl+C`
a second time calls `cancel.cancel()` again, which force-kills any lingering process.

### 4.4 New `TuiEvent` variants

```rust
pub enum TuiEvent {
    // … existing variants …

    /// The `!<cmd>` is about to start (printed as the user-message line).
    BashModeStarted {
        command: String,
    },

    /// A line of stdout or stderr from the running `!<cmd>`.
    BashModeLine {
        line: String,
        is_stderr: bool,
    },

    /// The `!<cmd>` exited.
    BashModeExit {
        code: i32,
        duration: std::time::Duration,
    },

    /// An internal error prevented execution (policy violation, spawn failure, etc.).
    BashModeError {
        message: String,
    },
}
```

### 4.5 `AppState::handle_event` — rendering

```rust
TuiEvent::BashModeStarted { command } => {
    // Echo the command as a user-message line (same as ❯ text for agent tasks).
    self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
        format!("❯ !{command}"),
        LineKind::UserMessage,
    )));
    self.phase = WorkflowPhase::BashRunning;
    self.bash_running = true;
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
}

TuiEvent::BashModeLine { line, is_stderr } => {
    let kind = if is_stderr { LineKind::BashStderr } else { LineKind::BashStdout };
    self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
        format!("  !  {line}"),
        kind,
    )));
}

TuiEvent::BashModeExit { code, duration } => {
    self.bash_running = false;
    self.bash_cancel = None;
    self.phase = WorkflowPhase::Idle;
    let dur_str = format_duration_secs(duration);
    if code == 0 {
        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
            format!("  ✓ exit=0  ({dur_str})"),
            LineKind::Success,
        )));
    } else {
        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
            format!("  ✗ exit={code}  ({dur_str})"),
            LineKind::Error,
        )));
    }
    self.mutations.push(RenderMutation::SetEphemeral(None));
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
}

TuiEvent::BashModeError { message } => {
    self.bash_running = false;
    self.bash_cancel = None;
    self.phase = WorkflowPhase::Idle;
    self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
        format!("  ✗ {message}"),
        LineKind::Error,
    )));
    self.mutations.push(RenderMutation::SetEphemeral(None));
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
}
```

### 4.6 `WorkflowPhase` extension

```rust
pub enum WorkflowPhase {
    Idle,
    Planning,
    Coding,
    QaReview,
    Fixing,
    Done,
    Error,
    BashRunning,   // ← new: a !<cmd> is executing
}

impl WorkflowPhase {
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Idle | Self::Done | Self::Error)
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::BashRunning => "Shell",
            // … other arms unchanged …
        }
    }
}
```

`BashRunning.is_active() == true`, so:
- The input bar is locked (no new task/command accepted).
- `Ctrl+B` (background, PRD 33d) is a no-op while `BashRunning`.
- The ephemeral spinner shows `  Shell  !<truncated-cmd>  Ns`.

### 4.7 `AppState` new fields

```rust
/// True while a `!<cmd>` is executing.
pub bash_running: bool,
/// Cancellation token for the currently running `!<cmd>`.
pub bash_cancel: Option<CancellationToken>,
/// The handler used to validate and spawn `!<cmd>` invocations.
/// `None` until `app.rs` wires it after bootstrapping the runtime.
pub bash_mode: Option<Arc<BashModeHandler>>,
```

### 4.8 Approval gate interaction

Before spawning the task, `handle_bash_mode` consults `self.approval_queue` and the
current `permission_mode` (when PRD 33c is implemented) using `RiskLevel::from_bash_command`:

```rust
fn handle_bash_mode(&mut self, cmd: String) {
    let Some(ref handler) = self.bash_mode else {
        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
            "  ✗ bash mode not available (no working directory configured)".into(),
            LineKind::Error,
        )));
        return;
    };

    // Interactive-command guard.
    if looks_interactive(&cmd) {
        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
            "  ✗ interactive command not supported in bash mode".into(),
            LineKind::Error,
        )));
        return;
    }

    // Policy check — runs synchronously (policy never blocks).
    if let Err(e) = handler.policy_check(&cmd) {
        self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
            format!("  ✗ {e}"),
            LineKind::Error,
        )));
        return;
    }

    // Approval gate: respect current permission mode.
    let risk = RiskLevel::from_bash_command(&cmd);
    let needs_prompt = match self.permission_mode() {
        PermissionMode::Bypass => false,
        PermissionMode::AutoApprove => risk >= RiskLevel::High,
        PermissionMode::Normal => true,
    };

    if needs_prompt {
        // Defer to the approval queue — same flow as ToolPendingApproval.
        // BashModeStarted fires only after the user approves.
        let result = self.approval_queue.push_bash(cmd.clone());
        match result {
            ApprovalStatus::AutoApproved => { /* fall through to spawn */ }
            ApprovalStatus::Pending => {
                self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
                    format!("  ⚠ Run shell command? [a]yes  [r]no\n  !  {cmd}"),
                    LineKind::System,
                )));
                self.pending_bash_cmd = Some(cmd);
                return;
            }
        }
    }

    self.spawn_bash_mode(cmd, handler.clone());
}
```

`spawn_bash_mode` creates the `CancellationToken`, stores it in `self.bash_cancel`,
emits `TuiEvent::BashModeStarted`, and spawns the streaming task.

`pending_bash_cmd: Option<String>` stores the deferred command. When the user presses
`a` / `y` in the approval handler, `AppState::resolve_bash_approval` is called which
fires `spawn_bash_mode`.

### 4.9 `RiskLevel::from_bash_command`

```rust
impl RiskLevel {
    /// Classify a raw shell command for the approval gate.
    ///
    /// Heuristic pattern-matching on the command string:
    /// - `Critical`: `sudo`, `chmod 777`, `dd if=`, `mkfs`
    /// - `High`:     `rm`, `mv`, `kill`, `pkill`, `truncate`, `git push --force`
    /// - `Medium`:   `git commit`, `git push`, `cargo install`, `npm install`, `pip install`
    /// - `Low`:      everything else (read-only: `ls`, `cat`, `grep`, `git status`, etc.)
    pub fn from_bash_command(cmd: &str) -> Self { … }
}
```

This ensures the same risk taxonomy as tool calls. The patterns are conservative:
when in doubt, classify higher. The user can override with `Bypass` mode.

### 4.10 Transcript recording

`TuiEvent::BashModeStarted` stores the command in `AppState::last_bash_command`.
After `BashModeExit`, if the session has a `ConversationStore`, a synthetic
`Message::user` + `Message::tool_result` pair is appended so the agent can see the
output on the next task. This is opt-in via `[tui] record_bash_in_conversation =
true` (default `false`) — enabling it means the LLM sees every `!<cmd>` and its
output, which may be useful or noisy depending on workflow.

### 4.11 `looks_interactive(cmd) → bool`

A command is considered interactive if it appears to require terminal input that
`bash -c` cannot provide in a non-TTY context:

```rust
fn looks_interactive(cmd: &str) -> bool {
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    matches!(
        first_word,
        "vim" | "vi" | "nano" | "emacs" | "less" | "more" | "top" | "htop"
        | "ssh" | "telnet" | "ftp" | "sftp" | "mysql" | "psql" | "sqlite3"
        | "python" | "python3" | "node" | "irb" | "iex" | "ghci"
        | "ipython" | "julia"
    )
}
```

This is a best-effort heuristic. Users who know what they're doing can pipe input
explicitly (`echo "exit" | python`) — that is not caught as interactive.

### 4.12 Long-output handling

Commands producing more than `ShellToolConfig::max_output_lines` (default 1000)
lines are truncated in the transcript. Once the cap is reached:
- Further `BashModeLine` events are counted but not pushed to `mutations`.
- On `BashModeExit`, an additional line is appended:
  `  … N more lines omitted (output exceeded limit) …` in dim colour.

The full output is still captured (not truncated at the child-process level) so
tools downstream can consume it; only the TUI rendering is capped.

### 4.13 Ephemeral spinner

The ephemeral line while `BashRunning` (contributed by `ephemeral::build_ephemeral`):

```
  Shell  !git log --oneline  ·  2s  [Ctrl+C to cancel]
```

The command is truncated to `terminal_cols - 30` chars with `…` suffix. The spinner
is the standard `⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` animation at 10fps.

---

## 5. Visual Format

### Normal command (non-destructive read)

```
❯ !git log --oneline -5
  !  a1b2c3d fix auth token refresh
  !  d4e5f6a add rate limit headers
  !  7890abc initial commit
  ✓ exit=0  (0.08s)
```

`!` prefix lines use `LineKind::BashStdout` (white text, `!` in dim grey).

### Command with stderr

```
❯ !cargo check
  !  warning: unused variable `x`
  !   --> src/main.rs:5:9
  !  error[E0308]: mismatched types
  ✗ exit=1  (1.42s)
```

`stderr` lines use `LineKind::BashStderr` (amber/dim, matching the warning style).

### Destructive command with approval prompt (Normal mode)

```
❯ !rm -rf ./tmp
  ⚠ Run shell command? [a]yes  [r]no
  !  rm -rf ./tmp
```

After the user presses `a`:
```
  ✓ Approved
❯ !rm -rf ./tmp
  ✓ exit=0  (0.01s)
```

### Policy-blocked command

```
❯ !rm -rf /
  ✗ policy violation: destructive command blocked (matches deny pattern `rm -rf /`)
```

### Timeout

```
❯ !sleep 9999
  … command timed out after 300s …
  ✗ timed out  (300.0s)
```

### Ctrl+C cancel

```
❯ !find / -name "*.rs"
  !  /usr/lib/…
  !  /usr/local/…
  ^C
  ✗ interrupted by user  (1.2s)
```

---

## 6. `LineKind` additions

```rust
pub enum LineKind {
    // … existing variants …
    /// A line of stdout from a `!<cmd>` bash-mode invocation.
    BashStdout,
    /// A line of stderr from a `!<cmd>` bash-mode invocation.
    BashStderr,
}
```

Both render with an `!` prefix glyph. `BashStdout` uses the default foreground
colour; `BashStderr` uses the theme's warning/amber colour (same as `LineKind::Error`
but dimmer — it's not always an error).

---

## 7. `XaftConfig` knobs

```toml
[tui]
# Maximum lines of !<cmd> output to display in the TUI transcript.
# Output beyond this is omitted with a "N more lines" notice.
bash_mode_max_lines = 1000

# Default timeout in seconds for !<cmd> invocations.
# Overrides [shell.timeout_secs] for bash-mode specifically.
# Set to 0 to disable timeout (not recommended).
bash_mode_timeout_secs = 300

# When true, !<cmd> output is appended to the LLM conversation as a
# tool_result message so the agent sees it on the next task.
bash_mode_record_in_conversation = false
```

These fields are added to `TuiConfig` in `crates/xaft-config/src/types.rs`
with corresponding defaults in `crates/xaft-config/src/defaults.rs`.

---

## 8. Signals

```rust
/// Emitted when a `!<cmd>` bash-mode invocation starts.
#[derive(Debug, Clone)]
pub struct XaftBashModeStarted {
    /// The raw command string (after the `!`).
    pub command: String,
}

/// Emitted when a `!<cmd>` completes.
#[derive(Debug, Clone)]
pub struct XaftBashModeCompleted {
    /// The raw command string.
    pub command: String,
    /// Process exit code.
    pub exit_code: i32,
    /// Wall-clock duration.
    pub duration_ms: f64,
    /// Number of stdout lines produced.
    pub stdout_lines: usize,
    /// Number of stderr lines produced.
    pub stderr_lines: usize,
    /// `true` if the command was killed by the user (`Ctrl+C`) or timed out.
    pub interrupted: bool,
}
```

---

## 9. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/bash_mode.rs` | **New** — `BashModeHandler`, `looks_interactive`, task spawner |
| `crates/xaft-tui/src/state.rs` | `!` prefix detection in `handle_submit`; new event arms; `BashRunning` phase; new fields; `pending_bash_cmd`; `resolve_bash_approval` |
| `crates/xaft-tui/src/bridge.rs` | `BashModeStarted`, `BashModeLine`, `BashModeExit`, `BashModeError` variants in `TuiEvent` |
| `crates/xaft-tui/src/transcript.rs` | `BashStdout`, `BashStderr` variants in `LineKind` |
| `crates/xaft-tui/src/ephemeral.rs` | Handle `BashRunning` phase in spinner builder |
| `crates/xaft-tui/src/approval.rs` | `RiskLevel::from_bash_command`; `ApprovalQueue::push_bash` |
| `crates/xaft-tui/src/app.rs` | Construct `BashModeHandler`; wire into `AppState`; export `bash_cancel` on quit |
| `crates/xaft-config/src/types.rs` | `bash_mode_max_lines`, `bash_mode_timeout_secs`, `bash_mode_record_in_conversation` on `TuiConfig` |
| `crates/xaft-config/src/defaults.rs` | Defaults for above fields |
| `crates/xaft-agent/src/signals.rs` | `XaftBashModeStarted`, `XaftBashModeCompleted` |
| `crates/xaft-agent/src/lib.rs` | Re-export new signals |
| `crates/xaft-tui/tests/bash_mode_integration.rs` | **New** — integration tests |

---

## 10. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | `!git status` runs and renders output without an LLM call. Phase never enters Planning. |
| AC2 | Output streams line-by-line; each stdout line is committed as a `CommitLine` event before the command exits. |
| AC3 | Exit 0 renders `✓ exit=0` in success colour; exit ≠ 0 renders `✗ exit=N` in error colour. |
| AC4 | `stderr` lines render in amber/dim colour distinct from stdout. |
| AC5 | In `Normal` permission mode, `!rm -rf ./tmp` shows the approval prompt before executing. |
| AC6 | In `Bypass` mode, `!rm -rf ./tmp` runs without prompting. |
| AC7 | Policy-blocked commands (deny pattern match) produce an error line and do NOT execute. |
| AC8 | `Ctrl+C` during `!<cmd>` cancels the child process and shows `✗ interrupted by user`. |
| AC9 | Commands exceeding `bash_mode_timeout_secs` are killed and show `✗ timed out`. |
| AC10 | Commands exceeding `bash_mode_max_lines` show `… N more lines omitted …` and still complete. |
| AC11 | Interactive commands (e.g., `!vim`) are rejected before spawning. |
| AC12 | Bare `!` (no command) shows a usage error. |
| AC13 | While `!<cmd>` is running, the input bar is locked; a new task cannot be submitted. |
| AC14 | `XaftBashModeStarted` and `XaftBashModeCompleted` signals are emitted. |
| AC15 | `!<cmd>` output is NOT appended to the LLM conversation when `bash_mode_record_in_conversation = false` (default). |

---

## 11. Tests

### `crates/xaft-tui/tests/bash_mode_integration.rs`

```rust
#[tokio::test]
async fn bash_mode_runs_command_and_captures_stdout() {
    // Inject BashModeStarted + BashModeLine(stdout) + BashModeExit(0) events.
    // Assert transcript contains "!  <line>" and "✓ exit=0".
    // Assert phase returns to Idle.
    // Assert no LLM call was made (mock runtime remains idle).
}

#[tokio::test]
async fn bash_mode_streams_output_before_exit() {
    // Inject 3 BashModeLine events followed by BashModeExit.
    // Assert all 3 lines are committed before the exit line.
    // Assert line order is preserved.
}

#[tokio::test]
async fn bash_mode_nonzero_exit_renders_error_colour() {
    // Inject BashModeExit { code: 127, duration: 50ms }.
    // Assert "✗ exit=127" appears in mutations with LineKind::Error.
}

#[tokio::test]
async fn bash_mode_stderr_uses_bash_stderr_kind() {
    // Inject BashModeLine { is_stderr: true }.
    // Assert the committed StyledLine has LineKind::BashStderr.
}

#[tokio::test]
async fn bash_mode_blocks_input_while_running() {
    // Set phase = BashRunning.
    // Assert accepts_new_task() == false.
    // Assert Ctrl+B key press is a no-op (bg_mode stays false).
}

#[tokio::test]
async fn bash_mode_ctrl_c_cancels() {
    // Inject BashModeStarted.
    // Assert bash_cancel.is_some().
    // Press Ctrl+C.
    // Assert bash_cancel is cancelled.
    // Inject BashModeError { message: "interrupted by user" }.
    // Assert phase returns to Idle.
}

#[tokio::test]
async fn bash_mode_respects_sandbox_policy() {
    // BashModeHandler with ExecutionPolicy::default() (strict).
    // Call handle_bash_mode("rm -rf /").
    // Assert error CommitLine present, BashModeStarted NOT emitted.
}

#[tokio::test]
async fn bash_mode_approval_prompt_in_normal_mode() {
    // permission_mode = Normal, cmd = "rm -rf ./tmp" (High risk).
    // Assert approval prompt line in mutations.
    // Assert pending_bash_cmd = Some("rm -rf ./tmp").
    // Assert bash_running = false (not yet executing).
}

#[tokio::test]
async fn bash_mode_bypass_skips_approval() {
    // permission_mode = Bypass, cmd = "rm -rf ./tmp".
    // Assert BashModeStarted event fired (task spawned immediately).
    // Assert no approval prompt in mutations.
}

#[tokio::test]
async fn bash_mode_interactive_command_rejected() {
    // handle_bash_mode("vim src/main.rs").
    // Assert error CommitLine containing "interactive command not supported".
    // Assert phase stays Idle.
}

#[tokio::test]
async fn bash_mode_bare_bang_returns_usage_error() {
    // Simulate submit of "!".
    // Assert error line with usage hint.
}

#[tokio::test]
async fn bash_mode_output_truncated_at_max_lines() {
    // Set bash_mode_max_lines = 5.
    // Inject 10 BashModeLine stdout events.
    // Assert only 5 BashStdout CommitLines in mutations.
    // Assert one "N more lines omitted" line present.
}

#[tokio::test]
async fn bash_mode_does_not_append_to_conversation_by_default() {
    // bash_mode_record_in_conversation = false.
    // Run a bash command.
    // Assert conversation_store receives no new messages.
}

#[tokio::test]
async fn bash_mode_signals_emitted() {
    // Subscribe to XaftBashModeStarted and XaftBashModeCompleted.
    // Run a command.
    // Assert both signals received with correct command / exit_code fields.
}
```

### Unit tests in `crates/xaft-tui/src/bash_mode.rs`

```rust
#[test]
fn looks_interactive_detects_vim() {
    assert!(looks_interactive("vim src/main.rs"));
    assert!(looks_interactive("python"));
    assert!(!looks_interactive("python src/main.py"));  // arg provided → non-interactive
    assert!(!looks_interactive("git status"));
}

#[test]
fn risk_level_from_bash_command_classifies_correctly() {
    assert_eq!(RiskLevel::from_bash_command("ls -la"), RiskLevel::Low);
    assert_eq!(RiskLevel::from_bash_command("git status"), RiskLevel::Low);
    assert_eq!(RiskLevel::from_bash_command("rm -rf ./tmp"), RiskLevel::High);
    assert_eq!(RiskLevel::from_bash_command("sudo rm"), RiskLevel::Critical);
    assert_eq!(RiskLevel::from_bash_command("git push"), RiskLevel::Medium);
}

#[test]
fn format_duration_secs_renders_correctly() {
    assert_eq!(format_duration_secs(Duration::from_millis(420)), "0.42s");
    assert_eq!(format_duration_secs(Duration::from_secs(5)), "5.00s");
    assert_eq!(format_duration_secs(Duration::from_secs(300)), "300.0s");
}
```

---

## 12. Edge cases

| Case | Behaviour |
|------|-----------|
| `!` with only whitespace (`!   `) | Treated as bare `!` — usage error. |
| Command produces no output | Only `✓ exit=0 (Ns)` line rendered. |
| Command outputs NUL bytes | NUL stripped from lines before rendering (avoids terminal corruption). |
| Command produces very long single line | Line hard-wrapped at `terminal_cols - 5` chars with `↩` continuation glyph. |
| Command with `\r` in output (Windows-style) | `\r` stripped before rendering. |
| Policy check throws unexpectedly | Caught, rendered as `✗ policy check failed: <err>`. |
| `bash_mode` is `None` when `!<cmd>` submitted | Error: `bash mode not available`. Cannot happen in normal flow. |
| `!<cmd>` during background pipeline (PRD 33d) | Allowed — bg pipeline continues buffering; `!<cmd>` runs in foreground. |
| TUI quit while `!<cmd>` running | `bash_cancel.cancel()` called; child killed; no wait for graceful exit (same as runtime abort). |
| `!<cmd>` in headless mode | Headless entry point does not construct `BashModeHandler`; `!` prefix falls through to agent. |
| Sandbox root changed mid-session | Sandbox is created once at startup from `working_dir`. Subsequent `!cd /other` changes the child's CWD but not the sandbox — sandbox still enforces path restrictions on the *next* command. |

---

## 13. Security invariants

1. **Path confinement**: the `Sandbox` is constructed with `working_dir` as root.
   Commands that attempt to escape (e.g., `!cat /etc/passwd`) are either blocked by
   `ExecutionPolicy` patterns or produce no side-effects within the project tree.
   Note: `Sandbox` enforces CWD and environment; it does not chroot. Destructive
   reads of files outside `working_dir` are not prevented — that is a known
   limitation documented to the user.
2. **No implicit sudo**: `sudo` is classified `Critical` and requires approval in
   `Normal`/`AutoApprove` modes. In `Bypass` mode, `sudo` executes if the
   system permits it — the user has explicitly opted in.
3. **Denial list**: `ExecutionPolicy::deny_patterns` applies to `!<cmd>` exactly as
   it does to `bash_exec` tool calls. The same list is enforced; bash mode is not
   a bypass.
4. **Signal isolation**: `XaftBashModeStarted` / `XaftBashModeCompleted` are
   observational signals. They do not gate execution. An observer cannot cancel or
   modify the execution by dropping or ignoring the signal.

---

## 14. Interaction matrix

| Scenario | Behaviour |
|----------|-----------|
| `!<cmd>` + agent task running | Input bar is locked during agent run — `!<cmd>` cannot be submitted until agent is done or backgrounded (PRD 33d). |
| `!<cmd>` + background pipeline running | Allowed. `!<cmd>` runs in foreground; bg pipeline buffers separately. |
| `!<cmd>` + approval pending | Input bar locked by approval prompt; `!<cmd>` is NOT possible until resolved. |
| `Shift+Tab` (permission mode cycle, PRD 33c) during `!<cmd>` | Mode change takes effect for the *next* `!<cmd>`; the running command is unaffected. |
| `/clear` (PRD 35) during `!<cmd>` | Not reachable — input bar is locked during `BashRunning`. |
| `Ctrl+B` (PRD 33d) during `!<cmd>` | No-op. Bash mode always runs foreground. |
| `BashRunning` + `Ctrl+C` | First press: cancel token fired → SIGINT to child. Second press: force-kill. Third press (the usual double-Ctrl+C quit path): `should_quit = true`. |
