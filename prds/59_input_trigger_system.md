---
title: "PRD-59: Input Trigger System — Generalised Dropdown Architecture"
status: draft
version: 0.1.0
created: 2026-06-13
analog: python/agenthicc/prds/prd-39-input-trigger-system.md
---

# PRD-59: Input Trigger System

## Executive Summary

The xaft TUI has two trigger-character systems hardcoded into `AppState`:
`@`-mention autocomplete and the `/` slash-command palette. Both work, but
each was bolted in independently. Every field, method, and rendering path is
duplicated. Adding a third trigger (e.g. `#` for history recall, `:` for
emoji insert) requires forking all that boilerplate again. This PRD defines
the **Input Trigger System** — a registry-based architecture where each
trigger character is an independent handler implementing a single trait, and
`AppState` keeps exactly one `Option<ActiveTrigger>` regardless of how many
handlers are registered.

---

## 1. Problem Statement

### 1.1 Structural duplication in `AppState`

`crates/xaft-tui/src/state.rs` carries two completely separate trigger
subsystems as struct fields:

```rust
// state.rs — current, two independent Option<T> fields
pub mention_autocomplete: Option<AutocompleteDropdown>,  // line 147
pub slash_palette: Option<SlashPalette>,                 // line 179
```

Each has its own state type with no shared interface:

- `AutocompleteDropdown` (defined in `prompt.rs`) holds `prefix`, `dir_prefix`,
  `candidates: Vec<String>`, and `selected: usize`.
- `SlashPalette` (defined in `slash/palette.rs`) holds `partial`, `candidates`,
  `selected`, `scroll_top`, `descriptions`, and `args_hints` — six fields, three
  of which are parallel `Vec`s.

### 1.2 Duplicated refresh methods

Two methods repopulate state on every keystroke. They share zero code:

```rust
// state.rs line 448
pub fn refresh_autocomplete(&mut self) {
    // Scan backwards for '@' with no whitespace, list workspace dir,
    // populate self.mention_autocomplete ...  ~80 lines
}

// state.rs line 960
fn refresh_slash_palette(&mut self) {
    // Check if first line starts with '/', populate self.slash_palette
    // ... ~10 lines (but calls SlashPalette::new which is ~30 more)
}
```

Both are called on every `InputAction::BufferChanged | InputAction::CursorMoved`
(state.rs line 2197–2198):

```rust
InputAction::BufferChanged | InputAction::CursorMoved => {
    self.refresh_autocomplete();
    self.refresh_slash_palette();
    // ...
}
```

A third trigger would add a third call here, a third `Option<T>` field, a
third `refresh_X()` method.

### 1.3 Guard-clause priority coupling in `handle_key()`

The key-dispatch section of `handle_key()` (state.rs lines 2011–2151) uses
explicit `if self.X.is_some() { ... return; }` guards that create an
accidental priority ordering:

```rust
// state.rs line 2011 — @-mention takes priority
if self.mention_autocomplete.is_some() {
    match key.code {
        KeyCode::Up     => { self.autocomplete_prev();     return; }
        KeyCode::Down   => { self.autocomplete_next();     return; }
        KeyCode::Tab
        | KeyCode::Enter => { self.autocomplete_complete(); return; }
        KeyCode::Esc    => { self.mention_autocomplete = None; return; }
        _ => {}
    }
}

// (approval queue check at line 2043 — unrelated)

// state.rs line 2082 — slash palette second
if self.slash_palette.is_some() {
    match key.code {
        KeyCode::Up    => { p.select_prev(); return; }
        KeyCode::Down  => { p.select_next(); return; }
        KeyCode::Tab   => { /* complete */ return; }
        KeyCode::Enter => { /* execute */   return; }
        KeyCode::Esc   => { self.slash_palette = None; return; }
        _ => {}
    }
}
```

Both blocks are ~35 lines of almost identical match arms. Both can be open
simultaneously (both `Option`s are `Some`) if the user types `@/path` — the
`@` guard fires first and swallows keys the user intended for the slash
palette, silently breaking expected behaviour.

### 1.4 Duplicated rendering

`crates/xaft-tui/src/renderer.rs` has two separate rendering paths for the
two dropdowns, both inlined in `draw_prompt_block()` (renderer.rs lines 691–798):

```rust
// renderer.rs line 691 — @-mention dropdown
if let Some(ref ac) = prompt.autocomplete {
    for (i, candidate) in ac.candidates.iter().take(10).enumerate() {
        // render each row ...
    }
}

// renderer.rs line 723 — slash palette dropdown
if let Some(ref sp) = prompt.slash_palette {
    for (i, row) in sp.rows.iter().skip(sp.scroll_top).take(palette_show).enumerate() {
        // different column layout, different colors, different indicator glyph ...
    }
    // render hint line
}
```

The height calculation above these blocks (renderer.rs lines 577–600) tracks
`ac_rows` and `palette_row_count` as separate variables and sums them to compute
`last_prompt_height`. A third trigger requires yet more separate accounting.

### 1.5 Duplicated `PromptState` fields

`crates/xaft-tui/src/prompt.rs` carries both trigger snapshots:

```rust
pub struct PromptState {
    // ... other fields ...
    pub autocomplete: Option<AutocompleteDropdown>,       // line ~68
    pub slash_palette: Option<SlashPaletteSnapshot>,      // line ~73
}
```

`build_prompt()` fills both from `AppState`, with separate conversion logic for
each (prompt.rs lines ~133–157).

### 1.6 Impact of adding a third trigger

Adding a `#history-recall` trigger currently requires:

1. A new state struct (e.g. `HistoryDropdown`) in a new or existing file.
2. A new `Option<HistoryDropdown>` field on `AppState`.
3. A new `refresh_history_dropdown(&mut self)` method on `AppState`.
4. A new `if self.history_dropdown.is_some() { ... }` guard block in `handle_key()`.
5. A new `Option<HistoryDropdown>` field on `PromptState`.
6. A new conversion arm in `build_prompt()`.
7. A new `Option<HistorySnapshot>` field on `PromptState` (renderer-side).
8. A new rendering section in `draw_prompt_block()`.
9. Manual height accounting in the block-height calculation.
10. Separate test coverage for each new piece.

Ten touch points for what is semantically a one-handler registration.

---

## 2. Goals

| ID | Goal |
|----|------|
| G1 | Any single character can be registered as a trigger by implementing `TriggerHandler` and calling `TriggerRegistry::register()` |
| G2 | Each trigger handler is an independent struct — zero shared mutable state between handlers |
| G3 | `handle_key()` has a single trigger-dispatch block (`if let Some(ref mut at) = self.active_trigger`) regardless of which trigger is active |
| G4 | `refresh_autocomplete()` and `refresh_slash_palette()` collapse into a single `refresh_trigger()` method |
| G5 | `AppState` carries `active_trigger: Option<ActiveTrigger>` instead of `mention_autocomplete` and `slash_palette` |
| G6 | `PromptState` carries `active_trigger: Option<ActiveTriggerSnapshot>` instead of `autocomplete` and `slash_palette` |
| G7 | `draw_prompt_block()` calls a single `draw_trigger_dropdown()` helper instead of two inline rendering sections |
| G8 | Adding a new trigger (`#`, `:`, `!`) requires zero changes to `handle_key()`, `refresh_trigger()`, `build_prompt()`, or `draw_prompt_block()` |
| G9 | `@`-mention and slash-command behaviour is preserved with no regression — same keybindings, same visual layout, same insertion logic |
| G10 | Handlers are `Send + Sync` and cheaply `Arc`-clonable so they can be shared across async boundaries |

## Non-Goals

