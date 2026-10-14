# PRD 29a — Multi-line Input with Shift+Enter (Design & Architecture)

**Status:** Proposal
**Target:** next minor release
**Parent PRD:** PRD 28 §F21 (`Multi-line Input with Shift+Enter`)
**Primary crate:** `xaft-tui`
**Touched crates:** `xaft-tui`, `xaft-config`, `xaft-agent` (signals only)
**Companion PRD:** `29b_multi_line_input_implementation.md`

---

## 1. Background

Today the xaft input bar (`crates/xaft-tui/src/state.rs:51`) is a single
`pub input_buffer: String`. The `handle_key` function
(`crates/xaft-tui/src/state.rs:618`) handles four key paths:

| Key | Behaviour |
|---|---|
| `Char(c)` (no Ctrl) | `input_buffer.push(c)` → `UpdatePrompt` |
| `Backspace` | `input_buffer.pop()` → `UpdatePrompt` |
| `Enter` | trim, send via `user_message_tx`, clear buffer |
| `Ctrl+C` | cancel / quit (existing) |

The renderer (`crates/xaft-tui/src/renderer.rs:41`) assumes a fixed
**3-row prompt block**:

```text
┌────── top border ──────┐    ┐
│ ❯ user typing here     │    │  PROMPT_ROWS = 3
└── bottom border ───────┘    ┘
```

Cursor invariant: cursor always rests at *end of prompt text on the input
line* (see `renderer.rs` lines 14-19).

### Existing but-unused affordance

`xaft-config/src/defaults.rs:397-400` already declares:

```rust
bindings.insert(
    "alt+enter".to_string(),
    KeyAction::Single("newline_in_input".to_string()),
);
```

…but no code consumes the `"newline_in_input"` action. The binding is
declarative only; the input handler short-circuits on `KeyCode::Enter`
regardless of modifiers and submits.

### Why this matters

