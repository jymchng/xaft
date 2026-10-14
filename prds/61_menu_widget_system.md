---
title: "PRD-61: Menu Widget System"
status: draft
version: 0.1.0
created: 2026-06-13
depends-on:
  - 59_input_trigger_system.md   # TriggerHandler, ActiveTrigger, ActiveTriggerSnapshot, TermWriter
  - 32c_slash_command_registry.md
analog: python/agenthicc/prds/prd-41-menu-widget-system.md
---

# PRD-61: Menu Widget System

## Executive Summary

xaft slash commands today can only return static output (lines of text, config tables, errors).
This PRD introduces a **`MenuWidget` trait**, a **`MenuDriver`** coordinator, and the plumbing to let
any slash command open a full, interactive terminal widget — a form, a picker, a confirmation dialog — inside
the existing `IncrementalRenderer` input area.  A `DropdownWidget` adapter bridges the existing `ActiveTrigger`
trigger system (PRD-59) into the new abstraction so future refactors can unify both surfaces under one driver.

---

## 1. Problem Statement

### 1.1 Slash commands are output-only

Every slash command handler returns a `CommandResult` variant:

```rust
// crates/xaft-tui/src/slash/mod.rs
pub enum CommandResult {
    Lines(Vec<String>),
    StyledLines(Vec<StyledLine>),
    Handled,
    Error(String),
    ConfigDisplay(Vec<ConfigSection>),
}
```

None of these variants allow a command to open an interactive widget.  A command like `/model` that
should present a selectable list of available models, or a `/theme` picker showing live previews, must
today either dump a static list or launch a separate Ratatui overlay (which contradicts the
append-only `IncrementalRenderer` architecture).

### 1.2 No shared interactive-widget protocol

The existing `@`-mention dropdown and `/` slash-command palette are interactive, but they are driven
entirely by the `TriggerHandler` protocol — they activate on a typed character, not on a command
dispatch.  There is no equivalent protocol for:

- A command that wants to show a picker after the user presses Enter.
- A form widget (e.g. `config set` interactive editor).
- A multi-step wizard opened by a skill.

Implementing any of these today requires bespoke additions to `AppState::handle_key()` and the
renderer, with no shared abstraction.

### 1.3 Result: duplication surface when adding interactive commands

Without a shared protocol, every new interactive command would add:

1. A new `Option<T>` field on `AppState`.
2. New key-dispatch guards in `handle_key()`.
3. New rendering logic in `draw_prompt_block()`.
4. A new `PromptState` field.
5. A new arm in `build_prompt()`.

The `MenuWidget` / `MenuDriver` system provides a single slot that serves all of these.

---

## 2. Goals

| ID | Goal |
|----|------|
| G1 | Define a `MenuWidget` trait with `render()`, `handle_key()`, `edit_field_value()`, and `title()` |
| G2 | Define `MenuResult` enum: `Continue`, `Done(MenuPayload)`, `Cancel` |
| G3 | Define `MenuDriver` that owns at most one active `Box<dyn MenuWidget>` at a time |
| G4 | `AppState` holds a single `menu_driver: MenuDriver`; all keys route through it while active |
| G5 | `CommandResult::OpenMenu(Box<dyn MenuWidget>)` lets any slash command open an interactive widget |
| G6 | `AppState::apply_command_result()` calls `self.menu_driver.open(widget)` on `OpenMenu` |
| G7 | `IncrementalRenderer::draw_prompt_block()` calls `menu_driver.render()` below the input bar when active |
| G8 | `DropdownWidget` wraps an `ActiveTrigger` + `TriggerHandler` to implement `MenuWidget`, providing a migration path for existing trigger dropdowns |
| G9 | `PromptState` gets a `menu_active: bool` field so the renderer knows whether to reserve space |
| G10 | All new types are `Send + Sync`; `MenuDriver` itself is `!Send` (lives on the TUI event thread) |

## Non-Goals

- Replacing the existing `TriggerHandler` / `ActiveTrigger` system with `MenuDriver` (that migration
  is a follow-up; `DropdownWidget` is the bridge).
- Nested menus (a `MenuWidget` opening another `MenuWidget`).
- Mouse input routing to `MenuWidget`.
- Ratatui layout primitives — widgets write directly to `impl TermWriter`.
- Async widget methods — `handle_key` and `render` are synchronous.

---

## 3. File Layout

```
crates/xaft-tui/src/menu/
  mod.rs        ← MenuWidget trait, MenuResult, MenuPayload, MenuDriver
  dropdown.rs   ← DropdownWidget (wraps ActiveTrigger + Arc<dyn TriggerHandler>)
```

Add to `crates/xaft-tui/src/lib.rs`:

```rust
pub mod menu;
```

---

## 4. Core Types

### 4.1 `MenuPayload`

The value a widget produces when the user commits a selection.

```rust
// crates/xaft-tui/src/menu/mod.rs

/// The value produced by a `MenuWidget` when the user commits.
///
/// Variants are intentionally coarse — widgets that need richer payloads
/// should serialise to `Custom(serde_json::Value)` or define their own
/// downstream conversion.
#[derive(Debug, Clone)]
pub enum MenuPayload {
    /// A single selected string (e.g. a model name, a theme name).
    Selected(String),
    /// A key-value pair (e.g. config key + new value from a form field).
    KeyValue(String, String),
    /// A free-form JSON blob for structured payloads from complex widgets.
    Custom(serde_json::Value),
    /// The widget completed without producing a value (e.g. a confirmation that was
    /// accepted without a selection — the command already acted on the event).
    Empty,
}
```

### 4.2 `MenuResult`

Returned by `MenuWidget::handle_key()` on every keypress.