- Nested triggers (a trigger opening a sub-trigger)
- Multi-character trigger sequences (e.g. `##`)
- Mouse click selection in the dropdown
- Fuzzy matching engine (handlers implement their own matching)
- Trigger-specific configuration exposed to TOML (handlers read their own config at construction time)

---

## 3. Core Types

All new types live in a new module: `crates/xaft-tui/src/trigger/mod.rs`.

### 3.1 `MatchItem`

A single result row returned by any trigger handler.

```rust
// crates/xaft-tui/src/trigger/mod.rs

/// A single result row in the trigger dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchItem {
    /// Text displayed in the left column of the dropdown row.
    /// For `@`-mention: `"src/main.rs"`. For slash: `"/clear  Clear the transcript"`.
    pub display: String,

    /// Text inserted into the input buffer when this item is selected.
    /// May differ from `display` (e.g. display shows the full path but insert
    /// is just `"@src/main.rs "`).
    pub insert: String,

    /// Optional hint shown below the dropdown (e.g. argument syntax for `/model`).
    /// When `Some`, an extra line is appended after the last row.
    pub hint: Option<String>,

    /// Semantic category, used to select rendering style.
    pub kind: MatchKind,
}

/// Semantic category for a `MatchItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchKind {
    /// A regular file (shown without trailing `/`).
    File,
    /// A directory (shown with trailing `/`).
    Directory,
    /// A slash command (e.g. `/clear`, `/model`).
    Command,
    /// An emoji or symbol (e.g. from `:` trigger).
    Emoji,
    /// Application-defined category. The `String` payload is used as a label.
    Custom(String),
}
```

### 3.2 `TriggerContext`

Read-only context passed to every handler method. Handlers must not mutate
application state through this struct.

```rust
/// Read-only context available to trigger handlers on every refresh.
pub struct TriggerContext<'a> {
    /// Everything the user typed after the trigger character up to the cursor,
    /// with no leading trigger char. E.g. if the buffer contains `"foo @src/ma"`
    /// and the cursor is at the end, `prefix` is `"src/ma"`.
    pub prefix: &'a str,

    /// Directory component of `prefix` — everything up to and including the
    /// last `/`. Empty string (`""`) means the workspace root. Used by
    /// path-aware handlers like `MentionTriggerHandler`.
    pub dir_prefix: &'a str,

    /// Absolute path to the session working directory.
    pub working_dir: &'a std::path::Path,

    /// Current terminal width in columns. Handlers may use this to truncate
    /// display strings.
    pub terminal_cols: u16,
}
```

### 3.3 `TriggerHandler` trait

```rust
/// Protocol every trigger handler must implement.
///
/// Handlers are stored as `Arc<dyn TriggerHandler>` in the registry and
/// must therefore be `Send + Sync`. All methods take `&self` — no mutation
/// of handler state is permitted; any caching must be interior-mutable and
/// thread-safe.
pub trait TriggerHandler: Send + Sync {
    /// The single character that activates this handler.
    ///
    /// Must be unique across all registered handlers. The registry enforces
    /// this at `register()` time.
    fn trigger_char(&self) -> char;

    /// Return the list of matching rows for the current `prefix`.
    ///
    /// Called on every `InputAction::BufferChanged` or `InputAction::CursorMoved`
    /// while this trigger is active. Should be cheap (no async I/O). Return an
    /// empty `Vec` to show a "no matches" state (dropdown hidden or greyed out,
    /// handler's choice).
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem>;

    /// Return the string to insert into the buffer when `item` is selected
    /// (Tab / Enter). The returned string replaces everything from the trigger
    /// character position to the current cursor.
    ///
    /// Default: return `item.insert.clone()`.
    fn on_select(&self, item: &MatchItem, ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// Whether the user can submit the raw typed text even when it doesn't
    /// match any item (Enter without a selection dismisses the dropdown and
    /// sends the raw text as-is).
    ///
    /// `true` for the slash-command handler (bare `/` with no match submits
    /// the literal text to the agent). `false` for the `@`-mention handler
    /// (Enter picks the highlighted candidate or is a no-op when the dropdown
    /// is open).
    fn allows_free_text(&self) -> bool {
        false
    }

    /// Maximum number of rows to show in the dropdown at once.
    ///
    /// Default: `8`.
    fn max_visible(&self) -> usize {
        8
    }

    /// When `true`, the suggestion is rendered inline with the cursor rather
    /// than in a dropdown below the bottom border.
    ///
    /// Reserved for future use (e.g. ghost-text completion). Default: `false`.
    fn is_inline(&self) -> bool {
        false
    }
}
```

### 3.4 `TriggerRegistry`

```rust
use indexmap::IndexMap;
use std::sync::Arc;

/// Registry of all registered trigger handlers, keyed by trigger character.
///
/// Built once in `AppState::new()` and stored as a plain (non-`Arc`) field.
/// Handlers themselves are `Arc`-wrapped so they can be shared cheaply.
pub struct TriggerRegistry {
    /// Preserves insertion order for deterministic iteration.
    handlers: IndexMap<char, Arc<dyn TriggerHandler>>,
}

impl TriggerRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: IndexMap::new(),
        }
    }

    /// Register a handler. Panics if a handler for `handler.trigger_char()`
    /// is already registered (programming error, not a runtime condition).
    pub fn register(&mut self, handler: Arc<dyn TriggerHandler>) {
        let ch = handler.trigger_char();
        if self.handlers.contains_key(&ch) {
            panic!(
                "TriggerRegistry: duplicate handler for trigger char {:?}",
                ch
            );
        }
        self.handlers.insert(ch, handler);
    }

    /// Look up the handler for a trigger character, if registered.
    pub fn get(&self, ch: char) -> Option<&Arc<dyn TriggerHandler>> {
        self.handlers.get(&ch)
    }

    /// Scan `line` left of `cursor_col` for the nearest registered trigger
    /// character that is followed by no whitespace up to the cursor.
    ///
    /// Returns `None` when the cursor is not immediately after a trigger token,
    /// or when the trigger character found has no registered handler.
    ///
    /// Only the *rightmost* eligible trigger character is considered. When the
    /// user types `"git @src/ma"`, the `@` is found because there is no
    /// whitespace between it and the cursor. When they type `"@ foo"`, the space
    /// after `@` breaks the token and `None` is returned.
    pub fn active_for(
        &self,
        line: &str,
        cursor_col: usize,
    ) -> Option<ActiveTriggerScan> {
        let slice = &line[..cursor_col.min(line.len())];

        // Walk backwards to find the rightmost registered trigger char
        // with no intervening whitespace.
        for (byte_pos, ch) in slice.char_indices().rev() {
            if ch.is_whitespace() {
                // Whitespace before finding a trigger char — stop.
                break;
            }
            if self.handlers.contains_key(&ch) {
                let after = &slice[byte_pos + ch.len_utf8()..];
                // Guard: there must be no whitespace between the trigger
                // char and the cursor (already guaranteed by the loop above,
                // but kept explicit for clarity).
                if after.contains(char::is_whitespace) {
                    break;
                }
                let prefix = after.to_owned();
                let dir_prefix = match prefix.rfind('/') {
                    Some(slash) => prefix[..slash + 1].to_owned(),
                    None => String::new(),
                };
                return Some(ActiveTriggerScan {
                    trigger_char: ch,
                    prefix,
                    dir_prefix,
                    trigger_byte_pos: byte_pos,
                });
            }
        }
        None
    }
}

impl Default for TriggerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

### 3.5 `ActiveTriggerScan`

