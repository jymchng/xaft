---
title: "PRD-66: Mode UI — Shift+Tab Cycling, Badge, Footer, /mode Command"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on: 64_mode_system_architecture.md, 65_builtin_modes.md
analog: python/agenthicc/prds/prd-49-mode-ui.md
---

# PRD-66: Mode UI — Shift+Tab Cycling, Badge, Footer, /mode Command

## Overview

This PRD describes every user-visible Mode UI surface: the permanent mode footer below the prompt, the mode badge left of the `❯` glyph, the mode badge in the ephemeral status line, the one-shot switch notification, and the `/mode` slash command. It also defines the Shift+Tab key binding that cycles through modes.

A Rust engineer who has read PRDs 64 and 65 must be able to implement the full UI from this PRD alone without looking at the agenthicc source.

---

## 1. Key binding — Shift+Tab cycles modes

### Where in `handle_key()`

`AppState::handle_key()` (in `crates/xaft-tui/src/state.rs`) dispatches keys in this priority order, highest to lowest:

1. **Menu driver** (`self.menu_driver.is_active()`) — captures all keys.
2. **Escape-confirmation dialog** (`self.escape_dialog.is_some()`) — modal.
3. **Ctrl+B** — background pipeline detach/re-attach.
4. **Background pipeline digit selection** (`self.awaiting_bg_select`).
5. **InputBar** (`self.input_bar.handle_key(key)`) — printable chars, editing.
6. **Fallthrough `match key.code`** — Ctrl+C, Ctrl+Q, etc.

The Shift+Tab handler must be inserted **between step 4 and step 5** — after the escape-dialog and Ctrl+B checks, but before the `InputBar` consumes the keystroke. `InputBar::handle_key` does not handle `KeyCode::BackTab`, so inserting it at step 5 would also work, but it is cleaner to insert it as an explicit early-return before the `InputBar` call, in the same style as the `Ctrl+B` block.

### Exact placement (between steps 4 and 5)

```rust
// ── Shift+Tab → cycle mode ────────────────────────────────────────────────
if key.code == KeyCode::BackTab {
    let new_mode = self.mode_manager.cycle();
    self.mode_notification = Some(format!("✦ Switched to {} mode", new_mode.name));
    // Emit XaftModeChanged on the signal bus.
    if let Some(ref bus) = self.signal_bus {
        let sig = XaftModeChanged {
            from_name: self.mode_manager.previous_name().to_string(),
            to_name: new_mode.name.clone(),
            to_label: new_mode.label.clone(),
            to_colour: new_mode.colour,
        };
        emit_signal_if_bus(Some(Arc::clone(bus)), sig);
    }
    self.mutations
        .push(RenderMutation::UpdatePrompt(build_prompt(self)));
    return;
}
```

Key notes:
- `KeyCode::BackTab` — crossterm maps `\x1b[Z` (the terminal escape for Shift+Tab) to this variant. No modifier check is needed; the modifiers field is `KeyModifiers::SHIFT` on most terminals but `KeyModifiers::empty()` on others. Match on `KeyCode::BackTab` alone.
- `self.mode_manager.cycle()` advances the mode ring by one and returns the newly active `&AgentMode`. See PRD 64 for `ModeManager`.
- `self.mode_notification` is cleared after the next `build_prompt` call (see §3).
- `emit_signal_if_bus` is the existing helper in `state.rs` that falls back to a single-threaded runtime when there is no tokio context (test safety).
- The `UpdatePrompt` mutation must be pushed so the renderer redraws the badge, footer, and caret position before the next keypress.

---

## 2. `PromptState` additions

Add two fields to `PromptState` in `crates/xaft-tui/src/prompt.rs`:

```rust
/// Badge string for the active mode (ANSI-colored), or `None` in Auto mode.
/// Rendered left of the `❯` prompt marker.
/// Example value: `"\x1b[33m[PLAN]\x1b[0m"` (yellow brackets + label).
pub mode_badge: Option<String>,

/// Permanent footer text rendered below the prompt block on every frame.
/// Contains either a one-shot switch notification ("✦ Switched to PLAN mode")
/// or the standing help text ("⏵⏵ Auto  (shift+tab to cycle)").
pub mode_footer: String,
```

`PromptState` derives `Default`; both fields should default to `None` and `String::new()` respectively, consistent with Auto-mode behaviour at startup before `build_prompt` is called for the first time.

```rust
impl Default for PromptState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            cursor: Cursor::default(),
            scroll_top: 0,
            agent_active: false,
            is_empty: true,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: "⏵⏵ Auto  (shift+tab to cycle)".into(),
        }
    }
}
```

### Updates to `build_prompt()`

Add the following block at the end of `build_prompt(state: &AppState)`, immediately before constructing `PromptState { … }`:

```rust
// ── Mode fields ───────────────────────────────────────────────────────────
let mode = state.mode_manager.active();
let mode_badge = if mode.name == "Auto" {
    None
} else {
    Some(mode.ansi_badge())
};
let mode_footer = state.mode_notification.take_ref().cloned().unwrap_or_else(|| {
    if mode.name == "Auto" {
        "⏵⏵ Auto  (shift+tab to cycle)".into()
    } else {
        let desc_preview = mode.description.char_indices()
            .nth(50)
            .map(|(i, _)| &mode.description[..i])
            .unwrap_or(&mode.description);
        format!(
            "⏵⏵ [{}] {} — {}  (shift+tab to cycle)",
            mode.label,
            mode.label,
            desc_preview,
        )
    }
});
```

Because `build_prompt` takes `&AppState` (immutable), clearing `mode_notification` cannot happen inside it. Instead, `AppState` exposes a read-and-clear helper:

```rust
impl AppState {
    /// Return the current `mode_notification` and clear it.
    ///
    /// Called exactly once per `build_prompt` invocation to produce the
    /// one-shot footer text. After the first call, subsequent `build_prompt`
    /// calls within the same frame see `None` and fall through to the
    /// standing footer text.
    pub fn take_mode_notification(&mut self) -> Option<String> {
        self.mode_notification.take()
    }
}
```

Adjust the signature of `build_prompt` to take `&mut AppState`:

```rust
pub fn build_prompt(state: &mut AppState) -> PromptState {
    // ... existing scroll/cursor logic unchanged ...

    let mode = state.mode_manager.active().clone();
    let mode_badge = if mode.name == "Auto" {
        None
    } else {
        Some(mode.ansi_badge())
    };
    let mode_footer = state.take_mode_notification().unwrap_or_else(|| {
        if mode.name == "Auto" {
            "⏵⏵ Auto  (shift+tab to cycle)".into()
        } else {
            let desc_preview = {
                let d = &mode.description;
                let boundary = d.char_indices()
                    .nth(50)
                    .map(|(i, _)| i)
                    .unwrap_or(d.len());
                &d[..boundary]
            };
            format!(
                "⏵⏵ [{}] {} — {}  (shift+tab to cycle)",
                mode.label, mode.label, desc_preview,
            )
        }
    });

    PromptState {
        lines: lines.to_vec(),
        cursor: state.input_bar.cursor(),
        scroll_top,
        agent_active: state.phase.is_active(),
        is_empty: state.input_bar.is_empty(),
        hidden_above,
        hidden_below,
        active_trigger,
        menu_active: state.menu_driver.is_active(),
        mode_badge,
        mode_footer,
    }
}
```

All existing call-sites of `build_prompt(self)` inside `state.rs` already have `&mut self` context, so the signature change is safe. The only call site in the test module uses `build_prompt(&mut s)` — update that too.

### `AppState` field

Add to the `AppState` struct definition (group with related one-shot fields, e.g. near `error_message`):

```rust
/// One-shot notification text for a mode switch, shown in the prompt footer
/// for exactly one frame. `build_prompt` calls `take_mode_notification()`
/// which consumes this field.
pub mode_notification: Option<String>,
```

Initialise to `None` in `AppState::new()`.

---

## 3. `AgentMode::ansi_badge()` method

Add to the `impl AgentMode` block in `crates/xaft-tui/src/mode/` (wherever PRD 64 defines `AgentMode`):

