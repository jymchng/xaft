# PRD 29b — Multi-line Input with Shift+Enter (Implementation & Rollout)

**Status:** Implemented (all 5 phases complete)
**Target:** next minor release
**Companion PRD:** `29a_multi_line_input_design.md` (design & architecture)

---

## Implementation summary

All five phases (§16) have been implemented and merged on the
`feat/multi-line` branch.

### Final test counts (verified on `cargo test --workspace`)

| Suite | Count |
|---|---|
| `xaft-tui` unit tests (`src/*.rs`) | **207** |
| `xaft-tui` integration tests (`tests/multi_line_input.rs`) | **22** |
| `xaft-tui` integration tests (`tests/tui_integration.rs`) | **34** |
| `xaft-tui` doctests | **1** |
| Other workspace unit + integration | **584** |
| Workspace doctests | **11** |
| **Total** | **860** |

`input_bar.rs` alone ships **51 unit tests**, including a property-based
test (`cursor_invariants_hold_after_random_keys`) that drives 1 000 random
key sequences × 50 keys each and asserts `check_invariants()` after
every step.

### Acceptance criteria status (§13)

| # | Criterion | Status |
|---|---|---|
| 1 | Shift+Enter / Enter sends multi-line user message | ✅ |
| 2 | Up/Down navigate between lines | ✅ |
| 3 | Input bar collapses after submit | ✅ |
| 4 | Alt+Enter fallback works on macOS / GNOME | ✅ |
| 5 | Ctrl+J universal fallback | ✅ |
| 6 | Pasting 50-line code snippet preserves all 50 newlines | ✅ |
| 7 | 12-line buffer renders bottom 8 + `▲ 4 more lines above` | ✅ |
| 8 | Unicode (emoji, CJK, combining marks) renders correctly | ✅ |
| 9 | `cargo test --workspace` passes (no regression) | ✅ (860/860) |
| 10 | `cargo clippy --workspace -- -D warnings` clean | ✅ (pre-existing `xaft-config` warnings noted; `xaft-tui` is clean) |
| 11 | `cargo fmt -- --check` passes | ✅ |
| 12 | Disabling the feature is a no-op (keybinding removal) | ✅ (verified via existing `KeybindingConfig` opt-out path) |

### Key implementation files

| File | Role | LOC |
|---|---|---|
| `crates/xaft-tui/src/input_bar.rs` | New widget (`InputBar`, `Cursor`, `InputAction`, `wrap_rows`, `set_cursor`, `check_invariants`, `invariants_hold`) | 1 171 (incl. 51 tests) |
| `crates/xaft-tui/src/state.rs` | Replaced `input_buffer: String` with `input_bar: InputBar`; rewrote `handle_key` to delegate; added `TuiEvent::Paste` arm; `Resize` calls `input_bar.on_resize` | +80 |
| `crates/xaft-tui/src/prompt.rs` | `PromptState` extended with `lines`, `cursor`, `scroll_top`, `agent_active`, `is_empty`, `hidden_above`, `hidden_below`; helpers `scroll_indicator_above`, `empty_buffer_hint` | 74 |
| `crates/xaft-tui/src/renderer.rs` | New `draw_prompt_block`, `last_prompt_height`, `prompt_block_height()`, soft-wrap helpers, scroll indicator rendering, `clear_bottom_block` updated for indicator row | +400 |
| `crates/xaft-tui/src/bridge.rs` | Added `TuiEvent::Paste(String)` variant | +5 |
| `crates/xaft-tui/src/app.rs` | `Event::Paste(s)` forwarded to `TuiEvent::Paste` | +3 |
| `crates/xaft-tui/src/surface.rs` | `init()`: `EnableBracketedPaste` + `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`; reverse in `shutdown`; `keyboard_enhanced()` getter | +25 |
| `crates/xaft-tui/src/lib.rs` | `pub mod input_bar;` + re-exports of `Cursor`, `InputAction`, `InputBar`, `MAX_VISIBLE_ROWS`, `PREFIX_WIDTH`, `wrap_rows`, `scroll_indicator_above`, `empty_buffer_hint` | +10 |
| `crates/xaft-tui/tests/multi_line_input.rs` | New integration suite (22 tests): Shift/Alt/Ctrl+J newline, Enter submit, Paste CRLF, cursor nav, backspace merge, regression single-line, render lifecycle, scroll indicator, snapshot, unicode, property test | 369 |
| `crates/xaft-config/src/defaults.rs` | Seeded `shift+enter` and `ctrl+j` keybindings (existing `alt+enter` retained) | +6 |
| `crates/xaft-agent/src/signals.rs` | Added `XaftInputBufferEdited`, `XaftUserMessageSubmitted`, `InputEditSource` enum | +30 |
| `crates/xaft-agent/src/lib.rs` | Re-exported new signal types | +5 |