```rust
/// Result of a successful scan for a trigger character in the current input.
///
/// Produced by `TriggerRegistry::active_for()` and stored inside
/// `ActiveTrigger`.
#[derive(Debug, Clone)]
pub struct ActiveTriggerScan {
    /// The trigger character found (e.g. `'@'`, `'/'`).
    pub trigger_char: char,

    /// Everything the user typed after the trigger character up to the cursor.
    /// Empty string means the cursor is immediately after the trigger char.
    pub prefix: String,

    /// Directory portion of `prefix` — everything up to and including the
    /// last `/`. Empty string means the workspace root.
    pub dir_prefix: String,

    /// Byte offset of the trigger character within the current line.
    /// Used by `on_select` insertion logic to replace the right slice.
    pub trigger_byte_pos: usize,
}
```

### 3.6 `ActiveTrigger`

```rust
/// Live state for the currently-open trigger dropdown.
///
/// `AppState` holds at most one `Option<ActiveTrigger>` at a time.
/// Opening a new trigger (by typing a different trigger char) replaces the
/// existing one.
#[derive(Debug, Clone)]
pub struct ActiveTrigger {
    /// Scan result that produced this active trigger.
    pub scan: ActiveTriggerScan,

    /// Items returned by the handler's most recent `matches()` call.
    pub items: Vec<MatchItem>,

    /// Index of the currently highlighted item (0-based, wraps).
    pub selected: usize,

    /// Index of the first visible item (scroll offset).
    pub scroll_top: usize,
}

impl ActiveTrigger {
    /// The currently highlighted `MatchItem`, if any.
    pub fn selected_item(&self) -> Option<&MatchItem> {
        self.items.get(self.selected)
    }

    /// Advance selection to the next item (wraps around). Adjusts `scroll_top`
    /// to keep the selected row visible within `max_visible` rows.
    pub fn select_next(&mut self, max_visible: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.clamp_scroll(max_visible);
    }

    /// Move selection to the previous item (wraps around). Adjusts `scroll_top`.
    pub fn select_prev(&mut self, max_visible: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or(self.items.len() - 1);
        self.clamp_scroll(max_visible);
    }

    fn clamp_scroll(&mut self, max_visible: usize) {
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        } else if self.selected >= self.scroll_top + max_visible {
            self.scroll_top = self.selected + 1 - max_visible;
        }
    }
}
```

### 3.7 `ActiveTriggerSnapshot` (renderer-side)

```rust
// crates/xaft-tui/src/prompt.rs

/// Renderer-visible snapshot of the active trigger dropdown.
/// Produced by `build_prompt()` from `AppState::active_trigger`.
/// Contains no `Arc`s and no references — fully owned.
#[derive(Debug, Clone)]
pub struct ActiveTriggerSnapshot {
    /// Which trigger character is active (used to select rendering style).
    pub trigger_char: char,

    /// Items to display (cloned from `ActiveTrigger::items`).
    pub items: Vec<crate::trigger::MatchItem>,

    /// Index of the highlighted item.
    pub selected: usize,

    /// First visible item index (scroll offset).
    pub scroll_top: usize,

    /// Maximum number of rows to show (from `TriggerHandler::max_visible()`).
    pub max_visible: usize,

    /// Hint text to show below the last visible row (from the selected item's
    /// `MatchItem::hint` field, or `None`).
    pub hint: Option<String>,
}
```

---

## 4. `AppState` Changes

File: `crates/xaft-tui/src/state.rs`

### 4.1 Fields to remove

```rust
// REMOVE:
pub mention_autocomplete: Option<AutocompleteDropdown>,   // line 147
pub slash_palette: Option<SlashPalette>,                  // line 179
```

### 4.2 Fields to add

```rust
// ADD (after `mention_config` / in the slash-command section):

/// Registry of all registered trigger handlers, built once in `new()`.
pub trigger_registry: TriggerRegistry,

/// Live state for the currently-open trigger dropdown, or `None` when no
/// trigger is active. Replaces the former `mention_autocomplete` and
/// `slash_palette` fields.
pub active_trigger: Option<ActiveTrigger>,
```

### 4.3 `new()` construction

```rust
// In AppState::new(), replace:
//   mention_autocomplete: None,
//   slash_palette: None,
// with:

let mut trigger_registry = TriggerRegistry::new();
trigger_registry.register(Arc::new(MentionTriggerHandler::new(
    /* workspace injected later via init_mention() */
    MentionConfig::default(),
)));
trigger_registry.register(Arc::new(SlashCommandTriggerHandler::new(Arc::clone(
    &slash_registry,
))));

// ... later in the struct literal:
trigger_registry,
active_trigger: None,
```

The `MentionTriggerHandler` is constructed with a placeholder workspace;
`init_mention()` replaces it (see §4.5).

### 4.4 `refresh_trigger()` — replaces both refresh methods

```rust
/// Scan the input bar at the current cursor position. If the cursor is
/// immediately after a registered trigger character with no whitespace
/// between the char and the cursor, call the handler's `matches()` and
/// populate `active_trigger`. Clears `active_trigger` otherwise.
///
/// Replaces `refresh_autocomplete()` and `refresh_slash_palette()`.
pub fn refresh_trigger(&mut self) {
    let cursor = self.input_bar.cursor();
    let lines = self.input_bar.lines();

    // Slash-command trigger ('/') is only valid on a single-line buffer
    // that starts with '/'. All other triggers scan the cursor's line normally.
    let line = match lines.get(cursor.row) {
        Some(l) => l.as_str(),
        None => {
            self.active_trigger = None;
            return;
        }
    };

    // Scan the current line for the nearest eligible trigger char.
    let Some(scan) = self.trigger_registry.active_for(line, cursor.col) else {
        self.active_trigger = None;
        return;
    };

    // Special case: '/' trigger only activates on a single-line buffer
    // whose first (and only) line starts with '/'.
    if scan.trigger_char == '/'
        && (lines.len() > 1 || !line.starts_with('/'))
    {
        self.active_trigger = None;
        return;
    }

    let handler = match self.trigger_registry.get(scan.trigger_char) {
        Some(h) => Arc::clone(h),
        None => {
            self.active_trigger = None;
            return;
        }
    };

    let ctx = TriggerContext {
        prefix: &scan.prefix,
        dir_prefix: &scan.dir_prefix,
        working_dir: &self.working_dir,
        terminal_cols: self.terminal_size.0,
    };

    let items = handler.matches(&ctx);

    if items.is_empty() && !handler.allows_free_text() {
        self.active_trigger = None;
        return;
    }

    // Preserve selected index when only the prefix changed (same trigger char
    // and same total item count — avoids jumping on every keystroke).
    let selected = match &self.active_trigger {
        Some(prev) if prev.scan.trigger_char == scan.trigger_char => {
            prev.selected.min(items.len().saturating_sub(1))
        }
        _ => 0,
    };
    let scroll_top = match &self.active_trigger {
        Some(prev) if prev.scan.trigger_char == scan.trigger_char => prev.scroll_top,
        _ => 0,
    };

    self.active_trigger = Some(ActiveTrigger {
        scan,
        items,
        selected,
        scroll_top,
    });
}
```

### 4.5 `init_mention()` — update to notify the handler

```rust
pub fn init_mention(
    &mut self,
    workspace: Arc<dyn WorkspaceStore>,
    workspace_root: PathBuf,
    config: MentionConfig,
    signals: Option<Arc<SignalBus>>,
) {
    self.mention_workspace = Some(Arc::clone(&workspace));
    self.mention_workspace_root = Some(workspace_root.clone());
    self.mention_config = config.clone();
    self.signal_bus = signals;

    // Re-register the MentionTriggerHandler now that we have the workspace.
    // The registry allows replacement only through this dedicated path.
    let handler = Arc::new(MentionTriggerHandler::with_workspace(
        workspace,
        workspace_root,
        config,
    ));
    self.trigger_registry.replace('@', handler);
}
```