```rust
impl AgentMode {
    /// Return an ANSI-colored badge string: `"[LABEL]"` in the mode's colour.
    ///
    /// Used as a prefix in the prompt `❯` line and the ephemeral status line.
    /// The reset sequence `\x1b[0m` always terminates the string so that the
    /// caller does not need to manage colour state.
    pub fn ansi_badge(&self) -> String {
        let ansi = match self.colour {
            ModeColour::Green   => "\x1b[32m",
            ModeColour::Yellow  => "\x1b[33m",
            ModeColour::Cyan    => "\x1b[36m",
            ModeColour::Blue    => "\x1b[34m",
            ModeColour::Magenta => "\x1b[35m",
            ModeColour::Red     => "\x1b[31m",
            ModeColour::White   => "\x1b[37m",
        };
        format!("{ansi}[{}]\x1b[0m", self.label)
    }
}
```

ANSI codes used:
- `\x1b[32m` — green (SGR 32, foreground green)
- `\x1b[33m` — yellow (SGR 33, foreground yellow)
- `\x1b[36m` — cyan (SGR 36, foreground cyan)
- `\x1b[34m` — blue (SGR 34, foreground blue)
- `\x1b[35m` — magenta (SGR 35, foreground magenta)
- `\x1b[31m` — red (SGR 31, foreground red)
- `\x1b[37m` — white (SGR 37, foreground white)
- `\x1b[0m` — reset all attributes

Expected output for Plan mode (`label = "PLAN"`, `colour = ModeColour::Yellow`):
`"\x1b[33m[PLAN]\x1b[0m"`

---

## 4. Permanent mode footer in the renderer

### Current layout of `draw_prompt_block()`

```
cursor arrives here (from clear_bottom_block)
─────────── top border
[▲ N more lines above]   ← only when hidden_above > 0
❯ input text line 1
❯ input text line 2  (soft-wrap continuations)
─────────── bottom border
[trigger dropdown rows]  ← only when active_trigger.is_some()
[menu overlay rows]      ← only when menu_active (drawn by MenuDriver)
cursor moves back UP to input caret position
```

### New layout after this PRD

```
─────────── top border
[▲ N more lines above]
❯ [PLAN] input text          ← badge left of ❯ when not Auto (see §5)
─────────── bottom border
[trigger dropdown rows]
[menu overlay rows]
  ⏵⏵ Auto  (shift+tab to cycle)   ← NEW permanent mode footer row
cursor moves back UP to input caret position
```

The mode footer is always present — one row, always rendered. It is not part of the trigger dropdown or menu overlay. It sits below all overlays and above nothing.

### Renderer field additions

Add to `IncrementalRenderer<W>`:

```rust
/// Number of rows used by the permanent mode footer below the prompt block.
/// Currently always 0 or 1; tracked separately from `last_prompt_height`
/// so `clear_bottom_block` can erase it precisely.
footer_rows: usize,
```

Initialise to `0` in both `IncrementalRenderer::new()` and `IncrementalRenderer::with_writer()`.

### Changes to `draw_prompt_block()`

**Step 1 — Include `footer_rows` in `last_prompt_height`.**

The `total` rows calculation currently is:
```rust
let total = BORDER_ROWS as usize
    + (if show_indicator { 1 } else { 0 })
    + max_in_view
    + trigger_rows;
self.last_prompt_height = total as u16;
```

Change to:
```rust
const FOOTER_ROWS: usize = 1;
let total = BORDER_ROWS as usize
    + (if show_indicator { 1 } else { 0 })
    + max_in_view
    + trigger_rows
    + FOOTER_ROWS;
self.last_prompt_height = total as u16;
self.footer_rows = FOOTER_ROWS;
```

**Step 2 — Render the footer after the trigger dropdown.**

At the end of `draw_prompt_block`, after the existing trigger-dropdown rendering block but before the cursor-repositioning block:

```rust
// ── Permanent mode footer ─────────────────────────────────────────────────
// Always rendered; one line; dim style.
let footer = &prompt.mode_footer;
queue!(
    self.out,
    Print("\r\n"),
    terminal::Clear(ClearType::CurrentLine),
    Print(format!("  \x1b[2m{footer}\x1b[0m")),
)?;
```

`\x1b[2m` is SGR 2 (dim/faint). `\x1b[0m` resets. The two leading spaces align the footer with the prompt's prefix column.

**Step 3 — Include footer rows in the cursor-up calculation.**

The existing cursor-repositioning at the end of `draw_prompt_block` moves the cursor back up to the caret row:

```rust
let rows_from_bottom = (max_in_view - vis_row) as u16;
let col = (vis_col + PREFIX_WIDTH) as u16;
queue!(
    self.out,
    cursor::MoveToColumn(0),
    cursor::MoveUp(rows_from_bottom + trigger_rows as u16),
    cursor::MoveToColumn(col),
)?;
```

Change to:

```rust
let rows_from_bottom = (max_in_view - vis_row) as u16;
let col = (vis_col + PREFIX_WIDTH) as u16;
queue!(
    self.out,
    cursor::MoveToColumn(0),
    cursor::MoveUp(rows_from_bottom + trigger_rows as u16 + self.footer_rows as u16),
    cursor::MoveToColumn(col),
)?;
```

### Changes to `clear_bottom_block()`

The current implementation counts rows up from the caret:

```rust
let rows_up = self.ephemeral_count as u16
    + 1                                    // top border
    + u16::from(self.rendered_indicator)
    + self.rendered_caret_vis_row as u16;
```

The cursor rests at the caret row. Below the caret are:
- `(max_in_view - vis_row - 1)` remaining input rows
- 1 bottom border
- `trigger_rows` dropdown rows
- `footer_rows` footer rows

`clear_bottom_block` moves up to the top of the ephemeral region and issues `ClearFromCursorDown`, which clears everything below including all rows below the caret. This already works correctly for trigger rows (which are below the caret), and `footer_rows` is also below the caret — `ClearFromCursorDown` naturally erases it. **No change to the `rows_up` computation is needed** because `ClearFromCursorDown` always erases from the cursor to the end of the screen, regardless of how many footer rows were drawn.

However, the cursor must be positioned above the ephemeral region before `ClearFromCursorDown` runs. The existing `rows_up` calculation achieves this. After the clear, `redraw_bottom_block` calls `draw_prompt_block` which now re-draws the footer and moves the cursor back up. This is correct.

No change to `clear_bottom_block` is required.

### Cursor invariant after this change

After `draw_prompt_block` returns, the physical cursor rests at the caret's column in the input area (same invariant as before). The footer is one row below the cursor's resting position. The `ClearFromCursorDown` in `clear_bottom_block` will erase the footer along with all rows below the caret.

---

## 5. Mode badge in the prompt `❯` marker

In `draw_prompt_block`, the `❯` prefix is currently rendered like this (for the first visible row):

```rust
if is_first {
    queue!(self.out, SetForegroundColor(theme.warning))?;
    queue!(self.out, Print(prefix), SetAttribute(Attribute::Reset))?;
}
```

where `prefix = "❯ "`.

Replace this with badge-aware rendering. Because the badge already carries its own ANSI escape sequences via `ansi_badge()`, emit it as raw bytes rather than going through crossterm's `SetForegroundColor`:

```rust
if is_first {
    if let Some(ref badge) = prompt.mode_badge {
        // Non-Auto mode: "[PLAN] ❯ " with the badge in mode colour,
        // the ❯ in theme.warning colour.
        queue!(
            self.out,
            Print(format!(
                "  {} \x1b[1;{}m❯\x1b[0m ",
                badge,
                theme_ansi_code(theme.warning),
            )),
        )?;
    } else {
        // Auto mode: plain "  ❯ " in theme.warning colour.
        queue!(
            self.out,
            SetForegroundColor(theme.warning),
            Print("  ❯ "),
            SetAttribute(Attribute::Reset),
        )?;
    }
}
```

`theme_ansi_code` is a small private helper that converts a crossterm `Color` to the corresponding SGR foreground code number:

```rust
/// Map a crossterm `Color` to its SGR foreground colour number string.
/// Returns `"32"` (green) as a fallback for unexpected inputs.
fn theme_ansi_code(c: crossterm::style::Color) -> &'static str {
    use crossterm::style::Color;
    match c {
        Color::Green       => "32",
        Color::Yellow      => "33",
        Color::Cyan        => "36",
        Color::Blue        => "34",
        Color::Magenta     => "35",
        Color::Red         => "31",
        Color::White       => "37",
        Color::DarkGreen   => "32",
        Color::DarkYellow  => "33",
        _                  => "32",
    }
}
```

For continuation rows (soft-wrap, `is_first == false`), no badge is shown — the existing dim `❯ ` prefix is unchanged.

The `PREFIX_WIDTH` constant (currently `2` in `crates/xaft-tui/src/input_bar.rs`) represents the number of terminal columns consumed by `"❯ "`. With a badge the visual prefix is wider. **Do not change `PREFIX_WIDTH`.** The badge is decorative; the cursor column computation is anchored to `PREFIX_WIDTH` which maps to the start of the user's editable text, not to the start of the visual prefix. The badge is printed before the caret column and does not shift the user-text column in the model.

---

## 6. Mode in the ephemeral status line

`build_ephemeral(state: &AppState)` in `crates/xaft-tui/src/ephemeral.rs` builds the `EphemeralState` struct. The `status_line` field currently looks like:
```
"Tokens: 1.2k in / 400 out  ·  $0.0042"
```

When not in Auto mode, prepend the mode badge:

```rust
pub fn build_ephemeral(state: &AppState) -> Option<EphemeralState> {
    if !state.phase.is_active() && state.agent_tracker.active_count() == 0 {
        return None;
    }

    // ... (spinner_line unchanged) ...

    let status_line = if state.total_input_tokens > 0 || state.total_output_tokens > 0 {
        let in_tok  = format_tokens_compact(state.total_input_tokens);
        let out_tok = format_tokens_compact(state.total_output_tokens);
        let base = format!(
            "Tokens: {in_tok} in / {out_tok} out  ·  ${:.4}",
            state.total_cost_usd,
        );
        let mode = state.mode_manager.active();
        if mode.name != "Auto" {
            Some(format!("{}  {}", mode.ansi_badge(), base))
        } else {
            Some(base)
        }
    } else {
        None
    };

    Some(EphemeralState { spinner_line, status_line })
}
```

`state.mode_manager` must be accessible from `build_ephemeral`. Because `build_ephemeral` already takes `&AppState`, and `mode_manager` is a field of `AppState`, this works without a signature change.

---

## 7. `/mode` slash command

### Design rationale

`SlashHandler::execute()` takes `CommandContext` which is immutable with respect to `AppState`. The handler cannot call `self.mode_manager.cycle()` or `self.mode_manager.switch_to("Plan")` because it has no access to the `AppState` instance. Two clean approaches exist:

**Option A — signal-based (recommended):** The handler emits a new `TuiEvent::SwitchMode(String)` via `ctx.signals` (the `SignalBus`). `AppState::handle_event` picks it up and calls `mode_manager.switch_to(name)`. This keeps `AppState` as the sole mutator of its own mode field and is consistent with how other async state changes arrive (tool completions, etc.).

**Option B — lines-based:** The handler prints the current mode list or an error; the user must still manually cycle with Shift+Tab. This degrades `/mode` to a read-only command.

This PRD mandates **Option A**.

### New `TuiEvent::SwitchMode` variant

Add to `TuiEvent` in `crates/xaft-tui/src/bridge.rs`:

```rust
// ── Mode ──────────────────────────────────────────────────────────────────
/// Request to switch the active agent mode by name.
/// Emitted by `ModeHandler` via the signal bus; handled by
/// `AppState::handle_event` which calls `mode_manager.switch_to`.
SwitchMode(String),
```

Because `TuiEvent` derives `Debug` and `Clone`, `SwitchMode(String)` satisfies both automatically.

The `EventBridge` does **not** subscribe to any signal for this event — it is produced directly by the slash handler emitting on `ctx.signals` using a `SwitchMode`-specific signal type (see below), and the bridge translates it.

#### New signal type for the bridge

Add in `crates/xaft-agent/src/signals.rs` (or alongside existing mode signals from PRD 64):

```rust
/// Emitted by the `/mode` slash handler to request a mode switch.
/// The `EventBridge` translates this into `TuiEvent::SwitchMode`.
#[derive(Debug, Clone)]
pub struct XaftModeSwitchRequested {
    pub name: String,
}
```

In `EventBridge::attach()` in `bridge.rs`, subscribe to `XaftModeSwitchRequested`:

```rust
let tx2 = self.tx.clone();
bus.subscribe(move |sig: XaftModeSwitchRequested| {
    let _ = tx2.send(TuiEvent::SwitchMode(sig.name));
})
.await;
```

### Handler in `AppState::handle_event()`

Add a match arm in `handle_event`:

```rust
TuiEvent::SwitchMode(name) => {
    match self.mode_manager.switch_to(&name) {
        Ok(new_mode) => {
            self.mode_notification = Some(
                format!("✦ Switched to {} mode", new_mode.name)
            );
            if let Some(ref bus) = self.signal_bus {
                let sig = XaftModeChanged {
                    from_name: self.mode_manager.previous_name().to_string(),
                    to_name: new_mode.name.clone(),
                    to_label: new_mode.label.clone(),
                    to_colour: new_mode.colour,
                };
                emit_signal_if_bus(Some(Arc::clone(bus)), sig);
            }
        }
        Err(e) => {
            self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
                format!("  Mode error: {e}"),
                LineKind::System,
            )));
        }
    }
    self.mutations
        .push(RenderMutation::UpdatePrompt(build_prompt(self)));
}
```

`ModeManager::switch_to(&str)` is defined in PRD 64. It returns `Ok(&AgentMode)` on a case-insensitive name match, `Err(ModeError::UnknownMode(name))` otherwise.

### Handler struct: `ModeHandler`

New file: `crates/xaft-tui/src/slash/commands/mode.rs`

```rust
//! `/mode` slash command — show or switch the active agent mode.

use std::sync::Arc;

use xaft_config::XaftConfig;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};
use xaft_agent::signals::XaftModeSwitchRequested;

pub struct ModeHandler {
    pub config: Arc<XaftConfig>,
}

impl SlashHandler for ModeHandler {
    fn description(&self) -> &'static str {
        "Show or switch the active agent mode  (/mode [Auto|Plan|Ask|Review|Safe|Debug])"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[mode-name]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let args = ctx.args.trim();

        if args.is_empty() {
            // No argument: list available modes with descriptions.
            // The mode list comes from the config; fall back to the built-in set.
            let lines = vec![
                "Available modes (shift+tab to cycle):".into(),
                "  Auto    — default planner→coder→QA pipeline".into(),
                "  Plan    — read-only; produces a plan document, no edits".into(),
                "  Ask     — Q&A only; no file writes or shell commands".into(),
                "  Review  — code review; read-only diff analysis".into(),
                "  Safe    — no bash_exec; file writes need confirmation".into(),
                "  Debug   — verbose tracing; all tools available".into(),
                "".into(),
                "Usage: /mode <name>  e.g. /mode Plan".into(),
            ];
            return CommandResult::Lines(lines);
        }

        // Non-empty: request a mode switch via signal bus.
        // AppState::handle_event(TuiEvent::SwitchMode) does the actual switch.
        let name = args.to_string();
        // Emit through the signal bus — the EventBridge translates
        // XaftModeSwitchRequested → TuiEvent::SwitchMode.
        let bus = Arc::clone(&ctx.signals);
        let sig = XaftModeSwitchRequested { name: name.clone() };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                tokio::spawn(async move { bus.emit(sig).await });
            }
            Err(_) => {
                // Test context — build a temporary runtime.
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread().build() {
                    rt.block_on(async move { bus.emit(sig).await });
                }
            }
        }
        CommandResult::Handled
    }
}
```

### Parser entry in `SlashCommand` enum

Add a new variant to `SlashCommand` in `crates/xaft-tui/src/slash/mod.rs`:

```rust
/// `/mode [name]` — show available modes or switch to `name`.
Mode { name: Option<String> },
```

Add `trigger_name`:
```rust
SlashCommand::Mode { .. } => "mode",
```

Add to `all_example_variants()`:
```rust
SlashCommand::Mode { name: None },
```

Add the parse function in `crates/xaft-tui/src/slash/parser.rs`:

```rust
fn parse_mode(args: &str) -> Result<SlashCommand, String> {
    let name = {
        let t = args.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    Ok(SlashCommand::Mode { name })
}
```

Register in `COMMAND_TABLE`:
```rust
CommandEntry { trigger: "mode", parse_fn: parse_mode },
```

### Registration in `build_registry_with_config()`

In `crates/xaft-tui/src/slash/commands/mod.rs`:

```rust
pub mod mode;
pub use mode::ModeHandler;
```

In `build_registry_with_config()`:
```rust
.register(SlashCommand::Mode { name: None }, ModeHandler { config: Arc::clone(&config) })
```

---

## 8. Visual examples

Terminal width assumed at 80 columns. Badges are shown as plain text; ANSI colours are omitted for legibility.

### 8.1 Auto mode — clean prompt, standing footer

```
──────────────────────────────────────────────────────────────────────────────
  ❯ Refactor the auth module to use JWTs
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ Auto  (shift+tab to cycle)
```

### 8.2 Plan mode — yellow `[PLAN]` badge, standing footer

```
──────────────────────────────────────────────────────────────────────────────
  [PLAN] ❯ Refactor the auth module to use JWTs
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ [PLAN] Plan — read-only  (shift+tab to cycle)
```

(The `[PLAN]` tokens are rendered in ANSI yellow `\x1b[33m`.)

### 8.3 Safe mode — magenta `[SAFE]` badge

```
──────────────────────────────────────────────────────────────────────────────
  [SAFE] ❯ _
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ [SAFE] Safe — no bash_exec  (shift+tab to cycle)
```

### 8.4 Transition moment (one-shot notification)

Immediately after Shift+Tab cycles from Auto → Plan, `mode_notification` is
set. The *next* `build_prompt` call consumes it and places it in `mode_footer`.
The frame after that, `mode_notification` is `None` and the footer reverts to
the standing text.

**Frame 1 (notification):**
```
──────────────────────────────────────────────────────────────────────────────
  [PLAN] ❯ _
──────────────────────────────────────────────────────────────────────────────
  ✦ Switched to Plan mode
```

**Frame 2+ (steady state):**
```
──────────────────────────────────────────────────────────────────────────────
  [PLAN] ❯ _
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ [PLAN] Plan — read-only  (shift+tab to cycle)
```

### 8.5 Ask mode — cyan `[ASK]` badge, trigger dropdown open

```
──────────────────────────────────────────────────────────────────────────────
  [ASK] ❯ /mode
──────────────────────────────────────────────────────────────────────────────
  ▶ mode    Show or switch the active agent mode
    model   Switch or display the active LLM model
    memory  Manage memory entries
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ [ASK] Ask — Q&A only  (shift+tab to cycle)
```

### 8.6 Ephemeral status line — non-Auto mode

When an agent run is active in Plan mode, the ephemeral region looks like:

```
✢ Planning… (0m 12s · ↓ 0 tokens)
[PLAN]  Tokens: 0 in / 0 out  ·  $0.0000
──────────────────────────────────────────────────────────────────────────────
  [PLAN] ❯ _
──────────────────────────────────────────────────────────────────────────────
  ⏵⏵ [PLAN] Plan — read-only  (shift+tab to cycle)
```

---

## 9. Tests

### 9.1 Unit tests — `crates/xaft-tui/src/mode/mod.rs` (or `prompt.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::{AgentMode, ModeColour};

    fn plan_mode() -> AgentMode {
        AgentMode {
            name: "Plan".into(),
            label: "PLAN".into(),
            description: "Read-only; produces a plan document, no edits".into(),
            colour: ModeColour::Yellow,
        }
    }

    fn auto_mode() -> AgentMode {
        AgentMode {
            name: "Auto".into(),
            label: "AUTO".into(),
            description: "Default pipeline".into(),
            colour: ModeColour::Green,
        }
    }

    // ── 1. ansi_badge contains the label ──────────────────────────────────

    #[test]
    fn test_ansi_badge_contains_label() {
        let mode = plan_mode();
        let badge = mode.ansi_badge();
        assert!(
            badge.contains("PLAN"),
            "badge must contain the label; got: {badge:?}"
        );
    }

    #[test]
    fn test_ansi_badge_contains_reset() {
        let mode = plan_mode();
        let badge = mode.ansi_badge();
        assert!(
            badge.contains("\x1b[0m"),
            "badge must end with ANSI reset; got: {badge:?}"
        );
    }

    #[test]
    fn test_ansi_badge_yellow_for_plan() {
        let mode = plan_mode();
        let badge = mode.ansi_badge();
        assert!(
            badge.contains("\x1b[33m"),
            "Plan mode badge must be yellow (SGR 33); got: {badge:?}"
        );
    }

    // ── 2. Auto mode → mode_badge is None ─────────────────────────────────

    #[test]
    fn test_auto_mode_badge_is_none() {
        let mut state = crate::state::AppState::new("");
        // mode_manager is initialised to Auto (the first mode in the ring).
        let prompt = crate::prompt::build_prompt(&mut state);
        assert!(
            prompt.mode_badge.is_none(),
            "Auto mode must produce mode_badge = None"
        );
    }

    // ── 3. Plan mode → mode_badge is Some ─────────────────────────────────

    #[test]
    fn test_plan_mode_badge_is_some() {
        let mut state = crate::state::AppState::new("");
        state.mode_manager.switch_to("Plan").unwrap();
        let prompt = crate::prompt::build_prompt(&mut state);
        assert!(
            prompt.mode_badge.is_some(),
            "Plan mode must produce mode_badge = Some(_)"
        );
    }

    // ── 4. Auto footer contains "Auto" and "shift+tab" ────────────────────

    #[test]
    fn test_mode_footer_default_text() {
        let mut state = crate::state::AppState::new("");
        let prompt = crate::prompt::build_prompt(&mut state);
        assert!(
            prompt.mode_footer.contains("Auto"),
            "Auto footer must contain 'Auto'"
        );
        assert!(
            prompt.mode_footer.to_lowercase().contains("shift+tab"),
            "Auto footer must mention 'shift+tab'"
        );
    }

    // ── 5. Plan footer contains "PLAN" ────────────────────────────────────

    #[test]
    fn test_mode_footer_shows_active_mode() {
        let mut state = crate::state::AppState::new("");
        state.mode_manager.switch_to("Plan").unwrap();
        let prompt = crate::prompt::build_prompt(&mut state);
        assert!(
            prompt.mode_footer.contains("PLAN"),
            "Plan footer must contain 'PLAN'; got: {:?}",
            prompt.mode_footer
        );
    }

    // ── 6. Notification cleared after second build_prompt call ────────────

    #[test]
    fn test_mode_notification_cleared_after_build_prompt() {
        let mut state = crate::state::AppState::new("");
        state.mode_notification = Some("✦ Switched to Plan mode".into());

        // First call consumes the notification.
        let p1 = crate::prompt::build_prompt(&mut state);
        assert!(
            p1.mode_footer.contains("Switched to Plan mode"),
            "first build_prompt must use notification text"
        );

        // Second call has no notification; footer reverts to standing text.
        let p2 = crate::prompt::build_prompt(&mut state);
        assert!(
            !p2.mode_footer.contains("Switched to Plan mode"),
            "second build_prompt must not show notification again"
        );
    }
}
```

### 9.2 Integration tests — `crates/xaft-tui/tests/mode_ui_integration.rs`

```rust
//! Integration tests for PRD-66 Mode UI.
//!
//! Tests run against a real `AppState` + key event dispatch.
//! No terminal I/O occurs — mutations are inspected in memory.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::AppState;