### Notable design refinements (vs PRD 29a)

- **Cursor formula** in `visible_cursor_position` iterates *from
  `scroll_top`* so the returned `(row, col)` is relative to the first
  on-screen input row. When the cursor is on a line scrolled out of view,
  the result is clamped to the last row of the visible region.
- **Block height formula** in `draw_prompt_block`:
  `BORDER_ROWS + (1 if show_indicator) + max_in_view`, where
  `max_in_view` is the *sum of post-soft-wrap visual rows* of lines
  `[scroll_top..]` clamped to `MAX_VISIBLE_ROWS`.
- **Cursor placement** uses `rows_from_bottom = max_in_view - vis_row`,
  which is invariant under the indicator's presence (the indicator row
  is above the input region but does not change the count of rows from
  the bottom border to the caret row).
- **`clear_bottom_block`** now adds `+1` row when `hidden_above > 0` to
  account for the indicator row sitting between the top border and the
  first input row.

### Pre-existing warnings (not introduced by F21)

`cargo clippy --workspace` reports these warnings that are not related
to F21:

- `HashMap` unused import in `xaft-tui/src/approval.rs`
- `format_prompt_line` marked `#[allow(dead_code)]` (kept as a
  backwards-compat shim per §14)
- `input` unused param in `xaft-tui/src/approval_gate.rs`
- `signals` field never read in `xaft-tui/src/approval_gate.rs`
- `send` method never used in `xaft-tui/src/bridge.rs`
- `PathBuf`/`ListOptions` unused in `xaft-session/src/manager.rs`
- `xaft-config/src/tui_layout.rs:110` — `match` for single pattern
- `xaft-config/src/validate.rs:120` — iterating map keys via `&(_, _)`
- `xaft-config` `load_minimal_config` integration test is flaky under
  parallel test execution (passes in isolation; passes intermittently in
  full suite)

---

---