`TriggerRegistry::replace()` is a new method that overwrites an existing
handler without panicking:

```rust
impl TriggerRegistry {
    /// Replace the handler for `ch`. Panics if no handler for `ch` is
    /// currently registered (use `register` for new chars).
    pub fn replace(&mut self, ch: char, handler: Arc<dyn TriggerHandler>) {
        assert!(
            self.handlers.contains_key(&ch),
            "TriggerRegistry::replace: no handler for {:?}",
            ch
        );
        self.handlers.insert(ch, handler);
    }
}
```

### 4.6 `handle_key()` — unified trigger dispatch

Replace the two separate `if self.mention_autocomplete.is_some()` and
`if self.slash_palette.is_some()` blocks (state.rs lines 2011–2151) with:

```rust
// Single unified trigger-dispatch block.
// Runs before approval-queue handling, after escape-dialog handling.
if self.active_trigger.is_some() {
    let handled = self.handle_trigger_key(key);
    if handled {
        self.mutations
            .push(RenderMutation::UpdatePrompt(build_prompt(self)));
        return;
    }
    // Fall through for unhandled keys (e.g. printable chars).
}
```

With the dispatch extracted into:

```rust
/// Handle a keypress while a trigger dropdown is open.
/// Returns `true` if the key was consumed by the trigger system.
fn handle_trigger_key(&mut self, key: KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    let Some(ref at) = self.active_trigger else {
        return false;
    };
    let trigger_char = at.scan.trigger_char;
    let handler = match self.trigger_registry.get(trigger_char) {
        Some(h) => Arc::clone(h),
        None => return false,
    };
    let max_visible = handler.max_visible();

    match key.code {
        KeyCode::Up => {
            if let Some(ref mut at) = self.active_trigger {
                at.select_prev(max_visible);
            }
            true
        }
        KeyCode::Down => {
            if let Some(ref mut at) = self.active_trigger {
                at.select_next(max_visible);
            }
            true
        }
        KeyCode::Tab => {
            self.trigger_complete();
            true
        }
        KeyCode::Enter => {
            if handler.allows_free_text() {
                // Submit the highlighted command (or fall through to submit
                // raw text when the dropdown is empty).
                if let Some(ref at) = self.active_trigger {
                    if let Some(item) = at.selected_item() {
                        let insert = item.insert.clone();
                        self.active_trigger = None;
                        // Treat the completed text as a submit.
                        self.input_bar.set_text(&insert);
                        let action = self.input_bar.submit();
                        if let InputAction::Submit(msg) = action {
                            self.handle_submit(msg);
                        }
                        return true;
                    }
                }
                // No candidate — dismiss and let the input bar submit.
                self.active_trigger = None;
                false
            } else {
                // For non-free-text handlers (e.g. @-mention), Enter = Tab.
                self.trigger_complete();
                true
            }
        }
        KeyCode::Esc => {
            self.active_trigger = None;
            true
        }
        _ => false,
    }
}

/// Apply the currently-selected trigger item to the input buffer.
fn trigger_complete(&mut self) {
    let (scan, item) = match &self.active_trigger {
        Some(at) => match at.selected_item() {
            Some(item) => (at.scan.clone(), item.clone()),
            None => return,
        },
        None => return,
    };
    let handler = match self.trigger_registry.get(scan.trigger_char) {
        Some(h) => Arc::clone(h),
        None => return,
    };

    let cursor = self.input_bar.cursor();
    let lines = self.input_bar.lines().to_vec();
    let line = match lines.get(cursor.row) {
        Some(l) => l.clone(),
        None => return,
    };
    let col = cursor.col.min(line.len());

    // Build the context so the handler can compute its insertion string.
    let ctx = TriggerContext {
        prefix: &scan.prefix,
        dir_prefix: &scan.dir_prefix,
        working_dir: &self.working_dir,
        terminal_cols: self.terminal_size.0,
    };
    let insertion = handler.on_select(&item, &ctx);

    // Replace from trigger_byte_pos to cursor position with the insertion.
    let new_line = format!(
        "{}{}{}",
        &line[..scan.trigger_byte_pos],
        insertion,
        &line[col..]
    );
    let mut all_lines = lines;
    all_lines[cursor.row] = new_line;
    self.input_bar.set_text(&all_lines.join("\n"));
    let new_col = scan.trigger_byte_pos + insertion.len();
    self.input_bar.set_cursor(cursor.row, new_col);

    self.active_trigger = None;

    // For directory items, immediately re-scan so the next level appears.
    if item.kind == MatchKind::Directory {
        self.refresh_trigger();
    }
}
```

### 4.7 BufferChanged / CursorMoved update

```rust
// In handle_key(), after input_bar.handle_key():
InputAction::BufferChanged | InputAction::CursorMoved => {
    // Single call replaces refresh_autocomplete() + refresh_slash_palette().
    self.refresh_trigger();
    self.mutations
        .push(RenderMutation::UpdatePrompt(build_prompt(self)));
    true
}
```

---

## 5. `PromptState` Changes

File: `crates/xaft-tui/src/prompt.rs`

### 5.1 Types to remove

```rust
// REMOVE:
pub struct SlashPaletteRow { ... }
pub struct SlashPaletteSnapshot { ... }
// (AutocompleteDropdown stays only if used elsewhere; if used only in
//  PromptState, remove it too. See §7 for the handler migration.)
```

### 5.2 `PromptState` field replacement

```rust
pub struct PromptState {
    // ... existing fields unchanged ...
    pub lines: Vec<String>,
    pub cursor: Cursor,
    pub scroll_top: usize,
    pub agent_active: bool,
    pub is_empty: bool,
    pub hidden_above: usize,
    pub hidden_below: usize,

    // REPLACES: autocomplete + slash_palette
    /// Snapshot of the active trigger dropdown for this render frame,
    /// or `None` when no trigger is open.
    pub active_trigger: Option<ActiveTriggerSnapshot>,
}
```

### 5.3 `build_prompt()` — unified snapshot construction

```rust
pub fn build_prompt(state: &AppState) -> PromptState {
    // ... existing scroll/hidden_above/hidden_below calculation unchanged ...

    let active_trigger = state.active_trigger.as_ref().and_then(|at| {
        let handler = state.trigger_registry.get(at.scan.trigger_char)?;
        let hint = at.selected_item().and_then(|item| item.hint.clone());
        Some(ActiveTriggerSnapshot {
            trigger_char: at.scan.trigger_char,
            items: at.items.clone(),
            selected: at.selected,
            scroll_top: at.scroll_top,
            max_visible: handler.max_visible(),
            hint,
        })
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
    }
}
```

---

## 6. Rendering Changes

File: `crates/xaft-tui/src/renderer.rs`

### 6.1 Height calculation

Replace the separate `ac_rows` + `palette_row_count` variables with a single
`trigger_rows` calculation:

```rust
// In draw_prompt_block(), replace:
//   let ac_rows = prompt.autocomplete.as_ref().map(...).unwrap_or(0);
//   let palette_show = prompt.slash_palette.as_ref().map(...).unwrap_or(0);
//   let palette_row_count = if palette_show == 0 { 0 } else { palette_show + 1 };
// with:

let trigger_rows = prompt
    .active_trigger
    .as_ref()
    .map(|snap| {
        let visible = snap
            .items
            .len()
            .saturating_sub(snap.scroll_top)
            .min(snap.max_visible);
        let hint_row = if snap.hint.is_some() { 1 } else { 0 };
        visible + hint_row
    })
    .unwrap_or(0);

let total = BORDER_ROWS as usize
    + (if show_indicator { 1 } else { 0 })
    + max_in_view
    + trigger_rows;
self.last_prompt_height = total as u16;
```