```rust
/// The outcome of routing one keypress through a `MenuWidget`.
#[derive(Debug)]
pub enum MenuResult {
    /// The widget consumed the key and remains open.  No payload yet.
    Continue,
    /// The user committed a selection.  `MenuDriver` will close the widget
    /// and surface the payload to `AppState`.
    Done(MenuPayload),
    /// The user dismissed the widget (Esc, Ctrl+C).  `MenuDriver` closes
    /// it and discards the payload.
    Cancel,
}
```

### 4.3 `MenuWidget` trait

```rust
/// A self-contained interactive terminal widget rendered below the input bar.
///
/// # Contract
///
/// - `render` writes directly to `out` using crossterm `queue!` macros and
///   returns the number of **rows** it wrote (not bytes).  The `prev_rows`
///   parameter is the row count the widget claimed on the *previous* frame;
///   `draw_prompt_block` uses it to position the cursor-up before the next
///   erase+redraw.
///
/// - `handle_key` is called for every `KeyEvent` while the widget is active,
///   **before** `AppState`'s normal key dispatch.  Returning `Continue` means
///   the key was fully consumed; `Done` / `Cancel` closes the widget and the
///   key is **not** re-dispatched.
///
/// - Widgets are `Send + Sync` so they can be constructed by background tasks
///   (e.g. a slash command handler running in a `spawn_blocking` context) and
///   sent through a channel to the TUI thread, which is the only thread that
///   calls `render` and `handle_key`.
///
/// # Error handling
///
/// `render` returns `io::Result<usize>`.  On error the `MenuDriver` logs the
/// error and calls `close()` — the widget is not left in a partially-rendered
/// state.
pub trait MenuWidget: Send + Sync {
    /// Write the widget's UI to `out`.
    ///
    /// - `cols`: current terminal width in columns.
    /// - `prev_rows`: rows written on the previous frame (used to erase before
    ///   redrawing; pass `0` on the first frame).
    ///
    /// Returns the number of rows written.  Must match the actual number of
    /// `\r\n` sequences emitted so the renderer can move the cursor back up.
    fn render(
        &self,
        out: &mut impl TermWriter,
        cols: u16,
        prev_rows: usize,
    ) -> io::Result<usize>;

    /// Handle a single key event.
    ///
    /// Called before `AppState`'s normal dispatch while `menu_driver.is_active()`.
    fn handle_key(&mut self, key: KeyEvent) -> MenuResult;

    /// If the widget has an inline text field (e.g. a search filter), return
    /// the current field value.  `draw_prompt_block` may display this in the
    /// input area instead of the normal buffer.
    ///
    /// Default: `None` (no inline edit field).
    fn edit_field_value(&self) -> Option<&str> {
        None
    }

    /// Short human-readable title shown in the widget chrome (e.g. `"Select Model"`).
    fn title(&self) -> &str;
}
```

> **Note on `impl TermWriter` in a trait method**: Because `render` takes
> `&mut impl TermWriter` (not a trait object), implementations cannot call
> `render` through a `Box<dyn MenuWidget>`.  The `MenuDriver::render()` method
> solves this by using an internal `render_erased` helper — see §5.2.

### 4.4 Object-safe rendering shim

`impl Trait` parameters are not object-safe.  `MenuDriver` stores a
`Box<dyn MenuWidget>` and must call `render` through dynamic dispatch.  Use an
object-safe helper trait in the same module:

```rust
// Private — only used by MenuDriver internally.
trait MenuWidgetExt {
    fn render_dyn(
        &self,
        out: &mut dyn TermWriter,
        cols: u16,
        prev_rows: usize,
    ) -> io::Result<usize>;
}

impl<W: MenuWidget> MenuWidgetExt for W {
    fn render_dyn(
        &self,
        out: &mut dyn TermWriter,
        cols: u16,
        prev_rows: usize,
    ) -> io::Result<usize> {
        // Wraps the concrete `out: &mut dyn TermWriter` in a newtype that
        // also implements `Write`, then delegates to the inherent `render`.
        // See §5.2 for the full DynTermWriter wrapper.
        self.render(&mut DynTermWriter(out), cols, prev_rows)
    }
}
```

`DynTermWriter` is a thin newtype:

```rust
/// Newtype that wraps `&mut dyn TermWriter` and re-implements `TermWriter`
/// so it can be passed to `MenuWidget::render(&mut impl TermWriter, ...)`.
struct DynTermWriter<'a>(&'a mut dyn TermWriter);

impl Write for DynTermWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { self.0.write(buf) }
    fn flush(&mut self) -> io::Result<()> { self.0.flush() }
}

impl TermWriter for DynTermWriter<'_> {
    fn terminal_size(&self) -> (u16, u16) { self.0.terminal_size() }
}
```

---

## 5. `MenuDriver`

### 5.1 Struct definition