1. **Compose-friction.** Users copying multi-line snippets from a browser
   today either lose newlines (single-line buffer truncates at the first
   `\n` from bracketed paste — which isn't even enabled) or have to use
   `\\n` escapes manually.
2. **Code blocks.** Asking the agent "rewrite this function" requires the
   user to either paste-by-typing or rely on `@file` mention (which is a
   separate flow, see PRD 27 §F3).
3. **Structured prompts.** Numbered lists, separated paragraphs, "context
   + question" patterns all suffer.
4. **Parity gap.** Claude Code, Cursor, Aider, Continue all support
   Shift+Enter. xaft is the outlier.

---

## 2. Problem statement

> The xaft TUI input bar cannot represent a buffer containing newlines.
> There is no UI primitive (widget, cursor model, renderer hook) that
> supports an input region taller than one row. Pasting multi-line
> content silently drops every character past the first `\n` because
> bracketed paste is not enabled and the `String` buffer is rendered
> single-row.

---

## 3. Goals

| # | Goal | Verifiable by |
|---|---|---|
| G1 | `Shift+Enter` inserts `\n` at cursor, does **not** submit. | unit test `shift_enter_inserts_newline` |
| G2 | `Alt+Enter` inserts `\n` at cursor (fallback for terminals that fold Shift+Enter into Enter). | unit test `alt_enter_inserts_newline_fallback` |
| G3 | `Ctrl+J` inserts `\n` at cursor (universal fallback — `Ctrl+J` IS line-feed in ASCII). | unit test `ctrl_j_inserts_newline_universal` |
| G4 | `Enter` submits the full multi-line buffer as one message. | unit test `enter_submits_multi_line_buffer` |
| G5 | Pasted text with embedded `\n` preserves newlines. | unit test `pasted_text_preserves_newlines` |
| G6 | Visual cursor moves with `Up/Down/Left/Right` within the buffer. | unit test `cursor_moves_across_lines` |
| G7 | Buffer renders with `│ ` continuation guides on wrapped/secondary rows. | renderer snapshot test |
| G8 | Buffer auto-grows to ≤ 8 visual rows; older rows scroll out of the viewport. | unit test `buffer_grows_then_scrolls_at_max_rows` |
| G9 | After submit, the bar collapses back to one row. | unit test `buffer_collapses_after_submit` |
| G10 | The change is **fully backward-compatible**: existing tests pass without modification. | `cargo test --workspace` green |

---

## 4. Non-goals

- **Rich-text editing.** No bold, italics, color in the input bar.
- **Markdown preview.** The buffer renders as plain text. Preview is F26.
- **Multi-cursor / column selection.** Single caret only.
- **Vim/emacs full keymap.** Out of scope; this PRD only adds the minimal
  set: Up/Down/Left/Right/Home/End/Backspace/Ctrl+U/Ctrl+W. Vim mode is a
  separate item (`/vim` slash command, PRD 27 §F1).
- **Undo/redo.** Deferred to a follow-up PRD.
- **Soft-wrap word-boundary selection.** Wraps at hard column limit, not
  word boundaries.
- **History recall via Up/Down.** When buffer has > 1 line, Up/Down move
  the cursor. History recall via Up/Down on an empty buffer is PRD 28
  §F23 (input history). Both can coexist (see Open Questions in 29b).

---

## 5. Architecture

### 5.1 Data model

```text
                ┌──────────────────────────────────────┐
                │             InputBar                 │
                ├──────────────────────────────────────┤
                │ lines       : Vec<String>            │  ← logical rows
                │ cursor      : Cursor { row, col }    │  ← byte offsets
                │ scroll_top  : usize                  │  ← first visible row
                │ max_rows    : u16   (= 8)            │
                │ term_cols   : u16   (cached)         │
                │ dirty       : bool                   │
                └──────────────────────────────────────┘
                                │
                                ▼
                ┌──────────────────────────────────────┐
                │       handle_key(KeyEvent)           │
                ├──────────────────────────────────────┤
                │  → InputAction (enum)                │
                │    ├── Submit(String)                │
                │    ├── BufferChanged                 │
                │    ├── CursorMoved                   │
                │    └── NoOp                          │
                └──────────────────────────────────────┘
                                │
                                ▼
                ┌──────────────────────────────────────┐
                │  AppState propagates to renderer via │
                │  RenderMutation::UpdatePrompt(...)   │
                └──────────────────────────────────────┘
```

### 5.2 Layout

```text
┌────────────── top border ──────────────┐  ← always 1 row
│ ❯ first line of the user's input       │  ← visible_row 0
│ │ second line continuation             │  ← visible_row 1
│ │ third line continuation              │  ← visible_row 2
│ │ … up to max_rows=8 …                 │
└────────────── bottom border ───────────┘  ← always 1 row

  Total prompt block height = 2 + visible_rows (1..=8)
```

`PROMPT_ROWS` becomes a **runtime-computed value** (function of
`InputBar::visible_rows()`), not a `const`.

### 5.3 Cursor model

**Logical cursor**: `Cursor { row: usize, col: usize }` where:
- `row` indexes into `lines: Vec<String>`.
- `col` is a **byte offset** into `lines[row]` (UTF-8 boundary
  enforced).

**Visual cursor**: derived on render from logical cursor + soft-wrap:
- Each logical line may wrap into multiple visual rows.
- Visual column uses `unicode_width::UnicodeWidthChar::width()` (already
  a dep — `crates/xaft-tui/Cargo.toml:41`).

### 5.4 Soft-wrap

Wrap policy: **hard wrap at `term_cols - PREFIX_WIDTH`** where
`PREFIX_WIDTH = 2` (`❯ ` or `│ `). Wrapping never splits a multi-byte
character but may split mid-word (intentional — no token boundary
analysis).

A logical line `"foo bar baz qux"` on a 10-col terminal renders as:

```
❯ foo bar
│ baz qux
```

The cursor at byte offset 11 (`'q'`) sits at visual `(row=1, col=4)`.

### 5.5 Key event flow

```text
                    ┌─────────────────────────────────┐
                    │   crossterm::Event::Key(ev)     │
                    └──────────────┬──────────────────┘
                                   │
            ApprovalQueue::has_pending() ?
              ┌──── yes ────┐    │
              ▼             │   no
        approval handler    │    ▼
                            │   InputBar::handle_key(ev) → InputAction
                            │            │
                            │     ┌──────┼──────────┐
                            │     ▼      ▼          ▼
                            │   Submit  Buf-     CursorMoved
                            │     │     Changed     │
                            │     ▼      │          │
                            │   user_    │          │
                            │   message_ ▼          ▼
                            │   tx.send  UpdatePrompt mutation
                            ▼
                       AppState.mutations
```

### 5.6 Decision matrix for `handle_key`

| Key | Modifier mask | Action | Reason |
|-----|---------------|--------|--------|
| `Enter` | `NONE` | `Submit` | preserve existing default |
| `Enter` | `SHIFT` | `InsertNewline` | F21 primary |
| `Enter` | `ALT` | `InsertNewline` | F21 fallback (macOS Terminal, GNOME Terminal) |
| `Enter` | `CTRL` | `Submit` (no change) | already bound to `submit_input` in defaults |
| `Char('j')` | `CTRL` | `InsertNewline` | universal fallback — ASCII LF |
| `Char(c)` | `NONE` / `SHIFT` | `InsertChar(c)` | normal typing |
| `Backspace` | `NONE` | `BackspaceChar` | multi-line aware |
| `Backspace` | `CTRL` / `ALT` | `BackspaceWord` | optional |
| `Delete` | `NONE` | `DeleteChar` | new |
| `Up` | `NONE` | `CursorUp` | new |
| `Down` | `NONE` | `CursorDown` | new |
| `Left` | `NONE` | `CursorLeft` | new |
| `Right` | `NONE` | `CursorRight` | new |
| `Home` | `NONE` | `CursorLineStart` | new |
| `End` | `NONE` | `CursorLineEnd` | new |
| `Char('a')` | `CTRL` | `CursorLineStart` | emacs-style |
| `Char('e')` | `CTRL` | `CursorLineEnd` | emacs-style |
| `Char('u')` | `CTRL` | `ClearToLineStart` | F21 explicit |
| `Char('w')` | `CTRL` | `DeleteWordBackward` | bonus |
| `Char('k')` | `CTRL` | `ClearToLineEnd` | bonus |
| `Char('c')` | `CTRL` | (existing cancel path) | unchanged |

All other keys: NoOp (forward to existing handler chain — slash command
parser, etc., when those land).

### 5.7 Terminal-compatibility reality

Shift+Enter is **not** reliably distinguishable from Enter in the
xterm/VT100 protocol — the terminal sends only `\r` (or `\n`) for both.
Modern protocols that *can* distinguish:

| Protocol | Support | Status in crossterm 0.28 |
|---|---|---|
| Kitty Keyboard Protocol (CSI u) | kitty, foot, WezTerm, alacritty (≥0.13), iTerm2 (≥3.5), Ghostty | `event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` |
| modifyOtherKeys (xterm 216+) | xterm, gnome-terminal, konsole, vte-derived | best-effort |
| Legacy VT100 | everything else | **cannot** distinguish |

**Resolution strategy:**

1. On `surface.init()`, attempt to push `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES`. Ignore failures (older terminals).
2. If the terminal supports it, `Shift+Enter` arrives with
   `modifiers = KeyModifiers::SHIFT`, `code = KeyCode::Enter`.
3. If it doesn't, the user's `Shift+Enter` arrives as bare `Enter` → it
   submits. The user falls back to `Alt+Enter` or `Ctrl+J` (both work in
   every terminal we target).
4. We document the fallback chain in `/help` output and in the
   ephemeral hint line that appears when the buffer is empty:

```
   ❯
   ────────────────────────────────────────
   Shift+Enter / Alt+Enter / Ctrl+J → newline   ·   Enter → send
```

### 5.8 Bracketed paste

Currently disabled (`crates/xaft-tui/src/surface.rs:31-38` enables raw
mode + optional mouse, no `EnableBracketedPaste`). Without it, a paste
of `"line1\nline2"` arrives as separate `Event::Key` events — including
a literal `\r` that the current `handle_key` interprets as Enter →
**submits the message early**, dropping `"line2"`.

**Fix:** unconditionally enable bracketed paste in `surface.init()`:

```rust
use crossterm::event::EnableBracketedPaste;
execute!(io::stdout(), EnableBracketedPaste)?;
```

Paste arrives as `Event::Paste(String)`. The TUI app loop must forward
it as a new `TuiEvent::Paste(String)` variant, handled by
`InputBar::insert_str()` which splits on `\n` and grows the buffer.

---

## 6. API design

### 6.1 New module `xaft-tui/src/input_bar.rs`

```rust
//! Multi-line input widget for the xaft TUI.
//!
//! Owns a multi-line `Vec<String>` buffer with a logical cursor and a
//! visible-row viewport. Soft-wraps at terminal width. Emits
//! `InputAction` enum values, which `AppState` converts into
//! `RenderMutation`s (no direct state mutation).
//!
//! # Cursor model
//!
//! - `cursor.row` indexes into `lines: Vec<String>`.
//! - `cursor.col` is the BYTE offset into `lines[cursor.row]`.
//! - Char-boundary invariant: `lines[row].is_char_boundary(col) == true`.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Maximum visible rows before the buffer scrolls.
pub const MAX_VISIBLE_ROWS: u16 = 8;
/// Width of the prefix (`"❯ "` or `"│ "`) — used for soft-wrap.
pub const PREFIX_WIDTH: usize = 2;

/// Logical cursor position (byte offset, not visual column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

/// Output of `InputBar::handle_key` — never mutates external state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// The user pressed Enter; deliver the full buffer.
    Submit(String),
    /// The buffer contents changed; redraw needed.
    BufferChanged,
    /// Only the cursor moved; redraw needed (for caret position).
    CursorMoved,
    /// Key was not consumed by the input bar.
    NoOp,
}

/// Multi-line input widget.
#[derive(Debug, Clone)]
pub struct InputBar {
    lines: Vec<String>,
    cursor: Cursor,
    scroll_top: usize,
    max_visible_rows: u16,
    term_cols: u16,
}

impl Default for InputBar {
    fn default() -> Self { Self::new(120) }
}

impl InputBar {
    /// Create an empty input bar sized for a `term_cols`-wide terminal.
    pub fn new(term_cols: u16) -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor::default(),
            scroll_top: 0,
            max_visible_rows: MAX_VISIBLE_ROWS,
            term_cols,
        }
    }

    // ── Read-only API ────────────────────────────────────────────────

    /// Joined buffer with `\n` separators (the submit payload).
    pub fn text(&self) -> String { self.lines.join("\n") }
    /// `true` when the buffer holds no characters.
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
    /// Logical line count (≥ 1).
    pub fn line_count(&self) -> usize { self.lines.len() }
    /// Visible (post-soft-wrap) row count, clamped to `max_visible_rows`.
    pub fn visible_rows(&self) -> u16 {
        let wrapped: usize = self.lines.iter()
            .map(|l| wrap_rows(l, self.wrap_width())).sum();
        wrapped.clamp(1, self.max_visible_rows as usize) as u16
    }
    /// Logical cursor.
    pub fn cursor(&self) -> Cursor { self.cursor }
    /// Slice of lines for rendering (no allocation).
    pub fn lines(&self) -> &[String] { &self.lines }

    // ── Mutating API (only via key events / paste) ────────────────────

    /// Handle a key event. Returns the resulting `InputAction`.
    /// Never mutates anything outside `self`.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction { /* see §5.6 */ }

    /// Insert a (possibly multi-line) string at the cursor.
    /// Used by paste events and external editor (F24).
    pub fn insert_str(&mut self, s: &str) -> InputAction { /* split on \n */ }

    /// Replace buffer with `s`; cursor → end.
    pub fn set_text(&mut self, s: &str) { /* tests, history recall */ }

    /// Clear buffer; cursor → (0, 0).
    pub fn clear(&mut self) { /* after submit */ }

    /// Inform the bar of a terminal resize (recomputes scroll viewport).
    pub fn on_resize(&mut self, term_cols: u16) { /* updates term_cols */ }

    // ── Internals ─────────────────────────────────────────────────────

    fn wrap_width(&self) -> usize {
        (self.term_cols as usize).saturating_sub(PREFIX_WIDTH).max(10)
    }
}

/// Count how many visual rows `line` occupies when wrapped at `width`.
pub fn wrap_rows(line: &str, width: usize) -> usize { /* unicode-width aware */ }
```

### 6.2 Action handlers (internal, table-driven)

```rust
impl InputBar {
    fn insert_char(&mut self, c: char) -> InputAction { ... }
    fn insert_newline(&mut self) -> InputAction { ... }
    fn backspace(&mut self) -> InputAction { ... }
    fn delete_forward(&mut self) -> InputAction { ... }
    fn cursor_left(&mut self) -> InputAction { ... }
    fn cursor_right(&mut self) -> InputAction { ... }
    fn cursor_up(&mut self) -> InputAction { ... }
    fn cursor_down(&mut self) -> InputAction { ... }
    fn cursor_line_start(&mut self) -> InputAction { ... }
    fn cursor_line_end(&mut self) -> InputAction { ... }
    fn clear_to_line_start(&mut self) -> InputAction { ... }
    fn clear_to_line_end(&mut self) -> InputAction { ... }
    fn delete_word_backward(&mut self) -> InputAction { ... }
    fn submit(&mut self) -> InputAction {
        let payload = self.text().trim().to_string();
        if payload.is_empty() { return InputAction::NoOp; }
        self.clear();
        InputAction::Submit(payload)
    }
}
```

### 6.3 Boundary safety

Every cursor write goes through:

```rust
fn snap_to_char_boundary(line: &str, col: usize) -> usize {
    let mut c = col.min(line.len());
    while c > 0 && !line.is_char_boundary(c) { c -= 1; }
    c
}
```

After every public mutation: `debug_assert!(self.cursor_invariants_hold())`.

---

## 7. State integration

### 7.1 `AppState` change

**Before:**

```rust
// crates/xaft-tui/src/state.rs:51
pub input_buffer: String,
```

**After:**

```rust
pub input_bar: InputBar,   // replaces input_buffer entirely
```

A re-export shim is provided for one release:

```rust
impl AppState {
    #[deprecated(note = "use `self.input_bar.text()`")]
    pub fn input_buffer(&self) -> String { self.input_bar.text() }
}
```

This keeps any out-of-tree code compiling for a release cycle.

### 7.2 `handle_key` rewrite

```rust
fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
    self.error_message = None;

    // Approval path (unchanged) ───────────────────────────────────────
    if self.approval_queue.has_pending() { /* unchanged */ return; }

    // Cancel path (unchanged) ─────────────────────────────────────────
    if is_ctrl_c(&key) { return self.handle_ctrl_c(); }

    // Input bar ───────────────────────────────────────────────────────
    match self.input_bar.handle_key(key) {
        InputAction::Submit(msg) => {
            self.mutations.push(RenderMutation::CommitLine(
                StyledLine::new(format!("❯ {msg}"), LineKind::UserMessage)));
            if let Some(tx) = &self.user_message_tx {
                let _ = tx.send(msg.clone());
            }
            self.task = msg;
            self.task_start_time = Some(Instant::now());
            self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
        }
        InputAction::BufferChanged | InputAction::CursorMoved => {
            self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
        }
        InputAction::NoOp => {}
    }
}
```

### 7.3 `PromptState` change

```rust
// crates/xaft-tui/src/prompt.rs
pub struct PromptState {
    pub lines: Vec<String>,          // was: pub buffer: String
    pub cursor: Cursor,              // new
    pub agent_active: bool,
}
```

`build_prompt`:

```rust
pub fn build_prompt(state: &AppState) -> PromptState {
    PromptState {
        lines: state.input_bar.lines().to_vec(),
        cursor: state.input_bar.cursor(),
        agent_active: state.phase.is_active(),
    }
}
```

### 7.4 Paste event wiring

Add a new variant to `TuiEvent`:

```rust
// crates/xaft-tui/src/bridge.rs
pub enum TuiEvent {
    // ...existing variants...
    Paste(String),   // new
}
```

In `app.rs` event reader loop:

```rust
Event::Paste(s) => { let _ = tx_keys.send(TuiEvent::Paste(s)); }
```

In `AppState::handle_event`:

```rust
TuiEvent::Paste(s) => {
    if let InputAction::BufferChanged = self.input_bar.insert_str(&s) {
        self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
    }
}
```

---

## 8. Renderer changes

### 8.1 `PROMPT_ROWS` becomes dynamic

```rust
// crates/xaft-tui/src/renderer.rs

// Before:
const PROMPT_ROWS: u16 = 3;

// After:
const BORDER_ROWS: u16 = 2;  // top + bottom
fn prompt_block_height(prompt: &PromptState, term_cols: u16) -> u16 {
    let content = prompt.visible_rows(term_cols).max(1).min(MAX_VISIBLE_ROWS);
    content + BORDER_ROWS
}
```

The renderer tracks `last_prompt_height: u16` so `clear_bottom_block`
knows how many rows to erase. `clear_bottom_block` becomes:

```rust
fn clear_bottom_block(&mut self) -> io::Result<()> {
    let rows_up = self.ephemeral_count as u16 + self.last_prompt_height - 1;
    queue!(self.out, cursor::MoveToColumn(0), cursor::MoveUp(rows_up),
           terminal::Clear(ClearType::FromCursorDown))?;
    Ok(())
}
```

### 8.2 `draw_prompt_block`

Replaces `draw_prompt_line`. Renders:

```text
─── top border ───
❯ <line 0 wrap row 0>
│ <line 0 wrap row 1>      ← soft-wrap continuation
│ <line 1>                 ← hard newline continuation
│ <line 2>
…
─── bottom border ───
```

Continuation prefix `│ ` styled in `theme.dim` (visually subtler than
the `❯ ` primary prompt).

### 8.3 Cursor placement

After redraw, move cursor to:

```rust
let (visual_row, visual_col) = visual_position(&prompt.lines, &prompt.cursor,
                                               wrap_width);
queue!(self.out,
       cursor::MoveToColumn(0),
       cursor::MoveUp(prompt_block_height - 1 - visual_row - 1 /* bottom border */),
       cursor::MoveToColumn((visual_col + PREFIX_WIDTH) as u16))?;
```

Helper `visual_position` is pure and unit-tested.

### 8.4 Scroll viewport

If `total_visible_rows > MAX_VISIBLE_ROWS`, the renderer shows the
**last** `MAX_VISIBLE_ROWS` rows by default (auto-scroll to cursor). A
`▲ N more lines above` indicator occupies the first visible row when
`scroll_top > 0`.

```text
─── top border ───
▲ 3 more lines above
│ line 4
│ line 5
│ line 6
│ line 7
│ line 8
│ line 9
│ line 10
❯ line 11 (with cursor)     ← cursor line at bottom of viewport
─── bottom border ───
```

---

## 9. Signals & AGENTS.md compliance

### 9.1 AGENTS.md analysis

`AGENTS.md:23` states *all workflow state mutations MUST occur through
tools*. Three relevant concerns:

| Concern | Classification | Compliance |
|---|---|---|
| Editing the input buffer | **Local UI state**, not workflow state | No tool required. The InputBar is a TUI widget; its buffer is ephemeral and never reaches the orchestrator or any persistent store. |
| Submitting a user message | **Workflow state mutation** (kicks off agent run) | Already correct: `user_message_tx.send(msg)` is the mutation boundary. The TUI does not synthesize state transitions; it sends a message into a channel that the runtime consumes and processes. |
| Recording input in history (PRD 28 §F23) | **Local persistence** | Will use a dedicated `HistoryStore` (PRD 28 §F23 already covers this); not in scope for F21. |

**Conclusion:** F21 introduces no new workflow-state mutation. It is a
pure-UI change. The single existing mutation boundary
(`user_message_tx`) is preserved.

### 9.2 Observability signals

Even though no workflow state changes, we **must** emit signals for
user-visible UI transitions per AGENTS.md §Event Requirements. Add:

```rust
// crates/xaft-agent/src/signals.rs

/// Emitted whenever the input bar buffer changes.
/// Diagnostic / metrics only — never consumed for state mutation.
#[derive(Debug, Clone)]
pub struct XaftInputBufferEdited {
    pub line_count: usize,
    pub char_count: usize,
    pub source: InputEditSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEditSource {
    KeyPress,
    Paste,
    ExternalEditor,   // for F24 forward-compat
    HistoryRecall,    // for F23 forward-compat
}

/// Emitted when the user submits a message.
/// Distinct from existing agent-message signals — captured BEFORE
/// the runtime sees the message.
#[derive(Debug, Clone)]
pub struct XaftUserMessageSubmitted {
    pub line_count: usize,
    pub char_count: usize,
    pub had_multi_line: bool,
}
```

These are tracing aids and feed the `/cost` and `/doctor` panels (PRD
27). They are not authoritative for any state.

### 9.3 Replayability

The input buffer is intentionally *not* persisted (no replay needed
across sessions). On crash, the in-flight buffer is lost — this is the
existing behaviour and is acceptable. PRD 28 §F23 (history) and §F24
(`$EDITOR`) provide opt-in durability for users who want it.

---

## 10. Summary

This PRD describes the design of a multi-line input bar for xaft's TUI.

- A new `InputBar` widget owns a `Vec<String>` buffer with a logical
  cursor and a viewport.
- Three independent newline-insertion paths (Shift+Enter primary,
  Alt+Enter and Ctrl+J fallbacks) handle the absence of a universal
  terminal protocol for distinguishing Shift+Enter from Enter.
- The renderer's fixed `PROMPT_ROWS=3` becomes a runtime-computed
  `prompt_block_height` that scales 3 → 10 rows with buffer growth.
- Bracketed paste is enabled so pasted multi-line content is preserved.
- The widget is **purely local UI state**; the single existing
  workflow-state mutation boundary (`user_message_tx`) is unchanged.

See `29b_multi_line_input_implementation.md` for files & line-level
changes, tests, acceptance criteria, migration, risk, phasing, and
open questions.