### 6.2 `draw_trigger_dropdown()`

Replace both inline rendering sections (renderer.rs lines 691–798) with a
call to a new helper:

```rust
// After drawing the bottom border:
if prompt.active_trigger.is_some() {
    self.draw_trigger_dropdown(prompt, theme)?;
}
```

The helper:

```rust
/// Render the active trigger dropdown below the bottom border.
///
/// Supports two visual modes keyed by `trigger_char`:
///
/// - `'@'` (file-mention mode): `" + <candidate>"` rows, no trigger prefix.
/// - `'/'` (command mode): `"▶/command  description  <args>"` rows with
///   a 12-char padded trigger column, a description column, and an optional
///   args-hint column.
/// - All other trigger chars: `"▶ <display>"` rows (generic mode).
///
/// A hint line is appended after the last visible row when `snap.hint` is `Some`.
fn draw_trigger_dropdown(
    &mut self,
    prompt: &PromptState,
    theme: &Theme,
) -> io::Result<()> {
    let Some(ref snap) = prompt.active_trigger else {
        return Ok(());
    };
    let visible_count = snap
        .items
        .len()
        .saturating_sub(snap.scroll_top)
        .min(snap.max_visible);

    for (offset, item) in snap
        .items
        .iter()
        .skip(snap.scroll_top)
        .take(visible_count)
        .enumerate()
    {
        let abs_index = snap.scroll_top + offset;
        let is_selected = abs_index == snap.selected;

        let row_text = self.format_trigger_row(snap.trigger_char, item, theme);
        let max_len = self.term_cols.saturating_sub(1) as usize;
        let display: String = row_text.chars().take(max_len).collect();

        queue!(self.out, Print("\r\n"))?;
        if is_selected {
            queue!(
                self.out,
                SetForegroundColor(theme.warning),
                SetAttribute(Attribute::Bold),
                Print(&display),
                SetAttribute(Attribute::Reset),
            )?;
        } else {
            queue!(
                self.out,
                SetForegroundColor(theme.fg),
                Print(&display),
                SetAttribute(Attribute::Reset),
            )?;
        }
    }

    // Hint line (navigation help or item-specific hint).
    let hint_text = snap.hint.as_deref().unwrap_or(match snap.trigger_char {
        '/' => "  ↑/↓ navigate · Tab complete · Enter execute · Esc dismiss",
        '@' => "", // no hint line for @-mention (matches current behaviour)
        _ => "  ↑/↓ navigate · Tab/Enter select · Esc dismiss",
    });
    if !hint_text.is_empty() {
        queue!(
            self.out,
            Print("\r\n"),
            SetForegroundColor(theme.dim),
            Print(hint_text),
            SetAttribute(Attribute::Reset),
        )?;
    }

    Ok(())
}

/// Format a single row string for the given trigger char and item.
fn format_trigger_row(
    &self,
    trigger_char: char,
    item: &crate::trigger::MatchItem,
    _theme: &Theme,
) -> String {
    match trigger_char {
        '@' => format!(" + {}", item.display),
        '/' => {
            // Command mode: "▶/command       description  args"
            // Matches the pre-refactor SlashPalette rendering.
            format!(" {:<13} {}", item.display, item.hint.as_deref().unwrap_or(""))
        }
        _ => format!(" ▶ {}", item.display),
    }
}
```

### 6.3 Cursor move-up adjustment

Replace the cursor-position adjustment that referenced `ac_rows` and
`palette_row_count` with `trigger_rows`:

```rust
// In draw_prompt_block(), at the cursor-restore section:
queue!(
    self.out,
    cursor::MoveToColumn(0),
    cursor::MoveUp(rows_from_bottom + trigger_rows as u16),
    cursor::MoveToColumn(col),
)?;
```

---

## 7. Migrating Existing Handlers

### 7.1 `MentionTriggerHandler`

File: `crates/xaft-tui/src/trigger/mention.rs`

```rust
//! `@`-mention file/directory trigger handler.
//!
//! Migrated from `AppState::refresh_autocomplete()` and
//! `AppState::autocomplete_complete()` in `state.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xaft_config::MentionConfig;
use agtrs_workspace::WorkspaceStore;

use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

pub struct MentionTriggerHandler {
    workspace: Option<Arc<dyn WorkspaceStore>>,
    workspace_root: Option<PathBuf>,
    config: MentionConfig,
}

impl MentionTriggerHandler {
    /// Construct with no workspace (placeholder; `AppState::init_mention()`
    /// replaces this via `TriggerRegistry::replace()`).
    pub fn new(config: MentionConfig) -> Self {
        Self {
            workspace: None,
            workspace_root: None,
            config,
        }
    }

    /// Construct with a fully initialised workspace.
    pub fn with_workspace(
        workspace: Arc<dyn WorkspaceStore>,
        workspace_root: PathBuf,
        config: MentionConfig,
    ) -> Self {
        Self {
            workspace: Some(workspace),
            workspace_root: Some(workspace_root),
            config,
        }
    }
}

impl TriggerHandler for MentionTriggerHandler {
    fn trigger_char(&self) -> char {
        '@'
    }

    /// Populate candidates by listing the directory implied by `ctx.dir_prefix`.
    ///
    /// Exact logic migrated from `AppState::refresh_autocomplete()`.
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let workspace_root = match &self.workspace_root {
            Some(r) => r.clone(),
            None => return vec![],
        };

        let dir_to_list = if ctx.dir_prefix.is_empty() {
            workspace_root.clone()
        } else {
            workspace_root.join(ctx.dir_prefix.trim_end_matches('/'))
        };

        let file_prefix = match ctx.prefix.rfind('/') {
            Some(slash) => &ctx.prefix[slash + 1..],
            None => ctx.prefix,
        };

        let raw_entries = match std::fs::read_dir(&dir_to_list) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
            Err(_) => return vec![],
        };

        let file_prefix_lower = file_prefix.to_lowercase();
        let mut items: Vec<MatchItem> = raw_entries
            .iter()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !file_prefix.starts_with('.') {
                    return None;
                }
                if !name.to_lowercase().starts_with(&file_prefix_lower) {
                    return None;
                }
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let (display_name, kind) = if is_dir {
                    (format!("{name}/"), MatchKind::Directory)
                } else {
                    (name.clone(), MatchKind::File)
                };
                let insert = if is_dir {
                    format!("@{}{}/", ctx.dir_prefix, name)
                } else {
                    format!("@{}{} ", ctx.dir_prefix, name)
                };
                Some(MatchItem {
                    display: display_name,
                    insert,
                    hint: None,
                    kind,
                })
            })
            .collect();

        // Sort: directories first, then files, both alphabetically.
        items.sort_by(|a, b| {
            let a_dir = a.kind == MatchKind::Directory;
            let b_dir = b.kind == MatchKind::Directory;
            match (a_dir, b_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.display.cmp(&b.display),
            }
        });
        items.truncate(10);
        items
    }

    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        // `item.insert` already contains the fully-formed `@<path>` or `@<dir>/`
        // string constructed in `matches()`.
        item.insert.clone()
    }

    fn allows_free_text(&self) -> bool {
        false
    }

    fn max_visible(&self) -> usize {
        10
    }
}
```

### 7.2 `SlashCommandTriggerHandler`

File: `crates/xaft-tui/src/trigger/slash_command.rs`

```rust
//! Slash-command palette trigger handler.
//!
//! Migrated from `AppState::refresh_slash_palette()` and the slash-palette
//! section of `handle_key()` in `state.rs`.