```rust
// crates/xaft-tui/src/menu/mod.rs

/// Owns at most one active `MenuWidget` and coordinates key routing and rendering.
///
/// `MenuDriver` is NOT `Send` — it lives exclusively on the TUI event thread.
/// The widgets it holds are `Send + Sync` so they may be constructed off-thread.
///
/// # Invariant
///
/// `last_rows` is the row count returned by the widget's last `render()` call.
/// It is reset to `0` when no widget is active and used by the renderer to
/// erase the old frame before writing the new one.
pub struct MenuDriver {
    widget: Option<Box<dyn MenuWidgetExt + Send + Sync>>,
    last_rows: usize,
}

impl MenuDriver {
    /// Create a driver with no active widget.
    pub fn new() -> Self {
        Self {
            widget: None,
            last_rows: 0,
        }
    }

    /// Open `widget`, replacing any currently-active widget without closing it
    /// gracefully.  Use `close()` first if a graceful close is needed.
    pub fn open(&mut self, widget: Box<dyn MenuWidget>) {
        // Box<dyn MenuWidget> → Box<dyn MenuWidgetExt + Send + Sync> via blanket impl.
        self.widget = Some(widget as Box<dyn MenuWidgetExt + Send + Sync>);
        self.last_rows = 0;
    }

    /// Close the active widget unconditionally.  A `MenuResult::Cancel` is
    /// implied — the caller is responsible for any cleanup.
    pub fn close(&mut self) {
        self.widget = None;
        self.last_rows = 0;
    }

    /// Returns `true` while a widget is open.
    pub fn is_active(&self) -> bool {
        self.widget.is_some()
    }

    /// Route a keypress to the active widget.
    ///
    /// Returns `None` when no widget is active (key should proceed to normal
    /// dispatch).  Returns `Some(MenuResult)` when the key was handled — the
    /// caller should close the driver on `Done` or `Cancel`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<MenuResult> {
        self.widget.as_mut()?.handle_key(key).into()
        // Note: `MenuWidgetExt` does not have `handle_key`; we access it via
        // a downcast-free approach.  See §5.3 for the concrete implementation.
    }

    /// Render the active widget to `out`, erasing the previous frame first.
    ///
    /// Returns `Ok(0)` when no widget is active.  On render error, closes the
    /// widget and returns the error.
    pub fn render(
        &mut self,
        out: &mut dyn TermWriter,
        cols: u16,
    ) -> io::Result<usize> {
        let widget = match self.widget.as_ref() {
            Some(w) => w,
            None => return Ok(0),
        };
        match widget.render_dyn(out, cols, self.last_rows) {
            Ok(rows) => {
                self.last_rows = rows;
                Ok(rows)
            }
            Err(e) => {
                self.close();
                Err(e)
            }
        }
    }

    /// The row count from the last render (used by the renderer's cursor-up
    /// calculation to erase the old frame before redrawing).
    pub fn last_rows(&self) -> usize {
        self.last_rows
    }

    /// Returns the `edit_field_value()` of the active widget, if any.
    pub fn edit_field_value(&self) -> Option<&str> {
        // Requires access through the concrete type; see §5.3.
        None // placeholder — full impl in §5.3
    }

    /// Title of the active widget, or `None` when no widget is active.
    pub fn title(&self) -> Option<&str> {
        // Placeholder — full impl in §5.3.
        None
    }
}

impl Default for MenuDriver {
    fn default() -> Self {
        Self::new()
    }
}
```

### 5.2 Trait object design note

The `Box<dyn MenuWidget>` storage problem is that `MenuWidget::render` takes
`&mut impl TermWriter` which is not object-safe.  The solution chosen here uses
**two traits**:

1. `MenuWidget` (public, `impl Trait` signature) — what implementors write.
2. `MenuWidgetExt` (private, `&mut dyn TermWriter` signature) — what `MenuDriver` calls.

A blanket `impl<W: MenuWidget> MenuWidgetExt for W` bridges them automatically.
Implementors only implement `MenuWidget`.

`MenuDriver::widget` stores `Box<dyn MenuWidgetExt + Send + Sync>`.  The
`handle_key`, `edit_field_value`, and `title` methods are also part of
`MenuWidgetExt` (not shown above for brevity); the full `MenuWidgetExt` trait
mirrors all methods of `MenuWidget` with object-safe signatures.

### 5.3 Full `MenuWidgetExt` definition

```rust
/// Object-safe version of `MenuWidget`.  Private to this module.
trait MenuWidgetExt: Send + Sync {
    fn render_dyn(
        &self,
        out: &mut dyn TermWriter,
        cols: u16,
        prev_rows: usize,
    ) -> io::Result<usize>;

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult;

    fn edit_field_value(&self) -> Option<&str>;

    fn title(&self) -> &str;
}

impl<W: MenuWidget> MenuWidgetExt for W {
    fn render_dyn(
        &self,
        out: &mut dyn TermWriter,
        cols: u16,
        prev_rows: usize,
    ) -> io::Result<usize> {
        self.render(&mut DynTermWriter(out), cols, prev_rows)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        MenuWidget::handle_key(self, key)
    }

    fn edit_field_value(&self) -> Option<&str> {
        MenuWidget::edit_field_value(self)
    }

    fn title(&self) -> &str {
        MenuWidget::title(self)
    }
}
```

With this design, `MenuDriver` stores `Box<dyn MenuWidgetExt + Send + Sync>`,
all its methods delegate to the inner `dyn MenuWidgetExt`, and widget authors
implement only `MenuWidget`.

---

## 6. `DropdownWidget`

File: `crates/xaft-tui/src/menu/dropdown.rs`

`DropdownWidget` wraps an `ActiveTrigger` (PRD-59) together with its owning
`Arc<dyn TriggerHandler>` and implements `MenuWidget`.  This gives trigger
dropdowns a migration path into the `MenuDriver` architecture without requiring
an immediate rewrite of the trigger system.