fn backtab_event() -> KeyEvent {
    KeyEvent {
        code: KeyCode::BackTab,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

// ── 1. Shift+Tab advances mode ─────────────────────────────────────────────

#[test]
fn test_shift_tab_cycles_mode() {
    let mut state = AppState::new("");
    let initial = state.mode_manager.active().name.clone();
    state.handle_event(TuiEvent::Key(backtab_event()));
    let after = state.mode_manager.active().name.clone();
    assert_ne!(
        initial, after,
        "Shift+Tab must advance to the next mode (was {initial}, still {after})"
    );
}

// ── 2. Cycling through all modes wraps back to Auto ───────────────────────

#[test]
fn test_shift_tab_wraps() {
    let mut state = AppState::new("");
    let total = state.mode_manager.len(); // number of modes in the ring
    for _ in 0..total {
        state.handle_event(TuiEvent::Key(backtab_event()));
    }
    assert_eq!(
        state.mode_manager.active().name,
        "Auto",
        "cycling through all modes must wrap back to Auto"
    );
}

// ── 3. mode_notification is set after Shift+Tab ───────────────────────────

#[test]
fn test_mode_notification_set_on_cycle() {
    let mut state = AppState::new("");
    state.handle_event(TuiEvent::Key(backtab_event()));
    assert!(
        state.mode_notification.is_some(),
        "mode_notification must be Some after Shift+Tab"
    );
}

// ── 4. /mode with no args shows the list ─────────────────────────────────

#[test]
fn test_slash_mode_no_args_shows_list() {
    use xaft_tui::slash::{CommandContext, SlashCommand};
    use xaft_tui::slash::commands::ModeHandler;
    use xaft_tui::slash::registry::SlashHandler;
    use std::sync::{Arc, RwLock};
    use agtrs_runtime::signals::SignalBus;
    use xaft_config::XaftConfig;

    let handler = ModeHandler { config: Arc::new(XaftConfig::default()) };
    let ctx = CommandContext {
        args: "".into(),
        command: SlashCommand::Mode { name: None },
        signals: Arc::new(SignalBus::new()),
        config: Arc::new(XaftConfig::default()),
        session_id: None,
        working_dir: std::path::PathBuf::from("/tmp"),
        terminal_cols: 80,
        llm_stats: Arc::new(RwLock::new(Default::default())),
        conversation_store: None,
        session_store: None,
    };
    let result = handler.execute(ctx);
    let text = match result {
        xaft_tui::slash::CommandResult::Lines(ls) => ls.join("\n"),
        other => panic!("expected Lines, got {other:?}"),
    };
    assert!(text.contains("Auto"), "mode list must mention Auto");
    assert!(text.contains("Plan"), "mode list must mention Plan");
}

// ── 5. /mode Plan switches mode ───────────────────────────────────────────

#[tokio::test]
async fn test_slash_mode_with_name_switches() {
    let mut state = AppState::new("");
    // Simulate the full signal round-trip by sending TuiEvent::SwitchMode directly.
    state.handle_event(TuiEvent::SwitchMode("Plan".into()));
    assert_eq!(
        state.mode_manager.active().name,
        "Plan",
        "SwitchMode(Plan) must activate Plan mode"
    );
}

// ── 6. /mode Bogus shows error ────────────────────────────────────────────

#[tokio::test]
async fn test_slash_mode_unknown_name_shows_error() {
    let mut state = AppState::new("");
    let mutation_count_before = state.mutations.len();
    state.handle_event(TuiEvent::SwitchMode("Bogus".into()));
    // A CommitLine error mutation must have been pushed.
    assert!(
        state.mutations.len() > mutation_count_before,
        "unknown mode name must produce an error mutation"
    );
}
```

### 9.3 E2E tests — `crates/xaft-tui/tests/mode_e2e.rs`

These tests exercise the connection between mode selection and `RunRequest` construction. They require PRD 64's `AgentMode::system_patch()` and `AgentMode::tool_filter()` to be implemented.

```rust
//! E2E tests: mode selection → RunRequest shaping.

use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::AppState;

// ── 1. Plan mode: RunRequest carries system patch ─────────────────────────

#[tokio::test]
async fn test_plan_mode_submitted_task_includes_patch() {
    let mut state = AppState::new("/tmp");
    state.handle_event(TuiEvent::SwitchMode("Plan".into()));
    assert_eq!(state.mode_manager.active().name, "Plan");

    // Simulate a submit via handle_submit (does not require a real channel).
    // We inspect the RunRequest that would be built.
    let run_request = state.build_run_request_for_test("Write a plan for auth refactor");
    let patch = run_request
        .mode_system_patch
        .as_deref()
        .unwrap_or("");
    assert!(
        patch.to_uppercase().contains("PLAN"),
        "Plan mode system patch must contain 'PLAN'; got: {patch:?}"
    );
}

// ── 2. Safe mode: tool filter rejects bash_exec ───────────────────────────

#[tokio::test]
async fn test_safe_mode_filters_tools_in_run_request() {
    let mut state = AppState::new("/tmp");
    state.handle_event(TuiEvent::SwitchMode("Safe".into()));
    assert_eq!(state.mode_manager.active().name, "Safe");

    let run_request = state.build_run_request_for_test("Run the test suite");
    let filter = run_request.mode_tool_filter.as_ref();
    assert!(
        filter.map(|f| f.rejects("bash_exec")).unwrap_or(false),
        "Safe mode must reject bash_exec in the tool filter"
    );
}
```

`build_run_request_for_test` is a test-only method on `AppState` that constructs a `RunRequest` from the current state without actually submitting it. Add it behind `#[cfg(test)]` in `state.rs`. The `mode_system_patch` and `mode_tool_filter` fields on `RunRequest` are defined by PRD 64/65 — this PRD does not add them; it merely tests that the plumbing from mode selection to run request is correctly wired.

---

## 10. Dependency summary

| PRD | What this PRD relies on |
|-----|-------------------------|
| PRD-64 | `AgentMode`, `ModeColour`, `ModeManager` struct with `cycle()`, `switch_to()`, `active()`, `previous_name()`, `len()` |
| PRD-64 | `XaftModeChanged` signal |
| PRD-65 | Built-in mode definitions: Auto, Plan, Ask, Review, Safe, Debug |
| PRD-65 | `AgentMode::system_patch()` and `AgentMode::tool_filter()` (used in E2E tests only) |

---

## 11. File change summary

| File | Change |
|------|--------|
| `crates/xaft-tui/src/prompt.rs` | Add `mode_badge`, `mode_footer` to `PromptState`; update `build_prompt` signature to `&mut AppState`; populate new fields |
| `crates/xaft-tui/src/state.rs` | Add `mode_notification: Option<String>` field; add `take_mode_notification()`; add Shift+Tab block in `handle_key()`; add `TuiEvent::SwitchMode` arm in `handle_event()` |
| `crates/xaft-tui/src/bridge.rs` | Add `TuiEvent::SwitchMode(String)` variant; add `XaftModeSwitchRequested` bridge subscription |
| `crates/xaft-tui/src/renderer.rs` | Add `footer_rows` field; render mode footer in `draw_prompt_block()`; include footer rows in cursor-up move; badge-aware `❯` rendering |
| `crates/xaft-tui/src/ephemeral.rs` | Prepend mode badge to `status_line` when not in Auto |
| `crates/xaft-tui/src/slash/mod.rs` | Add `SlashCommand::Mode { name: Option<String> }` variant |
| `crates/xaft-tui/src/slash/parser.rs` | Add `parse_mode` fn; register in `COMMAND_TABLE` |
| `crates/xaft-tui/src/slash/commands/mode.rs` | New file: `ModeHandler` |
| `crates/xaft-tui/src/slash/commands/mod.rs` | `pub mod mode`; `pub use mode::ModeHandler`; register in `build_registry_with_config` |
| `crates/xaft-agent/src/signals.rs` | Add `XaftModeSwitchRequested` signal |
| `crates/xaft-tui/tests/mode_ui_integration.rs` | New integration test file |
| `crates/xaft-tui/tests/mode_e2e.rs` | New E2E test file |