use std::sync::Arc;

use crate::slash::parser::{COMMAND_TABLE, SlashCommandParser};
use crate::slash::registry::SlashCommandRegistry;
use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

pub struct SlashCommandTriggerHandler {
    registry: Arc<SlashCommandRegistry>,
}

impl SlashCommandTriggerHandler {
    pub fn new(registry: Arc<SlashCommandRegistry>) -> Self {
        Self { registry }
    }
}

impl TriggerHandler for SlashCommandTriggerHandler {
    fn trigger_char(&self) -> char {
        '/'
    }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        // `ctx.prefix` is everything after the leading '/'.
        let candidates = SlashCommandParser::completions(ctx.prefix);
        candidates
            .iter()
            .map(|&trigger| {
                let meta = COMMAND_TABLE
                    .iter()
                    .find(|(k, _)| k == &trigger)
                    .map(|(_, m)| m);
                let description = meta.map(|m| m.description).unwrap_or("");
                let args_hint = meta.and_then(|m| m.args_hint);

                // `display` mirrors the pre-refactor SlashPaletteRow rendering:
                // trigger text only (description/args rendered by draw_trigger_dropdown).
                MatchItem {
                    display: trigger.to_string(),
                    insert: format!("/{} ", trigger),
                    hint: args_hint.map(String::from),
                    kind: MatchKind::Command,
                }
            })
            .collect()
    }

    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// The user can submit `/clear` even if it partially matches `/clearall`.
    fn allows_free_text(&self) -> bool {
        true
    }

    fn max_visible(&self) -> usize {
        8
    }
}
```

---

## 8. New Trigger Handler Stubs (Extensibility Examples)

These demonstrate that new triggers are addable with zero changes to the
generic machinery.

### 8.1 `HistoryTriggerHandler`

File: `crates/xaft-tui/src/trigger/history.rs`

```rust
//! History-recall trigger handler (trigger: `#`).
//!
//! When the user types `#` (or presses Up with an empty buffer — handled
//! separately in `handle_key()`), the last N submitted messages are shown
//! in the dropdown. Selecting one pastes it back into the input bar.
//!
//! This is a stub. Full implementation is a follow-up PRD.

use std::sync::{Arc, RwLock};

use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

/// Shared ring buffer of past user messages (populated by `handle_submit()`).
pub type SharedHistory = Arc<RwLock<Vec<String>>>;

pub struct HistoryTriggerHandler {
    history: SharedHistory,
}

impl HistoryTriggerHandler {
    pub fn new(history: SharedHistory) -> Self {
        Self { history }
    }
}

impl TriggerHandler for HistoryTriggerHandler {
    fn trigger_char(&self) -> char {
        '#'
    }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let guard = match self.history.read() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        guard
            .iter()
            .rev()
            .filter(|msg| {
                ctx.prefix.is_empty()
                    || msg.to_lowercase().contains(&ctx.prefix.to_lowercase())
            })
            .take(self.max_visible())
            .map(|msg| {
                let truncated: String = msg.chars().take(60).collect();
                MatchItem {
                    display: truncated.clone(),
                    insert: truncated,
                    hint: None,
                    kind: MatchKind::Custom("history".into()),
                }
            })
            .collect()
    }

    fn allows_free_text(&self) -> bool {
        false
    }

    fn max_visible(&self) -> usize {
        10
    }
}
```

### 8.2 `EmojiTriggerHandler` (stub)

File: `crates/xaft-tui/src/trigger/emoji.rs`

```rust
//! Emoji/symbol insert trigger handler (trigger: `:`).
//!
//! When the user types `:fire`, the dropdown shows matching emoji. Selecting
//! one inserts the emoji character and a trailing space.
//!
//! This is a stub. A real implementation would load a bundled emoji dataset.

use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

// Minimal static table for illustration purposes.
static EMOJI_TABLE: &[(&str, &str)] = &[
    ("fire", "\u{1F525}"),
    ("check", "\u{2705}"),
    ("x", "\u{274C}"),
    ("warning", "\u{26A0}"),
    ("rocket", "\u{1F680}"),
    ("bug", "\u{1F41B}"),
    ("tools", "\u{1F6E0}"),
];

pub struct EmojiTriggerHandler;

impl TriggerHandler for EmojiTriggerHandler {
    fn trigger_char(&self) -> char {
        ':'
    }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let query = ctx.prefix.to_lowercase();
        EMOJI_TABLE
            .iter()
            .filter(|(name, _)| name.starts_with(query.as_str()))
            .map(|(name, glyph)| MatchItem {
                display: format!("{glyph}  :{name}"),
                insert: format!("{glyph} "),
                hint: None,
                kind: MatchKind::Emoji,
            })
            .collect()
    }

    fn max_visible(&self) -> usize {
        6
    }
}
```

---

## 9. Signal Additions

File: `crates/xaft-agent/src/signals.rs`

Add two new signal structs below the existing definitions:

```rust
/// Emitted when a trigger dropdown becomes active (the user typed a trigger
/// character and the handler returned at least one match).
///
/// Useful for analytics and integration tests that assert the dropdown opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftTriggerActivated {
    /// The trigger character that opened the dropdown (e.g. `'@'`, `'/'`).
    pub trigger_char: char,
    /// The prefix text typed after the trigger char at activation time.
    pub prefix: String,
}