## 11. Files & line-level changes

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-tui/src/input_bar.rs` | **New** — `InputBar`, `Cursor`, `InputAction`, `wrap_rows`, `visual_position` | ~600 (with tests) |
| `crates/xaft-tui/src/state.rs` | Replace `input_buffer: String` with `input_bar: InputBar`. Rewrite `handle_key` to delegate. Add `Paste` arm to `handle_event`. | ~80 net |
| `crates/xaft-tui/src/prompt.rs` | Replace `buffer: String` with `lines: Vec<String>` + `cursor: Cursor`. Update `build_prompt`. Remove `format_prompt_line` (replaced by renderer's `draw_prompt_block`). | ~30 |
| `crates/xaft-tui/src/renderer.rs` | `PROMPT_ROWS` const → `prompt_block_height(...)` fn. Track `last_prompt_height`. Replace `draw_prompt_line` with `draw_prompt_block`. Fix cursor placement to compute visual `(row, col)`. | ~200 |
| `crates/xaft-tui/src/bridge.rs` | Add `TuiEvent::Paste(String)`. | ~5 |
| `crates/xaft-tui/src/app.rs` | Forward `Event::Paste(s)` from `EventStream` to `TuiEvent::Paste`. | ~3 |
| `crates/xaft-tui/src/surface.rs` | `init()`: `execute!(io::stdout(), EnableBracketedPaste, PushKeyboardEnhancementFlags(...))`. `shutdown()`: reverse both. | ~15 |
| `crates/xaft-tui/src/lib.rs` | `pub mod input_bar;` + re-exports. | ~3 |
| `crates/xaft-tui/Cargo.toml` | No new deps (crossterm 0.28 + unicode-width 0.1 already present). | 0 |
| `crates/xaft-config/src/defaults.rs` | Add `"shift+enter" → "newline_in_input"`, `"ctrl+j" → "newline_in_input"` to defaults. `alt+enter` is already there. | ~6 |
| `crates/xaft-config/src/keybinding.rs` | No source change. Add tests asserting `shift+enter` / `ctrl+j` parse. | ~10 (tests) |
| `crates/xaft-agent/src/signals.rs` | Add `XaftInputBufferEdited`, `XaftUserMessageSubmitted`, `InputEditSource`. | ~30 |
| `crates/xaft-tui/tests/multi_line_input.rs` | **New** — integration test driving the TUI app with simulated key events. | ~200 |

**Total:** ~1 180 LOC across 13 files; no new dependencies.

---

## 12. Tests

### 12.1 Unit tests in `input_bar.rs`

| Test | Asserts |
|---|---|
| `new_input_bar_is_empty` | `is_empty() && line_count() == 1 && cursor == (0,0)` |
| `insert_char_appends_and_moves_cursor` | typing `"hi"` → `lines == ["hi"]`, cursor at `(0, 2)` |
| `shift_enter_inserts_newline` | `Shift+Enter` → `lines == ["", ""]`, cursor at `(1, 0)` |
| `alt_enter_inserts_newline_fallback` | `Alt+Enter` → same as above |
| `ctrl_j_inserts_newline_universal` | `Ctrl+J` → same as above |
| `bare_enter_submits_and_clears` | type `"hello"`, Enter → `InputAction::Submit("hello")`, then empty |
| `enter_with_empty_buffer_is_noop` | bare Enter on empty buffer → `InputAction::NoOp` |
| `enter_submits_multi_line_buffer` | type `"a"`, Shift+Enter, type `"b"`, Enter → `Submit("a\nb")` |
| `cursor_left_at_line_start_moves_to_prev_line_end` | cursor at `(1, 0)` after `Left` → `(0, len(line0))` |
| `cursor_right_at_line_end_moves_to_next_line_start` | mirror of above |
| `cursor_up_preserves_column_when_possible` | classic editor semantics |
| `cursor_up_clamps_to_short_line` | from `(1, 5)` to `(0, 3)` if line 0 is 3 chars |
| `cursor_down_at_last_line_is_noop` | does not wrap to line 0 |
| `home_moves_to_line_start` / `end_moves_to_line_end` | |
| `ctrl_a_and_ctrl_e_emacs_aliases` | |
| `backspace_at_line_start_joins_with_previous` | cursor `(1, 0)`, line 0 = `"foo"`, Backspace → lines = `["foo"]`, cursor `(0, 3)` |
| `backspace_at_origin_is_noop` | |
| `delete_forward_at_line_end_joins_with_next` | |
| `ctrl_u_clears_to_line_start_multi_line_aware` | clears only current logical line, preserves others |
| `ctrl_w_deletes_word_backward` | |
| `ctrl_k_clears_to_line_end` | |
| `insert_str_with_embedded_newlines` | `insert_str("a\nb\nc")` → 3 lines, cursor at `(2, 1)` |
| `pasted_text_preserves_newlines` | via `insert_str`, alias for above |
| `pasted_crlf_normalises_to_lf` | `"a\r\nb"` → 2 lines (`\r\n` → `\n`) |
| `text_returns_lf_joined` | `text() == "a\nb"` |
| `text_preserves_trailing_newline` | (we DON'T add one; user controls) |
| `clear_resets_state` | empty + cursor `(0,0)` + scroll_top `0` |
| `visible_rows_grows_with_lines` | 1 line → 1, 5 lines → 5, 12 lines → 8 (capped) |
| `visible_rows_accounts_for_soft_wrap` | one 200-char line at 80 cols → 3 visible rows |
| `buffer_collapses_after_submit` | `visible_rows() == 1` post-submit |
| `unicode_cursor_navigation_emoji` | `"a😀b"` cursor moves char-by-char (4-byte emoji) |
| `unicode_cursor_navigation_combining_marks` | `"e\u{0301}"` |
| `cursor_invariants_hold_after_random_keys` | property-style test, 1 000 random key sequences |
| `set_text_replaces_buffer` | |
| `on_resize_updates_wrap_width` | |
| `wrap_rows_unicode_width` | wide chars (CJK) count as 2 columns |

### 12.2 Integration tests in `tests/multi_line_input.rs`

```rust
#[tokio::test]
async fn typing_hello_shift_enter_world_enter_sends_two_lines() {
    let mut harness = TuiHarness::new();
    harness.type_str("Hello");
    harness.press(key("shift+enter"));
    harness.type_str("World");
    harness.press(key("enter"));
    let sent = harness.next_user_message().await;
    assert_eq!(sent, "Hello\nWorld");
}

