# PRD 43 — `/vim` + `/emacs` Commands: Input Bar Keymap Modes

## Status: Draft
## Depends on: PRD 32a–32e, PRD 29a/29b (multi-line input)

---

## 1. Problem

`/vim` and `/emacs` return text strings but do not actually change input
bar behaviour. The `InputBar` processes every key in a single code path;
there is no concept of modal editing or Emacs-style keybindings.

---

## 2. Goals

### `/vim`

Adds a Vim-like modal editing layer to the input bar:

| Mode | Indicator | Behaviour |
|------|-----------|-----------|
| INSERT | `-- INSERT --` in status chip | All current keys work as-is |
| NORMAL | `-- NORMAL --` | `h/j/k/l` for cursor movement; `i/a/A/I/o/O` enter INSERT; `dd` deletes line; `u` undoes last insert; `x` deletes char; `w/b` word jumps; `0/$` line start/end; `/` opens a search within the buffer |
| VISUAL | `-- VISUAL --` | `v` from NORMAL selects by char; `y` yanks; `p` pastes |

- `Esc` from INSERT → NORMAL.
- `Esc` from NORMAL → stay in NORMAL (no double-Esc-to-quit behaviour).
- Entering INSERT mode always re-enables the standard key handler.

### `/emacs`

Activates Emacs-style keybindings on top of the existing bar:

| Binding | Action |
|---------|--------|
| `Ctrl+A` | Move to line start |
| `Ctrl+E` | Move to line end |
| `Ctrl+F` / `Ctrl+B` | Forward / backward one char |
| `Alt+F` / `Alt+B` | Forward / backward one word |
| `Ctrl+D` | Delete char forward |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+Y` | Yank (paste killed text) |
| `Ctrl+W` | Kill word backward |
| `Ctrl+P` / `Ctrl+N` | Previous / next history line |

`Esc` (bare) is intercepted by the Esc-confirm dialog, not Emacs mode —
Emacs users use `Alt`-prefixed bindings.

### `/vim` and `/emacs` toggle

Running `/vim` while in Vim mode switches back to default mode (toggle).
Running `/emacs` while in Emacs mode switches back to default.
Running `/vim` while in Emacs mode switches to Vim mode (replacing Emacs).

---

## 3. Non-Goals

- Full Vim ex-command support (`:q`, `:%s/…/…/g`, etc.).
- Vim macro recording (`q`-register).
- Emacs `M-x` command launcher.
- Persistent keymap preference across sessions (session-level only in v1).

---

## 4. Technical Design

### 4.1 `InputMode` enum

Add to `crates/xaft-tui/src/input_bar.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    Default,
    VimInsert,
    VimNormal,
    VimVisual,
    Emacs,
}
```

`InputBar` gains:
```rust
pub mode: InputMode,
pub vim_pending: Option<char>,   // for two-key combos like "dd"
pub kill_ring: Option<String>,   // for Emacs Ctrl+Y
```

### 4.2 Key routing in `InputBar::handle`

Before the existing key dispatch, check `self.mode`:
- `InputMode::VimNormal` → route to `handle_vim_normal(key)`.
- `InputMode::VimVisual` → route to `handle_vim_visual(key)`.
- `InputMode::Emacs` → check Emacs bindings first; fall through to default
  for unrecognised bindings.
- `InputMode::VimInsert` / `InputMode::Default` → existing handler.

### 4.3 Mode indicator in prompt

`PromptState` gains:
```rust
pub input_mode: InputMode,
```

`build_prompt` copies `state.input_bar.mode`.

`IncrementalRenderer::draw_prompt_block` renders a chip:
- Vim Normal: dim cyan `[N]` at the right end of the top prompt border.
- Vim Insert: nothing (default feel).
- Emacs: dim yellow `[E]`.

### 4.4 `VimHandler` and `EmacsHandler` updates

```rust
// VimHandler
fn execute(&self, ctx: CommandContext) -> CommandResult {
    // AppState handles mode switching via a new RenderMutation or signal
    CommandResult::SetInputMode(InputMode::VimInsert)
}

// EmacsHandler
fn execute(&self, ctx: CommandContext) -> CommandResult {
    CommandResult::SetInputMode(InputMode::Emacs)
}
```

Add `CommandResult::SetInputMode(InputMode)`. `AppState` handles it:
```rust
CommandResult::SetInputMode(mode) => {
    let current = self.input_bar.mode;
    let next = if current == mode { InputMode::Default } else { mode };
    self.input_bar.mode = next;
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
}
```

---

## 5. Implementation Steps

1. Add `InputMode` enum to `input_bar.rs`.
2. Add `mode`, `vim_pending`, `kill_ring` fields to `InputBar`.
3. Implement `handle_vim_normal` key handler (h/j/k/l, i/a, dd, u, x, w/b, 0/$).
4. Implement `handle_vim_visual` key handler (y/p, Esc).
5. Implement Emacs binding overrides in the existing key handler.
6. Add `input_mode: InputMode` to `PromptState`.
7. Render mode chip in `draw_prompt_block`.
8. Add `CommandResult::SetInputMode` variant.
9. Handle `SetInputMode` in `AppState`.
10. Update `VimHandler::execute` and `EmacsHandler::execute`.
11. Tests (see §6).

---

## 6. Tests

```
test vim_handler_sets_vim_insert_mode
test vim_handler_toggles_back_to_default
test emacs_handler_sets_emacs_mode
test input_bar_vim_esc_switches_to_normal
test input_bar_vim_normal_hjkl_moves_cursor
test input_bar_vim_normal_i_enters_insert
test input_bar_vim_normal_dd_deletes_line
test input_bar_emacs_ctrl_a_moves_to_start
test input_bar_emacs_ctrl_k_kills_to_end
test input_bar_emacs_ctrl_y_yanks
test prompt_state_includes_input_mode
test renderer_draws_vim_normal_chip
```