/// Emitted when the user selects an item from an active trigger dropdown
/// (Tab, Enter, or mouse click).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftTriggerItemSelected {
    /// The trigger character of the dropdown that was active.
    pub trigger_char: char,
    /// The `display` text of the selected item.
    pub item_display: String,
    /// The `insert` text that was placed into the buffer.
    pub item_insert: String,
}
```

Emit `XaftTriggerActivated` from `refresh_trigger()` when `active_trigger`
transitions from `None` to `Some`, and `XaftTriggerItemSelected` from
`trigger_complete()` after the insertion succeeds.

---

## 10. Files to Change

| File | Change type | Description |
|------|-------------|-------------|
| `crates/xaft-tui/src/trigger/mod.rs` | **New** | `MatchItem`, `MatchKind`, `TriggerContext`, `TriggerHandler`, `TriggerRegistry`, `ActiveTriggerScan`, `ActiveTrigger` |
| `crates/xaft-tui/src/trigger/mention.rs` | **New** | `MentionTriggerHandler` — migrated from `AppState::refresh_autocomplete()` / `autocomplete_complete()` |
| `crates/xaft-tui/src/trigger/slash_command.rs` | **New** | `SlashCommandTriggerHandler` — migrated from `AppState::refresh_slash_palette()` and its `handle_key()` block |
| `crates/xaft-tui/src/trigger/history.rs` | **New** | `HistoryTriggerHandler` stub |
| `crates/xaft-tui/src/trigger/emoji.rs` | **New** | `EmojiTriggerHandler` stub |
| `crates/xaft-tui/src/lib.rs` | **Modify** | Add `pub mod trigger;` export |
| `crates/xaft-tui/src/state.rs` | **Modify** | Remove `mention_autocomplete`, `slash_palette`; add `trigger_registry`, `active_trigger`; replace `refresh_autocomplete()` + `refresh_slash_palette()` with `refresh_trigger()`; collapse two key-dispatch blocks into `handle_trigger_key()` + `trigger_complete()` |
| `crates/xaft-tui/src/prompt.rs` | **Modify** | Remove `AutocompleteDropdown` (move logic to handler), `SlashPaletteRow`, `SlashPaletteSnapshot`; add `ActiveTriggerSnapshot`; rewrite `build_prompt()` trigger section |
| `crates/xaft-tui/src/renderer.rs` | **Modify** | Remove `ac_rows` + `palette_row_count` height vars; add `trigger_rows`; replace two inline dropdown rendering sections with `draw_trigger_dropdown()` + `format_trigger_row()` helpers |
| `crates/xaft-agent/src/signals.rs` | **Modify** | Add `XaftTriggerActivated` and `XaftTriggerItemSelected` |

---

## 11. Acceptance Criteria

| ID | Criterion |
|----|-----------|
| AC-01 | `AppState` has no `mention_autocomplete` field and no `slash_palette` field after the migration. `cargo build` succeeds. |
| AC-02 | `AppState` has exactly one `active_trigger: Option<ActiveTrigger>` field. |
| AC-03 | `handle_key()` contains no `if self.mention_autocomplete.is_some()` or `if self.slash_palette.is_some()` guard blocks. |
| AC-04 | Typing `@src/ma` in the input bar opens a dropdown showing file candidates from the workspace root that match `"ma"`. The visual layout (` + <name>` rows) is identical to the pre-migration layout. |
| AC-05 | Pressing Up/Down while the `@`-mention dropdown is open moves the selection. Tab/Enter completes the selected candidate into the buffer. Esc dismisses the dropdown. |
| AC-06 | Selecting a directory entry (trailing `/`) immediately re-opens the dropdown showing the directory's contents. |
| AC-07 | Typing `/cl` at the start of a single-line buffer opens the slash-command palette showing commands matching `"cl"` (e.g. `clear`, `cls`). The visual layout (indicator + trigger + description + args columns) is identical to the pre-migration layout. |
| AC-08 | Pressing Tab in the slash palette completes to the selected command text in the buffer. Pressing Enter executes the highlighted command. Pressing Esc dismisses the palette. |
| AC-09 | Typing `/` on a buffer that already has a second line (multi-line) does NOT open the slash palette. |
| AC-10 | Registering a third handler (e.g. `EmojiTriggerHandler`) in `AppState::new()` causes `:fire` to open a dropdown without any changes to `handle_key()`, `refresh_trigger()`, `build_prompt()`, or `draw_prompt_block()`. |
| AC-11 | `TriggerRegistry::register()` panics at startup (not at keypress) when two handlers share the same trigger character. The panic message names the conflicting character. |
| AC-12 | `XaftTriggerActivated` is emitted on the signal bus when the dropdown opens. `XaftTriggerItemSelected` is emitted when Tab or Enter completes an item. Existing F3 signals (`XaftMentionsResolved`, etc.) continue to be emitted unchanged. |
| AC-13 | `cargo test --workspace` passes with no new failures. The test suite for `state.rs` (currently inline `#[cfg(test)] mod tests`) covers `active_for()`, `refresh_trigger()`, `handle_trigger_key()`, and `trigger_complete()`. |
| AC-14 | `cargo clippy --workspace` produces no new warnings. All new types derive `Debug`. |
| AC-15 | `PromptState` has no `autocomplete` or `slash_palette` fields. `build_prompt()` references only `active_trigger`. |

---

## 12. Tests

All tests live in `crates/xaft-tui/src/trigger/mod.rs` (unit) and
`crates/xaft-tui/tests/trigger_integration.rs` (integration). Async tests
use `#[tokio::test]`.

```rust
// crates/xaft-tui/src/trigger/mod.rs  — #[cfg(test)] mod tests

// ── Helper trigger for unit tests ─────────────────────────────────────────────

struct EchoTrigger;

impl TriggerHandler for EchoTrigger {
    fn trigger_char(&self) -> char { '!' }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        if ctx.prefix.is_empty() {
            return vec![];
        }
        vec![MatchItem {
            display: ctx.prefix.to_owned(),
            insert: format!("!{} ", ctx.prefix),
            hint: Some(format!("echo: {}", ctx.prefix)),
            kind: MatchKind::Custom("echo".into()),
        }]
    }
}

// ── TriggerRegistry::register / get ───────────────────────────────────────────

#[test]
fn registry_register_and_get_returns_handler() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    assert!(reg.get('!').is_some());
    assert!(reg.get('@').is_none());
}

#[test]
#[should_panic(expected = "duplicate handler")]
fn registry_register_duplicate_panics() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    reg.register(Arc::new(EchoTrigger)); // second '!' — must panic
}

// ── TriggerRegistry::active_for ───────────────────────────────────────────────

#[test]
fn active_for_finds_trigger_at_cursor() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    // "say !hello" — cursor after 'o' (col=10)
    let result = reg.active_for("say !hello", 10);
    assert!(result.is_some());
    let scan = result.unwrap();
    assert_eq!(scan.trigger_char, '!');
    assert_eq!(scan.prefix, "hello");
}

#[test]
fn active_for_returns_none_when_whitespace_after_trigger() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    // "! hello" — whitespace immediately after '!'
    assert!(reg.active_for("! hello", 7).is_none());
}

#[test]
fn active_for_returns_none_when_no_trigger_char() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    assert!(reg.active_for("no trigger here", 15).is_none());
}

#[test]
fn active_for_uses_rightmost_trigger_char() {
    let mut reg = TriggerRegistry::new();
    reg.register(Arc::new(EchoTrigger));
    // "!foo !bar" — cursor at end; rightmost '!' is the active one
    let result = reg.active_for("!foo !bar", 9);
    let scan = result.unwrap();
    assert_eq!(scan.prefix, "bar");
    assert_eq!(scan.trigger_byte_pos, 5);
}

// ── ActiveTrigger::select_next / select_prev ──────────────────────────────────

#[test]
fn select_next_wraps_around() {
    let items = vec![
        MatchItem { display: "a".into(), insert: "a".into(), hint: None, kind: MatchKind::File },
        MatchItem { display: "b".into(), insert: "b".into(), hint: None, kind: MatchKind::File },
    ];
    let mut at = ActiveTrigger {
        scan: ActiveTriggerScan {
            trigger_char: '!',
            prefix: "".into(),
            dir_prefix: "".into(),
            trigger_byte_pos: 0,
        },
        items,
        selected: 1,
        scroll_top: 0,
    };
    at.select_next(8); // wraps from index 1 back to 0
    assert_eq!(at.selected, 0);
}

#[test]
fn select_prev_wraps_to_last() {
    let items = vec![
        MatchItem { display: "a".into(), insert: "a".into(), hint: None, kind: MatchKind::File },
        MatchItem { display: "b".into(), insert: "b".into(), hint: None, kind: MatchKind::File },
    ];
    let mut at = ActiveTrigger {
        scan: ActiveTriggerScan {
            trigger_char: '!',
            prefix: "".into(),
            dir_prefix: "".into(),
            trigger_byte_pos: 0,
        },
        items,
        selected: 0,
        scroll_top: 0,
    };
    at.select_prev(8); // wraps from index 0 to last (1)
    assert_eq!(at.selected, 1);
}

// ── refresh_trigger() via AppState ────────────────────────────────────────────

#[test]
fn refresh_trigger_opens_mention_dropdown() {
    let mut state = AppState::new_for_test();
    // Register a stub MentionTriggerHandler with a temp workspace root.
    let tmpdir = tempfile::TempDir::new().unwrap();
    std::fs::write(tmpdir.path().join("main.rs"), "").unwrap();
    let handler = Arc::new(
        crate::trigger::mention::MentionTriggerHandler::with_workspace(
            Arc::new(agtrs_workspace::FsWorkspaceStore::new(tmpdir.path())),
            tmpdir.path().to_path_buf(),
            xaft_config::MentionConfig::default(),
        )
    );
    state.trigger_registry.register(handler);
    state.working_dir = tmpdir.path().to_path_buf();

    state.input_bar.set_text("@ma");
    state.input_bar.set_cursor(0, 3);
    state.refresh_trigger();

    assert!(state.active_trigger.is_some());
    let at = state.active_trigger.as_ref().unwrap();
    assert_eq!(at.scan.trigger_char, '@');
    assert!(at.items.iter().any(|i| i.display.contains("main.rs")));
}

#[test]
fn refresh_trigger_clears_when_whitespace_after_at() {
    let mut state = AppState::new_for_test();
    state.input_bar.set_text("@ foo");
    state.input_bar.set_cursor(0, 5);
    state.refresh_trigger();
    assert!(state.active_trigger.is_none());
}
```

