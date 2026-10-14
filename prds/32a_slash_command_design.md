# PRD 32a — Slash Command System: Design & Architecture

## Status: Draft
## Depends on: PRD 09, PRD 10, PRD 11, PRD 26, PRD 28

---

## 1. Problem Statement

xaft's TUI accepts free-text messages that are forwarded directly to the agent loop.
There is no out-of-band channel for the user to control the application itself
(clear transcript, inspect cost, switch model, manage sessions) without those
control messages being interpreted as coding tasks.

Claude Code solves this with a `/` command namespace. xaft needs the same
mechanism — a slash command palette that intercepts `/`-prefixed input before
it reaches the agent, dispatches to built-in handlers, and keeps the agent loop
clean.

---

## 2. Goals

1. Provide ≥ 21 built-in slash commands covering session management, diagnostics,
   memory, git, config, and UX controls.
2. Zero latency for sync commands (no agent round-trip).
3. Inline autocomplete — typing `/` opens a suggestion list; `Tab` completes.
4. All commands emit a typed `SlashCommandExecuted` signal on the `SignalBus`.
5. Unknown commands surface a helpful error and never reach the agent.
6. The system is open for extension: adding a new command is ≤ 20 lines of Rust.

---

## 3. Non-Goals

- Custom user-defined slash commands (deferred to a later PRD).
- Slash commands over the non-TUI `xaft run` CLI path (TUI-only for now).
- Slash commands that require a TUI modal overlay larger than one screen row
  (those are handled by the existing approval/config UI surfaces).

---

## 4. Terminology

| Term | Meaning |
|------|---------|
| **Slash command** | Any input starting with `/` typed in the TUI input bar |
| **Trigger** | The canonical first word, e.g. `/help` |
| **Args** | Everything after the first space, e.g. `claude-3-5-sonnet` in `/model claude-3-5-sonnet` |
| **Handler** | Rust code that runs when the command fires |
| **Registry** | The central lookup table mapping triggers to handlers |
| **Palette** | The inline autocomplete overlay rendered below the input bar |

---

## 5. Architecture Overview

```
 User types: /cle<Tab>
                │
                ▼
  ┌──────────────────────────────────────────┐
  │           InputBar (input_bar.rs)        │
  │  on_char('/') → palette mode ON          │
  │  on_tab()    → complete partial trigger  │
  │  on_enter()  → SlashCommandParser::parse │
  └──────────────────┬───────────────────────┘
                     │ SlashCommand::Clear
                     ▼
  ┌──────────────────────────────────────────┐
  │         SlashCommandRegistry             │
  │  lookup("clear") → ClearHandler         │
  │  handler.execute(ctx) → CommandResult   │
  └────────┬─────────────────────┬──────────┘
           │                     │
   sync result              async result
    (String)             (Future<CommandResult>)
           │                     │
           └──────────┬──────────┘
                      ▼
         AppState::apply_command_result()
           - CommitLine(output)
           - RenderMutation variants
           - SignalBus::emit(SlashCommandExecuted)
```

### 5.1 Module layout

```
crates/xaft-tui/src/
  slash/
    mod.rs          — SlashCommand enum, CommandResult, CommandContext
    parser.rs       — SlashCommandParser::parse(), tab-completion list
    registry.rs     — SlashCommandRegistry, HandlerFn trait
    palette.rs      — Autocomplete overlay state (mirrors MentionAutocomplete)
    commands/
      help.rs
      clear.rs
      compact.rs
      config.rs
      cost.rs
      init.rs
      agents.rs
      mcp.rs
      resume.rs
      rewind.rs
      permissions.rs
      model.rs
      vim.rs         — /vim + /emacs share this file
      theme.rs
      login.rs       — /login + /logout
      doctor.rs
      memory.rs
      diff.rs
      commit.rs
      pr.rs
```

### 5.2 Data flow for a sync command (`/clear`)

1. User presses Enter with `/clear` in the input bar.
2. `AppState::handle_input_submitted` calls `SlashCommandParser::parse("/clear")`.
3. Parser returns `Some(SlashCommand::Clear)`.
4. `AppState` looks up `ClearHandler` in registry.
5. `ClearHandler::execute(ctx)` returns `CommandResult::Lines(vec!["Session cleared."])`.
6. `AppState` pushes `RenderMutation::CommitLine` for each output line.
7. `AppState` emits `SlashCommandExecuted { name: "clear", args: "" }` on `SignalBus`.
8. Input bar is cleared.

### 5.3 Data flow for an async command (`/compact`)

1. Same parse + registry lookup.
2. `CompactHandler::execute_async(ctx)` returns a `BoxFuture<CommandResult>`.
3. `AppState` spawns the future via `tokio::spawn`, stores the `JoinHandle`.
4. A spinner ephemeral line appears while the future runs.
5. When the future resolves, the result is pushed as `CommitLine`s.
Let
---

## 6. Core Types

### 6.1 `SlashCommand` enum

```rust
/// Parsed slash command with any arguments pre-validated.
#[derive(Debug, Clone)]
pub enum SlashCommand {
    Help,
    Clear,
    Compact,
    Config,
    Cost,
    Init,
    Agents,
    Mcp,
    Resume { id: Option<String> },
    Rewind { msg_index: Option<usize> },
    Permissions,
    Model { name: String },
    Vim,
    Emacs,
    Theme { name: Option<String> },
    Login,
    Logout,
    Doctor,
    Memory,
    Diff,
    Commit,
    Pr { title: Option<String> },
}
```