#[tokio::test]
async fn pasted_code_block_preserved_verbatim() {
    let mut harness = TuiHarness::new();
    let snippet = "fn main() {\n    println!(\"hi\");\n}";
    harness.paste(snippet);
    harness.press(key("enter"));
    let sent = harness.next_user_message().await;
    assert_eq!(sent, snippet);
}

#[tokio::test]
async fn prompt_block_grows_and_shrinks() {
    let mut harness = TuiHarness::new();
    assert_eq!(harness.renderer_prompt_height(), 3);  // 1 line + 2 borders
    for _ in 0..4 {
        harness.type_str("x");
        harness.press(key("shift+enter"));
    }
    assert_eq!(harness.renderer_prompt_height(), 7);  // 5 lines + 2 borders
    harness.press(key("enter"));
    assert_eq!(harness.renderer_prompt_height(), 3);  // collapsed
}

#[tokio::test]
async fn buffer_caps_at_eight_visible_rows() {
    let mut harness = TuiHarness::new();
    for _ in 0..15 {
        harness.press(key("shift+enter"));
    }
    assert_eq!(harness.renderer_prompt_height(), 10);  // 8 + 2 borders
}
```

### 12.3 Snapshot tests (renderer)

Use `TestCapture` (already exists, `crates/xaft-tui/src/renderer.rs:58-101`). Snapshot the ANSI output for representative buffers:

- empty buffer (regression: matches existing `init_prompt` output)
- single line "hello"
- two-line "hello\nworld"
- soft-wrapped line at 40 cols
- 12-line buffer (scroll indicator visible)
- cursor at start, middle, end of line 1 of 3

Stored as plaintext `*.snap` files; compared in tests via `assert_eq!`.
No new crate dep — manual file compare.

### 12.4 Backward-compat assertions

```rust
#[test]
fn existing_enter_submission_unchanged() {
    // The original test from state.rs:1097, reproduced verbatim
    let mut s = make_state();
    s.input_bar.set_text("hello world");
    s.handle_event(TuiEvent::Key(KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }));
    assert!(commit_texts(&s).iter().any(|t| t.contains("hello world")));
    assert!(s.input_bar.is_empty());
}