```rust
//! `DropdownWidget` — `MenuWidget` adapter over the PRD-59 `ActiveTrigger`.
//!
//! Allows trigger dropdowns to be driven by `MenuDriver` rather than the
//! separate `AppState::active_trigger` field.  This is an **opt-in migration
//! shim**; the existing trigger system continues to operate independently
//! until a follow-up PRD completes the unification.

use std::io;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};

use crate::renderer::TermWriter;
use crate::trigger::{ActiveTrigger, MatchKind, TriggerContext, TriggerHandler};

use super::{MenuPayload, MenuResult, MenuWidget};

/// A `MenuWidget` that presents the items of an `ActiveTrigger` dropdown.
///
/// Key bindings mirror the existing trigger dispatch in `AppState::handle_trigger_key()`:
/// - Up / Down: move selection
/// - Tab / Enter: commit selection (`Done`)
/// - Esc: dismiss (`Cancel`)
pub struct DropdownWidget {
    trigger: ActiveTrigger,
    handler: Arc<dyn TriggerHandler>,
    /// Working directory forwarded to `TriggerContext` for `on_select`.
    working_dir: std::path::PathBuf,
    /// Terminal width (columns) forwarded to `TriggerContext`.
    cols: u16,
}

impl DropdownWidget {
    /// Construct from an already-populated `ActiveTrigger` and its handler.
    pub fn new(
        trigger: ActiveTrigger,
        handler: Arc<dyn TriggerHandler>,
        working_dir: std::path::PathBuf,
        cols: u16,
    ) -> Self {
        Self {
            trigger,
            handler,
            working_dir,
            cols,
        }
    }

    /// The text that should be inserted into the input buffer when `Done` is
    /// returned.  Returns `None` if nothing is selected.
    pub fn selected_insert(&self) -> Option<String> {
        let item = self.trigger.selected_item()?;
        let ctx = TriggerContext {
            prefix: &self.trigger.scan.prefix,
            dir_prefix: &self.trigger.scan.dir_prefix,
            working_dir: &self.working_dir,
            terminal_cols: self.cols,
        };
        Some(self.handler.on_select(item, &ctx))
    }
}

impl MenuWidget for DropdownWidget {
    fn render(
        &self,
        out: &mut impl TermWriter,
        cols: u16,
        _prev_rows: usize,
    ) -> io::Result<usize> {
        use crossterm::{
            queue,
            style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
        };

        let snap = &self.trigger;
        let max_vis = self.handler.max_visible();
        let visible_count = snap
            .items
            .len()
            .saturating_sub(snap.scroll_top)
            .min(max_vis);

        let mut rows_written: usize = 0;

        for (offset, item) in snap
            .items
            .iter()
            .skip(snap.scroll_top)
            .take(visible_count)
            .enumerate()
        {
            let abs_index = snap.scroll_top + offset;
            let is_selected = abs_index == snap.selected;

            let row_text = match snap.scan.trigger_char {
                '@' => format!(" + {}", item.display),
                '/' => format!(" {:<13} {}", item.display, item.hint.as_deref().unwrap_or("")),
                _ => format!(" > {}", item.display),
            };
            let max_len = cols.saturating_sub(1) as usize;
            let display: String = row_text.chars().take(max_len).collect();

            queue!(out, Print("\r\n"))?;
            if is_selected {
                queue!(
                    out,
                    SetForegroundColor(Color::Yellow),
                    SetAttribute(Attribute::Bold),
                    Print(&display),
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                queue!(out, Print(&display), SetAttribute(Attribute::Reset))?;
            }
            rows_written += 1;
        }

        // Hint line.
        let hint_text: &str = match snap.selected_item().and_then(|i| i.hint.as_deref()) {
            Some(h) => h,
            None => match snap.scan.trigger_char {
                '/' => "  \u{2191}/\u{2193} navigate  Tab complete  Enter execute  Esc dismiss",
                '@' => "",
                _ => "  \u{2191}/\u{2193} navigate  Tab/Enter select  Esc dismiss",
            },
        };
        if !hint_text.is_empty() {
            queue!(
                out,
                Print("\r\n"),
                SetForegroundColor(Color::DarkGrey),
                Print(hint_text),
                SetAttribute(Attribute::Reset),
            )?;
            rows_written += 1;
        }

        out.flush()?;
        Ok(rows_written)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        let max_vis = self.handler.max_visible();
        match key.code {
            KeyCode::Up => {
                self.trigger.select_prev(max_vis);
                MenuResult::Continue
            }
            KeyCode::Down => {
                self.trigger.select_next(max_vis);
                MenuResult::Continue
            }
            KeyCode::Tab | KeyCode::Enter => {
                match self.selected_insert() {
                    Some(text) => MenuResult::Done(MenuPayload::Selected(text)),
                    None => {
                        if self.handler.allows_free_text() {
                            MenuResult::Cancel
                        } else {
                            MenuResult::Continue
                        }
                    }
                }
            }
            KeyCode::Esc => MenuResult::Cancel,
            _ => MenuResult::Continue,
        }
    }

    fn edit_field_value(&self) -> Option<&str> {
        Some(self.trigger.scan.prefix.as_str())
    }

    fn title(&self) -> &str {
        match self.trigger.scan.trigger_char {
            '@' => "File Mention",
            '/' => "Slash Command",
            '#' => "History",
            _ => "Dropdown",
        }
    }
}
```

---

## 7. `CommandResult::OpenMenu` Variant

File: `crates/xaft-tui/src/slash/mod.rs`

Add a new variant to `CommandResult`:

```rust
pub enum CommandResult {
    Lines(Vec<String>),
    StyledLines(Vec<StyledLine>),
    Handled,
    Error(String),
    ConfigDisplay(Vec<ConfigSection>),
    /// Open an interactive menu widget in the input area.
    ///
    /// `AppState::apply_command_result()` calls `self.menu_driver.open(widget)`
    /// and sets up a `on_menu_done` callback so the result payload can be
    /// acted on when the user commits or dismisses.
    OpenMenu(Box<dyn crate::menu::MenuWidget>),
}
```

`Box<dyn MenuWidget>` is `Send` because `MenuWidget: Send + Sync`.  This
allows slash command handlers to construct a widget in any context (including
`spawn_blocking`) and return it.