### 6.2 `CommandResult`

```rust
pub enum CommandResult {
    /// Zero or more plaintext lines to print to the transcript.
    Lines(Vec<String>),
    /// Lines with explicit styling (for tables, errors, etc.).
    StyledLines(Vec<StyledLine>),
    /// The command pushed mutations directly; nothing more to do.
    Handled,
    /// The command failed; print an error line.
    Error(String),
}
```

### 6.3 `CommandContext`

```rust
/// Everything a handler may need at execution time.
pub struct CommandContext {
    pub args: String,
    pub signals: Arc<SignalBus>,
    pub config: Arc<XaftConfig>,
    pub session_id: Option<String>,
    pub working_dir: PathBuf,
    pub terminal_cols: u16,
}
```

### 6.4 Handler trait

```rust
#[async_trait]
pub trait SlashHandler: Send + Sync {
    /// Human-readable description shown in /help output.
    fn description(&self) -> &'static str;

    /// Optional argument hint, e.g. `"[session-id]"`.
    fn args_hint(&self) -> Option<&'static str> { None }

    /// True if the handler needs async execution.
    fn is_async(&self) -> bool { false }

    /// Execute synchronously.  Called only when `is_async() == false`.
    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let _ = ctx;
        CommandResult::Error("not implemented".into())
    }

    /// Execute asynchronously.  Called only when `is_async() == true`.
    async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
        let _ = ctx;
        CommandResult::Error("not implemented".into())
    }
}
```

---

## 7. Input Bar Integration

### 7.1 Palette trigger

- When the first character of the input bar is `/` and the buffer contains no
  newline, the TUI enters **palette mode**.
- `AppState.mention_autocomplete` is already a field for `@`-mention autocomplete.
  A parallel `slash_palette: Option<SlashPalette>` field is added.
- `SlashPalette` holds the filtered list of matching command triggers and the
  currently-selected index.

### 7.2 Palette dismissal

The palette dismisses when:
- The user presses `Escape`.
- The user clears the `/` prefix.
- The user presses `Enter` (command fires, palette closes).
- The user types a character that makes the partial match empty (e.g. `/xyz`
  where no command starts with `xyz`).

### 7.3 Key bindings in palette mode

| Key | Action |
|-----|--------|
| `Tab` / `↓` | Select next candidate |
| `Shift+Tab` / `↑` | Select previous candidate |
| `Enter` | Execute selected/typed command |
| `Escape` | Dismiss palette, keep text |
| Any char | Refine filter, re-rank candidates |

### 7.4 History

Slash commands are saved to a per-session command history ring (capacity 50).
`↑`/`↓` in the input bar while in palette mode cycles through this history.
History is **not** persisted across sessions (ephemeral only).

---

## 8. `SlashCommandExecuted` Signal

```rust
/// Emitted on the SignalBus every time a slash command runs.
#[derive(Debug, Clone)]
pub struct SlashCommandExecuted {
    pub name: String,          // "clear"
    pub args: String,          // "" or "claude-3-5-sonnet"
    pub success: bool,
    pub duration_ms: f64,
}
```

This signal allows the TUI event bridge to:
- Log command usage for diagnostics.
- Update the bottom status bar ("Last: /compact").
- Future: analytics / telemetry opt-in.

---

## 9. Error Handling

| Scenario | Behaviour |
|----------|-----------|
| Unknown trigger (`/foo`) | `CommandResult::Error("Unknown command: /foo. Type /help for a list.")` |
| Missing required arg (`/model` with no name) | `CommandResult::Error("/model requires a model name. Usage: /model <name>")` |
| Async handler panics | The join handle error is caught; `CommandResult::Error` is returned |
| Handler returns `Error(msg)` | Rendered as a red `✗ msg` line (same style as tool failures) |

---

## 10. Rendering

### Output lines

All command output is pushed as `RenderMutation::CommitLine` with `LineKind::System`
(dimmed), except:
- Error output → `LineKind::Error` (red `✗` prefix).
- Table headers → `LineKind::AgentText` (white).
- Table rows → `LineKind::System`.

### Separator

Before the first output line, a separator is pushed:

```
  ╌╌ /clear ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
```

This visually separates command output from agent output.

---

## 11. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | Typing `/` in an empty input bar opens the palette within one render frame. |
| AC2 | `Tab` completes `/cle` → `/clear`. |
| AC3 | `Enter` with `/clear` resets the transcript (no agent call). |
| AC4 | `/nonexistent` prints the unknown-command error line and does NOT emit a `RunRequest`. |
| AC5 | Every executed command emits `SlashCommandExecuted` on `SignalBus`. |
| AC6 | `/help` lists all 21 commands with their descriptions in alphabetical order. |
| AC7 | `/cost` renders a table with at least `agent`, `input_tokens`, `output_tokens`, `cost_usd` columns. |
| AC8 | Async commands show a spinner ephemeral line while running. |
| AC9 | Palette disappears after command execution. |
| AC10 | Slash command history is accessible via `↑` / `↓` within one session. |