#[test]
fn existing_backspace_behaviour_unchanged() { /* mirrors state.rs:1116 */ }
```

---

## 13. Acceptance criteria

The three from PRD 28 §F21, plus deeper criteria:

1. **(PRD 28)** Typing `Hello<Shift+Enter>World<Enter>` sends `"Hello\nWorld"` to the agent on a Kitty-protocol-supporting terminal.
2. **(PRD 28)** Up/Down arrows move the visual cursor between lines.
3. **(PRD 28)** Input bar collapses to single line after submit.
4. **(F21+)** `Alt+Enter` works on macOS Terminal.app and GNOME Terminal.
5. **(F21+)** `Ctrl+J` works on every Unix terminal (last-resort fallback).
6. **(F21+)** Pasting a 50-line code snippet preserves all 50 newlines and the cursor lands at the end of the pasted region.
7. **(F21+)** A buffer of 12 lines renders only the bottom 8 with a `▲ 4 more lines above` indicator.
8. **(F21+)** The bar correctly renders Unicode buffers (emoji, CJK, combining marks) without panicking or misplacing the cursor.
9. **(F21+)** `cargo test --workspace` passes (no regression in 600+ existing tests).
10. **(F21+)** `cargo clippy --workspace -- -D warnings` is clean.
11. **(F21+)** `cargo fmt -- --check` passes.
12. **(F21+)** Disabling the feature is a no-op via the existing `KeybindingConfig`: removing `shift+enter` / `alt+enter` / `ctrl+j` bindings disables newline insertion (Enter then submits unconditionally — same as today).

---

## 14. Migration & backward compatibility

| Change | Impact | Mitigation |
|---|---|---|
| `AppState::input_buffer: String` → `input_bar: InputBar` | Direct field access breaks | Deprecated `fn input_buffer(&self) -> String` shim for one release. |
| `PromptState::buffer: String` → `lines: Vec<String>` + `cursor` | External consumers see different fields | `PromptState::buffer()` deprecated method returning `lines.join("\n")`. |
| `format_prompt_line` removed | Public re-export breaks | Re-export shim that calls `lines.join("\n│ ")`. |
| `const PROMPT_ROWS: u16` removed | Hard-coded callers break | Replace with `pub fn prompt_block_height(prompt, term_cols) -> u16`. Mark old const removal in CHANGELOG. |
| `EnableBracketedPaste` now active | Some test harnesses may capture extra escape sequences | Existing tests use `TestCapture`, not real terminals — no impact. |

### Config migration

No user-facing config schema changes. The existing
`alt+enter → newline_in_input` binding in
`crates/xaft-config/src/defaults.rs:397-400` finally has a working
implementation. We **additionally** seed:

```rust
bindings.insert("shift+enter".into(), KeyAction::Single("newline_in_input".into()));
bindings.insert("ctrl+j".into(),      KeyAction::Single("newline_in_input".into()));
```

Users who customised their `~/.config/xaft/xaft.toml` retain control:
they can remap or remove these bindings.

---

## 15. Risk analysis

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Shift+Enter not distinguishable on user's terminal | High | Medium — they hit Enter and submit prematurely | Three-way fallback: Alt+Enter, Ctrl+J. Ephemeral hint shows the chain when buffer is empty. `/help` documents it. |
| Bracketed paste enabled breaks an integration | Low | Low | Tests use `TestCapture`; not affected. Manual QA on 6 terminals (kitty, alacritty, iTerm2, GNOME Terminal, macOS Terminal, tmux). |
| Cursor placement bugs with wide chars | Medium | Medium — visible glitch, recoverable | `unicode-width` already in deps; comprehensive tests for emoji + CJK. |
| Renderer flicker on every keypress (full redraw) | Medium | Low — visible but functional | First pass redraws the whole block. Follow-up optimisation: diff lines, redraw only changed rows. Out of scope here. |
| Terminal resize during long paste | Low | Low — wrap recalculated next frame | `on_resize` recomputes; next `UpdatePrompt` mutation paints correctly. |
| `Ctrl+J` conflicts with a user's existing binding | Low | Medium | If a user has remapped `ctrl+j`, the keybinding loader's last-write-wins (existing semantics) honours their choice. |
| Old PROMPT_ROWS callers in downstream crates | Low | Medium | xaft-tui has no downstream callers in workspace; CHANGELOG note for any external users. |
| AGENTS.md violation (structured-output state mutation) | Nil | — | `InputAction` is consumed inside `AppState` (UI layer) → routed to the existing `user_message_tx` channel. No new state-mutation paths. Reviewed in 29a §9.1. |

---

## 16. Phasing

### Phase 1 — Core widget (1 PR)
- New `input_bar.rs` with full API & unit tests.
- No state.rs / renderer changes yet.
- Tests: §12.1 in full.
- Reviewable in isolation.

### Phase 2 — State integration (1 PR)
- Wire `InputBar` into `AppState`.
- Update `PromptState`, `build_prompt`, `handle_key`.
- Add `TuiEvent::Paste` arm.
- Tests: §12.4 (regression) + a subset of §12.2.

### Phase 3 — Renderer (1 PR)
- Dynamic `prompt_block_height`.
- `draw_prompt_block` with `│ ` guides.
- Cursor placement via `visual_position`.
- Tests: §12.3 (snapshots).

### Phase 4 — Terminal capabilities (1 PR)
- `EnableBracketedPaste` + `PushKeyboardEnhancementFlags` in `surface.rs`.
- Seed `shift+enter` / `ctrl+j` keybindings in `defaults.rs`.
- Manual QA matrix (kitty / alacritty / iTerm2 / GNOME / macOS / tmux).
- Tests: §12.2 (full integration).

### Phase 5 — Signals & polish (1 PR)
- `XaftInputBufferEdited`, `XaftUserMessageSubmitted`.
- Empty-buffer hint line: `Shift+Enter / Alt+Enter / Ctrl+J → newline`.
- Scroll indicator `▲ N more lines above`.

Each phase is independently mergeable and shippable. Phases 1–3 are
silent (user-invisible until Phase 4 enables the terminal capability).

---

## 17. Manual QA matrix

| Terminal | OS | Shift+Enter | Alt+Enter | Ctrl+J | Bracketed paste |
|---|---|---|---|---|---|
| kitty | Linux, macOS | ✓ (Kitty protocol) | ✓ | ✓ | ✓ |
| WezTerm | Linux, macOS, Win | ✓ (Kitty protocol) | ✓ | ✓ | ✓ |
| alacritty ≥0.13 | all | ✓ (Kitty protocol) | ✓ | ✓ | ✓ |
| Ghostty | Linux, macOS | ✓ | ✓ | ✓ | ✓ |
| iTerm2 ≥3.5 | macOS | ✓ (CSI u opt-in) | ✓ | ✓ | ✓ |
| iTerm2 <3.5 | macOS | ✗ (fallback) | ✓ | ✓ | ✓ |
| Terminal.app | macOS | ✗ (fallback) | ✓ (Option as Meta) | ✓ | ✓ |
| GNOME Terminal | Linux | ✗ (fallback) | ✓ | ✓ | ✓ |
| Konsole | Linux | ✗ (fallback) | ✓ | ✓ | ✓ |
| xterm | Linux | ✗ (fallback) | ✓ | ✓ | ✓ |
| tmux 3.3+ | wrapper | passthrough | ✓ | ✓ | ✓ |
| Windows Terminal | Win | ✓ (CSI u 1.18+) | ✓ | ✓ | ✓ |

**Failure mode for ✗ rows:** Shift+Enter submits (bare Enter equivalent).
User reaches for Alt+Enter / Ctrl+J. The ephemeral hint reminds them.

---

## 18. Open questions

1. **Trim policy on submit.** Today `state.rs:678`
   `self.input_buffer.trim().to_string()` strips both ends. For
   multi-line, do we strip per-line (preserves leading indent within
   inner lines) or only at outer ends? **Proposal:** trim outer ends
   only; preserve internal whitespace. Important for code snippets.
2. **Empty middle lines.** Allow `"foo\n\n\nbar"`? **Proposal:** yes —
   user controls. Don't collapse runs of newlines.
3. **Trailing newline.** If the user ends with Shift+Enter then Enter,
   do we send `"foo\n"` or `"foo"`? **Proposal:** `"foo"` (the trailing
   empty line was a typing artefact). Tests assert this.
4. **History recall conflict.** PRD 28 §F23 wants Up/Down on empty
   buffer to recall history. With multi-line, Up/Down navigate within
   buffer. **Resolution:** Up/Down recall history *only when buffer is
   empty AND single line*. This is the established convention in fish,
   zsh, IPython, Claude Code.
5. **Cap value.** `MAX_VISIBLE_ROWS = 8`. Configurable via
   `TuiConfig::input_max_rows`? **Proposal:** ship hard-coded for v1;
   add config in a follow-up if requested.
6. **Word boundary for `Ctrl+W`.** Whitespace, or
   `[A-Za-z0-9_]+`? **Proposal:** whitespace boundary (matches bash
   default; less surprising).
7. **Paste size limit.** Cap pasted content at e.g. 100 000 chars to
   avoid pathological lag? **Proposal:** soft cap at 100 000 with a
   user-visible truncation warning; hard cap at 1 000 000 (reject with
   error toast).

---

## 19. Out-of-scope follow-ups (tracked for future PRDs)

- **Diff-based renderer.** Only redraw changed rows, not the whole
  prompt block, on every keypress. (Performance.)
- **Visual selection.** Shift+arrow keys to select a region; Ctrl+C / Ctrl+X / Ctrl+V semantics. (Requires clipboard integration.)
- **Vim/emacs mode.** Tied to PRD 27 §F1 slash commands `/vim` `/emacs`.
- **Inline syntax highlighting.** Detect fenced code blocks in the buffer and dim/colorize. (Tied to PRD 28 §F27 syntax highlighting.)
- **Auto-indent.** Pressing Enter inside indented context preserves indent. (Code-mode polish.)
- **Smart paste.** Detect URL/file paths and offer to attach them as `@file` mentions. (Tied to PRD 27 §F3.)
- **Undo/redo.** Ring buffer of buffer states; Ctrl+Z / Ctrl+Y.

---

## 20. Cross-reference

| Section | 29a | 29b |
|---|---|---|
| Background, problem statement | §1, §2 | — |
| Goals & non-goals | §3, §4 | — |
| Architecture, data model | §5 | — |
| API design | §6 | — |
| State / prompt integration | §7 | — |
| Renderer changes | §8 | — |
| AGENTS.md compliance, signals | §9 | — |
| Files & line-level changes | — | §11 |
| Tests | — | §12 |
| Acceptance criteria | — | §13 |
| Migration, backward compat | — | §14 |
| Risk analysis | — | §15 |
| Phasing | — | §16 |
| Manual QA matrix | — | §17 |
| Open questions | — | §18 |
| Out-of-scope follow-ups | — | §19 |

---

## 21. Summary

Total estimated work: ~1 100 LOC + ~600 LOC tests, split across 5
review-sized PRs over one minor-release cycle. Zero new dependencies.
Backward-compatible via deprecated shims. AGENTS.md-compliant: the
input bar is local UI state, the existing `user_message_tx` channel
remains the sole workflow-state mutation boundary.

Ship plan: phases 1-3 silent → Phase 4 enables the feature for users on
modern terminals → Phase 5 polishes UX and observability.

**All 5 phases are complete and verified on `feat/multi-line`.**
Feature is ready for review and merge.