> **Error handling note**: `CommandResult` derives `Debug` but not `Clone`,
> because `Box<dyn MenuWidget>` is not `Clone`.  The existing `CommandResult`
> variants that are `Clone`-able continue to be so; callers that need to clone
> a `CommandResult::OpenMenu` must do so before opening (the widget is consumed
> by `MenuDriver::open()`).

---

## 8. `AppState` Changes

File: `crates/xaft-tui/src/state.rs`

### 8.1 New field

```rust
// In AppState struct, after the trigger system fields:

// ── Menu widget system ────────────────────────────────────────────────────────
/// Coordinates the single active interactive menu widget (if any).
///
/// When `menu_driver.is_active()`, all key events are routed to the driver
/// before normal dispatch.  A `CommandResult::OpenMenu` variant activates this.
pub menu_driver: MenuDriver,

/// Optional callback invoked when `menu_driver` produces `MenuResult::Done`.
///
/// Set by `apply_command_result()` when a slash command returns `OpenMenu`.
/// The callback receives the `MenuPayload` and typically dispatches a follow-up
/// action (e.g. changing the model, updating the config, submitting a form).
///
/// Cleared (set to `None`) after invocation, regardless of outcome.
pub on_menu_done: Option<Box<dyn FnOnce(MenuPayload, &mut AppState) + Send>>,
```

### 8.2 `AppState::new()` addition

```rust
// In AppState::new(), add to the struct literal:
menu_driver: MenuDriver::new(),
on_menu_done: None,
```

### 8.3 `handle_key()` routing

Add the following block **at the top of `handle_key()`**, before the escape
dialog check and before the trigger system dispatch:

```rust
// ── Menu widget system dispatch ──────────────────────────────────────────
if self.menu_driver.is_active() {
    if let Some(result) = self.menu_driver.handle_key(key) {
        match result {
            MenuResult::Continue => {
                // Widget consumed the key; redraw.
                self.mutations
                    .push(RenderMutation::UpdatePrompt(build_prompt(self)));
                return;
            }
            MenuResult::Done(payload) => {
                self.menu_driver.close();
                if let Some(cb) = self.on_menu_done.take() {
                    cb(payload, self);
                }
                self.mutations
                    .push(RenderMutation::UpdatePrompt(build_prompt(self)));
                return;
            }
            MenuResult::Cancel => {
                self.menu_driver.close();
                self.on_menu_done = None;
                self.mutations
                    .push(RenderMutation::UpdatePrompt(build_prompt(self)));
                return;
            }
        }
    }
    // If handle_key returned None (no active widget by the time we got here),
    // fall through to normal dispatch.
}
```

### 8.4 `apply_command_result()` — `OpenMenu` arm

```rust
/// Apply the result of a slash command execution to application state.
pub fn apply_command_result(&mut self, result: CommandResult) {
    match result {
        // ... existing arms unchanged ...

        CommandResult::OpenMenu(widget) => {
            self.menu_driver.open(widget);
            // on_menu_done is set by the slash command handler if it needs
            // a callback; it defaults to None (no-op on Done).
            self.mutations
                .push(RenderMutation::UpdatePrompt(build_prompt(self)));
        }
    }
}
```

When a slash command handler needs a follow-up action on `Done`, it should
set `AppState::on_menu_done` before returning.  The recommended pattern for
commands that want a callback is to accept a `&mut AppState` reference in their
`execute()` call (which they already have through `CommandContext`) and set
`state.on_menu_done` directly:

```rust
// Example inside a slash command handler's execute():
ctx.state.on_menu_done = Some(Box::new(|payload, state| {
    if let MenuPayload::Selected(model) = payload {
        state.xaft_config = Arc::new(state.xaft_config.with_model(model));
        // ... persist etc.
    }
}));
return Ok(CommandResult::OpenMenu(Box::new(ModelPickerWidget::new(models))));
```

---

## 9. `PromptState` and `build_prompt()` Changes

File: `crates/xaft-tui/src/prompt.rs`

### 9.1 New field on `PromptState`

```rust
pub struct PromptState {
    pub lines: Vec<String>,
    pub cursor: Cursor,
    pub scroll_top: usize,
    pub agent_active: bool,
    pub is_empty: bool,
    pub hidden_above: usize,
    pub hidden_below: usize,
    pub active_trigger: Option<ActiveTriggerSnapshot>,

    // NEW:
    /// `true` when `menu_driver.is_active()`.  The renderer uses this to
    /// reserve space below the input bar for the widget.
    pub menu_active: bool,
    /// Title of the active menu widget, for optional display in the chrome.
    pub menu_title: Option<String>,
}
```

### 9.2 `build_prompt()` addition

```rust
pub fn build_prompt(state: &AppState) -> PromptState {
    // ... existing logic unchanged ...

    PromptState {
        // ... existing fields unchanged ...
        menu_active: state.menu_driver.is_active(),
        menu_title: state.menu_driver.title().map(str::to_owned),
    }
}
```

---

## 10. `IncrementalRenderer` Changes

File: `crates/xaft-tui/src/renderer.rs`

### 10.1 Pass `menu_driver` into `draw_prompt_block()`

The renderer does not own `AppState`.  `draw_prompt_block()` is called from
`app.rs` with a `PromptState` snapshot.  To render the active menu widget, the
renderer also needs a mutable reference to `MenuDriver` (so it can call
`render()` which updates `last_rows`).

Change the signature of `draw_prompt_block()`:

```rust
/// Draw (or redraw) the prompt block, including any active menu widget.
///
/// `menu_driver` is `None` when called from a headless / test context with
/// no active widget.
pub fn draw_prompt_block(
    &mut self,
    prompt: &PromptState,
    theme: &Theme,
    menu_driver: Option<&mut MenuDriver>,
) -> io::Result<()> {
    // ... existing input bar rendering unchanged ...

    // After drawing the bottom border:
    if let Some(driver) = menu_driver {
        if driver.is_active() {
            let cols = self.term_cols;
            driver.render(&mut self.out, cols)?;
        }
    }

    // ... existing cursor-restore unchanged, but account for menu rows:
    let menu_rows = menu_driver
        .map(|d| d.last_rows())
        .unwrap_or(0);
    let trigger_rows = /* existing trigger_rows calculation */ 0usize; // unchanged
    queue!(
        self.out,
        cursor::MoveToColumn(0),
        cursor::MoveUp((rows_from_bottom + trigger_rows as u16 + menu_rows as u16)),
        cursor::MoveToColumn(col),
    )?;

    Ok(())
}
```

> The `menu_rows` are counted **after** the trigger dropdown rows in the
> cursor-up calculation because the menu widget renders below the trigger
> dropdown.  If both are active simultaneously (a trigger is open at the same
> time a menu is open), the combined height is `trigger_rows + menu_rows`.
> In practice, the `MenuDriver` check runs first in `handle_key()` and
> suppresses trigger key dispatch while a menu is active, but both can
> coexist visually.

### 10.2 `app.rs` call-site update

In `app.rs`, the call to `draw_prompt_block` passes `Some(&mut state.menu_driver)`:

```rust
renderer.draw_prompt_block(
    &prompt,
    &theme,
    Some(&mut state.menu_driver),
)?;
```

### 10.3 Height calculation

`draw_prompt_block` computes `total` (the number of rows the prompt block
occupies) for `last_prompt_height`.  Extend this to include menu rows:

```rust
let total = BORDER_ROWS as usize
    + (if show_indicator { 1 } else { 0 })
    + max_in_view
    + trigger_rows
    + menu_driver.map(|d| d.last_rows()).unwrap_or(0);
self.last_prompt_height = total as u16;
```

---

## 11. Key Differences from agenthicc PRD-41

> **Rust vs Python — what changed and why**

| Concern | agenthicc PRD-41 (Python) | xaft PRD-61 (Rust) |
|---|---|---|
| **Protocol vs trait** | `MenuWidget` is a Python `Protocol` — structural, duck-typed. Any class with the right methods satisfies it at runtime. | `MenuWidget` is a Rust `trait` — nominal, checked at compile time. Types must explicitly `impl MenuWidget`. No runtime reflection. |
| **Object safety** | Python protocols work naturally with lists of heterogeneous objects. | Rust trait objects (`Box<dyn MenuWidget>`) require object safety. `render(&mut impl TermWriter, ...)` is not object-safe, so a private `MenuWidgetExt` shim with `&mut dyn TermWriter` is required (§4.4, §5.3). |
| **Thread safety** | Python's GIL means thread safety is implicit (or ignored). | All `MenuWidget` impls must be `Send + Sync`. This is enforced at the `Box<dyn MenuWidget>` storage site. Widgets constructed in background threads (e.g. `spawn_blocking` slash handlers) can be sent to the TUI thread safely. |
| **Key event type** | agenthicc passes raw terminal key codes (`int` or `str`). | xaft uses crossterm's `KeyEvent` struct, which carries `KeyCode`, `KeyModifiers`, and `KeyEventKind`. Widgets pattern-match on `KeyEvent` — no raw byte handling. |
| **Output abstraction** | agenthicc writes to a Python `IO[bytes]`-compatible stream. | xaft uses the `TermWriter` trait (`Write + terminal_size()`), already defined in `renderer.rs`. `TestCapture` implements `TermWriter`, so widgets are testable without a real terminal. |
| **No runtime reflection** | Python can inspect method presence at runtime (`hasattr`). | The `edit_field_value()` and `title()` default methods use Rust's default trait method mechanism — no `Option<fn>` fields, no `Any` downcasting. |
| **Callback style** | agenthicc `MenuDriver` holds a Python callable for `on_done`. | xaft uses `Option<Box<dyn FnOnce(MenuPayload, &mut AppState) + Send>>` stored on `AppState`. The `FnOnce` bound prevents double-invocation. `Send` allows construction in async contexts. |
| **Error propagation** | agenthicc swallows render errors silently. | `render()` returns `io::Result<usize>`; `MenuDriver::render()` closes the widget on error, preserving the terminal in a consistent state. |

---

## 12. Tests

### 12.1 Unit tests — `crates/xaft-tui/src/menu/mod.rs`

All tests live in a `#[cfg(test)] mod tests` block.

**T-01 — `MenuDriver::new()` is inactive**
```
let driver = MenuDriver::new();
assert!(!driver.is_active());
assert_eq!(driver.last_rows(), 0);
assert!(driver.title().is_none());
```

**T-02 — `open()` activates the driver**
```
let mut driver = MenuDriver::new();
driver.open(Box::new(StubWidget::new("Test")));
assert!(driver.is_active());
assert_eq!(driver.title(), Some("Test"));
```

**T-03 — `close()` deactivates the driver**
```
driver.open(Box::new(StubWidget::new("Test")));
driver.close();
assert!(!driver.is_active());
assert_eq!(driver.last_rows(), 0);
```

**T-04 — `handle_key()` returns `None` when inactive**
```
let mut driver = MenuDriver::new();
let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
assert!(driver.handle_key(key).is_none());
```

**T-05 — `handle_key()` routes to active widget**
```
// StubWidget returns Done(MenuPayload::Empty) on Enter.
driver.open(Box::new(StubWidget::enter_done()));
let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
let result = driver.handle_key(key);
assert!(matches!(result, Some(MenuResult::Done(MenuPayload::Empty))));
```