---

## 13. Migration Plan

The migration is split into four phases that can be code-reviewed independently.
Each phase leaves the TUI in a working state.

### Phase 1 — New module, no behaviour change

**Branch**: `feat/trigger-system-types`

1. Create `crates/xaft-tui/src/trigger/mod.rs` with all new types (`MatchItem`,
   `MatchKind`, `TriggerContext`, `TriggerHandler`, `TriggerRegistry`,
   `ActiveTriggerScan`, `ActiveTrigger`). No usage yet.
2. Create `crates/xaft-tui/src/trigger/mention.rs` with `MentionTriggerHandler`.
   Do not wire it into `AppState` yet.
3. Create `crates/xaft-tui/src/trigger/slash_command.rs` with
   `SlashCommandTriggerHandler`. Do not wire it in yet.
4. Add `pub mod trigger;` to `crates/xaft-tui/src/lib.rs`.
5. Write all unit tests in `trigger/mod.rs`. Run `cargo test -p xaft-tui`.
6. **Gate**: `cargo build --workspace` green, all existing tests pass, no new
   fields or behaviour changed.

### Phase 2 — Wire into `AppState`, keep old fields as aliases

**Branch**: `feat/trigger-system-appstate`

1. Add `trigger_registry: TriggerRegistry` and `active_trigger: Option<ActiveTrigger>`
   to `AppState`. Populate `trigger_registry` in `new()` with both handlers.
2. Add `refresh_trigger()` method.
3. In `handle_key()`:
   - Keep both existing `if self.mention_autocomplete.is_some()` and
     `if self.slash_palette.is_some()` blocks **temporarily**.
   - Add `handle_trigger_key()` and `trigger_complete()` as new methods (unused).
4. In `InputAction::BufferChanged | InputAction::CursorMoved`, add a call to
   `refresh_trigger()` alongside the two existing calls.
5. **Gate**: TUI works end-to-end. `@`-mention and slash palette both still
   function via the old code paths. `active_trigger` is populated but not yet
   used for rendering or key dispatch.

### Phase 3 — Flip key dispatch and refresh to the new system

**Branch**: `feat/trigger-system-dispatch`

1. Remove the `if self.mention_autocomplete.is_some()` block from `handle_key()`.
   Replace with the unified `if self.active_trigger.is_some() { ... handle_trigger_key() ... }` block.
2. Remove the `if self.slash_palette.is_some()` block.
3. Remove `refresh_autocomplete()` and `refresh_slash_palette()`. Remove their
   calls from `InputAction::BufferChanged | InputAction::CursorMoved`.
4. Remove `mention_autocomplete` and `slash_palette` fields from `AppState`.
5. Update `AppState::new()` struct literal (remove the two old fields).
6. In `prompt.rs`:
   - Remove `AutocompleteDropdown` (or move it to a private type used only in
     `trigger/mention.rs` if needed for intermediate state).
   - Remove `SlashPaletteRow` and `SlashPaletteSnapshot`.
   - Add `ActiveTriggerSnapshot`.
   - Rewrite `build_prompt()` trigger section.
   - Update `PromptState` struct.
7. **Gate**: `cargo build --workspace` green. Manual smoke-test `@` and `/` in
   the TUI. Run `cargo test --workspace`.

### Phase 4 — Renderer unification

**Branch**: `feat/trigger-system-renderer`

1. Add `draw_trigger_dropdown()` and `format_trigger_row()` to `renderer.rs`.
2. Remove the `ac_rows` / `palette_row_count` height variables. Replace with
   `trigger_rows`.
3. Replace the two inline rendering sections (lines 691–798) with the single
   `draw_trigger_dropdown()` call.
4. Add `XaftTriggerActivated` and `XaftTriggerItemSelected` to
   `crates/xaft-agent/src/signals.rs`.
5. Emit the signals from `refresh_trigger()` and `trigger_complete()`.
6. Write integration tests in `crates/xaft-tui/tests/trigger_integration.rs`.
7. **Gate**: `cargo test --workspace`, `cargo clippy --workspace`, visual
   regression check that `@`-mention and slash-command dropdowns look identical
   to pre-migration screenshots.

### Rollback strategy

Each phase is a separate PR. If Phase 3 or 4 introduces a regression, the PR
is reverted without touching Phase 1 or 2 work. The old `AppState` fields
(`mention_autocomplete`, `slash_palette`) are not removed until Phase 3 is
merged, so Phase 1–2 can be rolled back independently.

### Testing during migration

At every phase gate, run:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
RUST_LOG=xaft=debug xaft run "list files"   # smoke test @-mention
```

A manual smoke-test checklist:

- [ ] Type `@src/` — dropdown opens, shows directory contents
- [ ] Press Down twice, Tab — completes into buffer correctly
- [ ] Press Esc — dropdown dismisses, buffer preserved
- [ ] Type `/cle` — slash palette opens, shows `clear`
- [ ] Press Enter — `/clear` executes, transcript cleared
- [ ] Type `:fir` (if `EmojiTriggerHandler` registered) — dropdown opens
- [ ] Multi-line buffer + `/` — slash palette does NOT open

---

## Appendix A: IndexMap dependency

`TriggerRegistry` uses `IndexMap<char, Arc<dyn TriggerHandler>>` from the
`indexmap` crate to preserve insertion order. If `indexmap` is not already in
`xaft-tui`'s dependency list, add it:

```toml
# crates/xaft-tui/Cargo.toml
[dependencies]
indexmap = "2"
```

Alternatively, `HashMap` is acceptable if deterministic iteration order is not
required. The only observable difference is the order in which `active_for()`
considers handlers when multiple registered trigger chars appear in the same
slice (last-char-wins semantics are preserved either way; the registry scan
is over the input string, not the handler map).

## Appendix B: `init_mention()` timing

`AppState::new()` registers a `MentionTriggerHandler` with no workspace (returns
empty `matches()`). `AppState::init_mention()` calls `TriggerRegistry::replace()`
to swap in the fully-initialised handler. This preserves the existing timing
contract: the workspace is injected after construction, from `app.rs`, once the
runtime's working directory is known. No behaviour change for sessions where
`init_mention()` is never called (e.g. headless / no-workspace runs).

## Appendix C: `allows_free_text()` semantics for `/`

The slash handler sets `allows_free_text() = true`. This enables the following
flow when the dropdown is open but no candidate is highlighted:

1. User types `/` → palette opens.
2. User types `zzz` — no candidates match `/zzz`.
3. User presses Enter.
4. `handle_trigger_key()` sees `allows_free_text = true`, no candidate (`items.is_empty()`).
5. `active_trigger` is set to `None` and `false` is returned — the key falls
   through to the input bar's normal Enter handler.
6. Input bar submits `/zzz` as a literal text message to the agent.

This reproduces the pre-migration behaviour where an unrecognised `/zzz` fell
through `SlashCommandParser::parse()` and was forwarded to the agent unchanged.