**T-06 — `handle_key()` Cancel closes the widget**
```
// StubWidget returns Cancel on Esc.
driver.open(Box::new(StubWidget::esc_cancel()));
let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
let result = driver.handle_key(key);
assert!(matches!(result, Some(MenuResult::Cancel)));
// Driver still open after handle_key; AppState is responsible for calling close().
assert!(driver.is_active());
```

**T-07 — `render()` calls widget and updates `last_rows`**
```
let mut driver = MenuDriver::new();
driver.open(Box::new(StubWidget::rows(3)));
let mut cap = TestCapture::new(80, 24);
let rows = driver.render(&mut cap, 80).unwrap();
assert_eq!(rows, 3);
assert_eq!(driver.last_rows(), 3);
```

**T-08 — `render()` returns 0 when inactive**
```
let mut driver = MenuDriver::new();
let mut cap = TestCapture::new(80, 24);
assert_eq!(driver.render(&mut cap, 80).unwrap(), 0);
```

**T-09 — `render()` closes widget on error**
```
// StubWidget::error() returns Err from render_dyn.
driver.open(Box::new(StubWidget::error()));
let mut cap = TestCapture::new(80, 24);
let result = driver.render(&mut cap, 80);
assert!(result.is_err());
assert!(!driver.is_active());
```

**T-10 — `open()` replaces existing widget**
```
driver.open(Box::new(StubWidget::new("First")));
driver.open(Box::new(StubWidget::new("Second")));
assert_eq!(driver.title(), Some("Second"));
assert_eq!(driver.last_rows(), 0); // reset on open
```

### 12.2 Unit tests — `crates/xaft-tui/src/menu/dropdown.rs`

**T-11 — `DropdownWidget::handle_key()` Up moves selection**
```
let at = make_active_trigger(3 items, selected=1);
let mut w = DropdownWidget::new(at, handler.clone(), dir, 80);
w.handle_key(up_key());
assert_eq!(w.trigger.selected, 0);
```

**T-12 — `DropdownWidget::handle_key()` Down wraps around**
```
// 3 items, selected=2 → Down → selected=0.
w.handle_key(down_key());
assert_eq!(w.trigger.selected, 0);
```

**T-13 — `DropdownWidget::handle_key()` Enter returns `Done`**
```
let result = w.handle_key(enter_key());
assert!(matches!(result, MenuResult::Done(MenuPayload::Selected(_))));
```

**T-14 — `DropdownWidget::handle_key()` Esc returns `Cancel`**
```
assert!(matches!(w.handle_key(esc_key()), MenuResult::Cancel));
```

**T-15 — `DropdownWidget::render()` writes correct row count**
```
// 5 items, max_visible=4 → render writes 4 data rows + 1 hint = 5 rows.
let mut cap = TestCapture::new(80, 24);
let rows = w.render(&mut cap, 80, 0).unwrap();
assert_eq!(rows, 5);
```

**T-16 — `DropdownWidget::render()` highlights selected row**
```
// Plain-text output of the selected row contains a bold/yellow indicator
// (or at minimum contains the item display text at the right index).
let plain = cap.plain_text();
assert!(plain.contains(w.trigger.items[selected].display.as_str()));
```

**T-17 — `DropdownWidget::edit_field_value()` returns prefix**
```
assert_eq!(w.edit_field_value(), Some("ma")); // prefix from scan
```

**T-18 — `DropdownWidget::title()` for '@' trigger**
```
assert_eq!(w.title(), "File Mention");
```

### 12.3 Unit tests — `CommandResult::OpenMenu` integration

**T-19 — `apply_command_result(OpenMenu)` activates `menu_driver`**
```
let mut state = AppState::new_for_test();
state.apply_command_result(CommandResult::OpenMenu(
    Box::new(StubWidget::new("Picker")),
));
assert!(state.menu_driver.is_active());
assert_eq!(state.menu_driver.title(), Some("Picker"));
```

**T-20 — `handle_key()` routes to menu driver when active**
```
state.apply_command_result(CommandResult::OpenMenu(
    Box::new(StubWidget::esc_cancel()),
));
state.handle_key(esc_key());
assert!(!state.menu_driver.is_active());
```

**T-21 — `on_menu_done` callback fires on `Done`**
```
let called = Arc::new(AtomicBool::new(false));
let called_clone = Arc::clone(&called);
state.on_menu_done = Some(Box::new(move |_payload, _state| {
    called_clone.store(true, Ordering::SeqCst);
}));
state.apply_command_result(CommandResult::OpenMenu(
    Box::new(StubWidget::enter_done()),
));
state.handle_key(enter_key());
assert!(called.load(Ordering::SeqCst));
assert!(state.on_menu_done.is_none());
```

**T-22 — `on_menu_done` is cleared on `Cancel`**
```
state.on_menu_done = Some(Box::new(|_, _| {}));
state.apply_command_result(CommandResult::OpenMenu(
    Box::new(StubWidget::esc_cancel()),
));
state.handle_key(esc_key());
assert!(state.on_menu_done.is_none());
```

### 12.4 Integration tests — `crates/xaft-tui/tests/menu_integration.rs`

**T-23 — Full round-trip: slash command opens `DropdownWidget`, user selects, callback fires**

Setup:
1. Create a minimal `AppState::new_for_test()`.
2. Simulate a slash command result: `CommandResult::OpenMenu(Box::new(DropdownWidget::new(...)))`.
3. Call `state.apply_command_result(result)` — driver becomes active.
4. Simulate `KeyCode::Down` × 2 — selection advances.
5. Simulate `KeyCode::Enter` — driver produces `Done(MenuPayload::Selected("src/lib.rs"))`.
6. Callback writes selection to `state.task`.

Assertions:
- `state.menu_driver.is_active()` is `false` after step 5.
- `state.task` equals `"src/lib.rs"` after step 6.
- `state.on_menu_done` is `None` after step 6.

**T-24 — Render output includes menu rows in height accounting**

Setup:
1. Open a `StubWidget` that renders 4 rows.
2. Call `draw_prompt_block` with the driver.

Assertions:
- `renderer.last_prompt_height` equals `base_height + 4`.

**T-25 — Menu driver does not interfere with trigger system when inactive**

Setup:
1. `AppState` with `TriggerRegistry` containing `@` handler.
2. `menu_driver` inactive.
3. Type `@ma` into input bar.
4. Call `state.handle_key(down_key())`.

Assertions:
- `state.active_trigger` selection advances (trigger handled the key, not menu driver).
- `state.menu_driver.is_active()` remains `false`.

**T-26 — Menu driver takes priority over trigger when both active**

Setup:
1. `AppState` with `@` trigger active (`active_trigger = Some(...)`).
2. `menu_driver.open(Box::new(StubWidget::esc_cancel()))`.
3. Call `state.handle_key(esc_key())`.

Assertions:
- `state.menu_driver.is_active()` becomes `false` (menu consumed Esc).
- `state.active_trigger` is **unchanged** (menu driver intercepted the key before trigger dispatch).

---

## 13. Migration Path

The `DropdownWidget` adapter (§6) is intentionally non-destructive.  The
existing `AppState::active_trigger` / `TriggerHandler` system is **not**
replaced by this PRD.  A future PRD may unify both by:

1. Having `AppState::refresh_trigger()` construct a `DropdownWidget` and
   call `menu_driver.open()` instead of setting `active_trigger`.
2. Removing the `active_trigger` field once the `MenuDriver` path is
   validated in production.

Until that migration, `DropdownWidget` exists purely as proof-of-compatibility
and as the recommended approach for **new** interactive trigger dropdowns that
are opened programmatically (not by a typed trigger character).

---

## 14. Files to Add or Change

| File | Change type | Description |
|------|-------------|-------------|
| `crates/xaft-tui/src/menu/mod.rs` | **New** | `MenuWidget`, `MenuWidgetExt`, `DynTermWriter`, `MenuResult`, `MenuPayload`, `MenuDriver` |
| `crates/xaft-tui/src/menu/dropdown.rs` | **New** | `DropdownWidget` — `MenuWidget` adapter over `ActiveTrigger` |
| `crates/xaft-tui/src/lib.rs` | **Modify** | Add `pub mod menu;` |
| `crates/xaft-tui/src/slash/mod.rs` | **Modify** | Add `CommandResult::OpenMenu(Box<dyn crate::menu::MenuWidget>)` variant |
| `crates/xaft-tui/src/state.rs` | **Modify** | Add `menu_driver: MenuDriver` and `on_menu_done` fields; add menu-driver routing block in `handle_key()`; add `OpenMenu` arm in `apply_command_result()` |
| `crates/xaft-tui/src/prompt.rs` | **Modify** | Add `menu_active: bool` and `menu_title: Option<String>` to `PromptState`; update `build_prompt()` |
| `crates/xaft-tui/src/renderer.rs` | **Modify** | Add `menu_driver: Option<&mut MenuDriver>` parameter to `draw_prompt_block()`; call `driver.render()` below the bottom border; include `menu_rows` in height calculation and cursor-up |
| `crates/xaft-tui/src/app.rs` | **Modify** | Pass `Some(&mut state.menu_driver)` to `draw_prompt_block()` |
| `crates/xaft-tui/tests/menu_integration.rs` | **New** | Integration tests T-23 through T-26 |

---

## 15. Acceptance Criteria

| ID | Criterion |
|----|-----------|
| AC-01 | `MenuWidget`, `MenuResult`, `MenuPayload`, and `MenuDriver` are defined in `crates/xaft-tui/src/menu/mod.rs`. `cargo build -p xaft-tui` succeeds. |
| AC-02 | `DropdownWidget` is defined in `crates/xaft-tui/src/menu/dropdown.rs` and implements `MenuWidget`. |
| AC-03 | `CommandResult` has an `OpenMenu(Box<dyn MenuWidget>)` variant. `cargo build --workspace` succeeds. |
| AC-04 | `AppState` has `menu_driver: MenuDriver`. `AppState::new()` initialises it as `MenuDriver::new()`. |
| AC-05 | When `menu_driver.is_active()`, all key events route to `menu_driver.handle_key()` before trigger dispatch and before normal dispatch. |
| AC-06 | `apply_command_result(CommandResult::OpenMenu(w))` calls `menu_driver.open(w)`. |
| AC-07 | On `MenuResult::Done`, `AppState` calls `on_menu_done.take()` (if set), then closes the driver. |
| AC-08 | On `MenuResult::Cancel`, `AppState` closes the driver and clears `on_menu_done`. |
| AC-09 | `draw_prompt_block()` renders the active widget below the bottom border and includes its row count in `last_prompt_height`. |
| AC-10 | `PromptState` has `menu_active: bool`. `build_prompt()` sets it from `state.menu_driver.is_active()`. |
| AC-11 | All unit tests T-01 through T-22 pass (`cargo test -p xaft-tui`). |
| AC-12 | All integration tests T-23 through T-26 pass (`cargo test -p xaft-tui --test menu_integration`). |
| AC-13 | `cargo clippy --workspace` produces no new warnings. All new types derive `Debug`. |
| AC-14 | `MenuWidget` implementors need zero knowledge of `MenuWidgetExt` or `DynTermWriter` — they are private to `menu/mod.rs`. |
| AC-15 | A `DropdownWidget` round-trip (open → Down × 2 → Enter) produces `MenuResult::Done(MenuPayload::Selected(s))` where `s` equals the handler's `on_select()` return value for the selected item. |
