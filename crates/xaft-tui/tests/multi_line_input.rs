//! End-to-end tests for the multi-line input bar (F21).
//!
//! These tests exercise the public surface of `AppState` and the `InputBar`
//! widget through `TuiEvent::Key` and `TuiEvent::Paste` to verify that:
//!
//! - Shift+Enter / Alt+Enter / Ctrl+J insert newlines
//! - Enter submits the full multi-line buffer as one message
//! - Bracketed paste preserves embedded newlines
//! - Cursor movement works across lines
//! - Existing single-line behaviour still works (regression)

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::{AppState, TuiEvent, input_bar::InputBar, prompt::build_prompt};

fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn make_state() -> AppState {
    AppState::new("")
}

// ── Shift+Enter path ──────────────────────────────────────────────────────

#[test]
fn shift_enter_inserts_newline() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('a'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('b'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::SHIFT)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('c'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('d'), KeyModifiers::NONE)));
    assert_eq!(s.input_bar.text(), "ab\ncd");
    assert_eq!(s.input_bar.line_count(), 2);
}

#[test]
fn alt_enter_inserts_newline_fallback() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('x'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::ALT)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('y'), KeyModifiers::NONE)));
    assert_eq!(s.input_bar.text(), "x\ny");
}

#[test]
fn ctrl_j_inserts_newline_universal() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('1'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('j'), KeyModifiers::CONTROL)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('2'), KeyModifiers::NONE)));
    assert_eq!(s.input_bar.text(), "1\n2");
}

// ── Submit path ───────────────────────────────────────────────────────────

#[test]
fn enter_submits_multi_line_buffer() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xaft_tui::UserMessage>();
    let mut s = make_state();
    s.user_message_tx = Some(tx);

    s.handle_event(TuiEvent::Key(k(KeyCode::Char('f'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::SHIFT)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('s'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::SHIFT)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Char('t'), KeyModifiers::NONE)));
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::NONE)));

    let msg = rx.try_recv().expect("expected user message to be sent");
    assert_eq!(msg.as_text_lossy(), "f\ns\nt");
    assert!(s.input_bar.is_empty());
}

#[test]
fn enter_on_empty_is_noop() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xaft_tui::UserMessage>();
    let mut s = make_state();
    s.user_message_tx = Some(tx);
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(
        rx.try_recv().is_err(),
        "no message should be sent for empty submit"
    );
}

#[test]
fn enter_on_whitespace_only_is_noop() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xaft_tui::UserMessage>();
    let mut s = make_state();
    s.user_message_tx = Some(tx);
    s.input_bar.set_text("   \n   ");
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(rx.try_recv().is_err(), "whitespace-only should not submit");
}

#[test]
fn submit_preserves_internal_whitespace() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xaft_tui::UserMessage>();
    let mut s = make_state();
    s.user_message_tx = Some(tx);
    s.input_bar.set_text("  code:\n    indented()  \n  end  ");
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::NONE)));
    let msg = rx.try_recv().expect("expected user message");
    // Outer trim strips leading "  " and trailing "  " from the WHOLE buffer;
    // internal whitespace (indentation, trailing space in middle line) is preserved.
    assert_eq!(msg.as_text_lossy(), "code:\n    indented()  \n  end");
}

// ── Paste path ────────────────────────────────────────────────────────────

#[test]
fn paste_with_newlines_inserts_lines() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Paste("line1\nline2\nline3".into()));
    assert_eq!(s.input_bar.text(), "line1\nline2\nline3");
    assert_eq!(s.input_bar.line_count(), 3);
}

#[test]
fn paste_crlf_normalized_to_lf() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Paste("a\r\nb\r\nc".into()));
    assert_eq!(s.input_bar.text(), "a\nb\nc");
}

#[test]
fn paste_in_middle_splits_correctly() {
    let mut s = make_state();
    s.input_bar.set_text("AB");
    s.input_bar.set_cursor(0, 1);
    s.handle_event(TuiEvent::Paste("X\nY".into()));
    assert_eq!(s.input_bar.text(), "AX\nYB");
    assert_eq!(s.input_bar.line_count(), 2);
}

#[test]
fn paste_single_line_no_newline() {
    let mut s = make_state();
    s.handle_event(TuiEvent::Paste("just text".into()));
    assert_eq!(s.input_bar.text(), "just text");
    assert_eq!(s.input_bar.line_count(), 1);
}

// ── Cursor movement across lines ─────────────────────────────────────────

#[test]
fn arrow_keys_navigate_lines() {
    let mut s = make_state();
    s.input_bar.set_text("ab\ncd");
    s.input_bar.set_cursor(0, 0);

    s.handle_event(TuiEvent::Key(k(KeyCode::Right, KeyModifiers::NONE)));
    assert_eq!(
        s.input_bar.cursor(),
        xaft_tui::input_bar::Cursor { row: 0, col: 1 }
    );

    s.handle_event(TuiEvent::Key(k(KeyCode::Right, KeyModifiers::NONE)));
    assert_eq!(
        s.input_bar.cursor(),
        xaft_tui::input_bar::Cursor { row: 0, col: 2 }
    );

    s.handle_event(TuiEvent::Key(k(KeyCode::Right, KeyModifiers::NONE)));
    assert_eq!(
        s.input_bar.cursor(),
        xaft_tui::input_bar::Cursor { row: 1, col: 0 }
    );

    s.handle_event(TuiEvent::Key(k(KeyCode::Down, KeyModifiers::NONE)));
    // last line — no-op
    assert_eq!(
        s.input_bar.cursor(),
        xaft_tui::input_bar::Cursor { row: 1, col: 0 }
    );

    s.handle_event(TuiEvent::Key(k(KeyCode::Up, KeyModifiers::NONE)));
    assert_eq!(
        s.input_bar.cursor(),
        xaft_tui::input_bar::Cursor { row: 0, col: 0 }
    );
}

#[test]
fn up_at_first_line_is_noop() {
    let mut s = make_state();
    s.input_bar.set_text("hi");
    let start = s.input_bar.cursor();
    s.handle_event(TuiEvent::Key(k(KeyCode::Up, KeyModifiers::NONE)));
    assert_eq!(s.input_bar.cursor(), start);
}

// ── Backspace across lines ────────────────────────────────────────────────

#[test]
fn backspace_merges_with_previous_line() {
    let mut s = make_state();
    s.input_bar.set_text("ab\ncd");
    s.input_bar.set_cursor(1, 0);
    s.handle_event(TuiEvent::Key(k(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_eq!(s.input_bar.text(), "abcd");
    assert_eq!(s.input_bar.line_count(), 1);
}

// ── Render prompt reflects multi-line state ───────────────────────────────

#[test]
fn build_prompt_reflects_multiline_buffer() {
    let mut s = make_state();
    s.input_bar.set_text("first\nsecond");
    let prompt = build_prompt(&s);
    assert_eq!(
        prompt.lines,
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(
        prompt.cursor,
        xaft_tui::input_bar::Cursor { row: 1, col: 6 }
    );
    assert!(!prompt.is_empty);
}

#[test]
fn build_prompt_empty_state() {
    let s = make_state();
    let prompt = build_prompt(&s);
    assert!(prompt.lines.is_empty() || prompt.lines == vec![String::new()]);
    assert!(prompt.is_empty);
}

// ── Regression: single-line submit still works ───────────────────────────

#[test]
fn regression_single_line_submit() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xaft_tui::UserMessage>();
    let mut s = make_state();
    s.user_message_tx = Some(tx);
    for c in "hello world".chars() {
        s.handle_event(TuiEvent::Key(k(KeyCode::Char(c), KeyModifiers::NONE)));
    }
    s.handle_event(TuiEvent::Key(k(KeyCode::Enter, KeyModifiers::NONE)));
    let msg = rx.try_recv().expect("expected user message");
    assert_eq!(msg.as_text_lossy(), "hello world");
    assert!(s.input_bar.is_empty());
}

#[test]
fn regression_backspace_works() {
    let mut s = make_state();
    for c in "hello".chars() {
        s.handle_event(TuiEvent::Key(k(KeyCode::Char(c), KeyModifiers::NONE)));
    }
    s.handle_event(TuiEvent::Key(k(KeyCode::Backspace, KeyModifiers::NONE)));
    assert_eq!(s.input_bar.text(), "hell");
}

// ── Resize updates wrap width ─────────────────────────────────────────────

#[test]
fn resize_updates_input_bar() {
    let mut s = make_state();
    s.input_bar.on_resize(80);
    assert_eq!(s.input_bar.term_cols(), 80);
    assert_eq!(s.input_bar.wrap_width(), 78);
}

// ── InputBar widget standalone ────────────────────────────────────────────

#[test]
fn inputbar_widget_round_trip() {
    let mut bar = InputBar::new(80);
    for c in "function foo() {\n  return 42;\n}".chars() {
        bar.insert_str(&c.to_string());
    }
    assert_eq!(bar.text(), "function foo() {\n  return 42;\n}");
    assert_eq!(bar.line_count(), 3);
    assert!(bar.visible_rows() >= 3);
}

// ── Rendering lifecycle (regression for "eats the line above" bug) ───────

#[test]
fn render_lifecycle_multiline_does_not_eat_above() {
    // Construct a renderer, type a multi-line buffer, verify the transcript
    // line above the prompt is preserved.
    use xaft_tui::IncrementalRenderer;
    use xaft_tui::prompt::PromptState;
    use xaft_tui::renderer::TestCapture;
    use xaft_tui::theme::Theme;
    use xaft_tui::transcript::{LineKind, StyledLine};

    let capture = TestCapture::new(80, 24);
    let mut r = IncrementalRenderer::with_writer(capture);
    let theme = Theme::dark();

    r.init_prompt(&PromptState::default(), &theme).unwrap();
    // Commit a transcript line.
    r.commit_line(
        &StyledLine::new("TRANSCRIPT_ABOVE".to_string(), LineKind::AgentText),
        &theme,
    )
    .unwrap();
    // Now type a 3-line input.
    let prompt = PromptState {
        lines: vec!["a".into(), "b".into(), "c".into()],
        cursor: xaft_tui::input_bar::Cursor { row: 2, col: 1 },
        scroll_top: 0,
        agent_active: false,
        is_empty: false,
        hidden_above: 0,
        hidden_below: 0,
        autocomplete: None,
        slash_palette: None,
    };
    r.update_prompt(&prompt, &theme).unwrap();
    let output = r.out.plain_text();
    assert!(
        output.contains("TRANSCRIPT_ABOVE"),
        "transcript line must survive multi-line update: {output:?}"
    );
    assert!(output.contains("a"), "line 0: {output:?}");
    assert!(output.contains("b"), "line 1: {output:?}");
    assert!(output.contains("c"), "line 2: {output:?}");
    // Block should be 5 rows tall (3 input + 2 borders)
    assert_eq!(r.prompt_block_height(), 5);
}

#[test]
fn render_lifecycle_collapse_after_submit() {
    use xaft_tui::IncrementalRenderer;
    use xaft_tui::prompt::PromptState;
    use xaft_tui::renderer::TestCapture;
    use xaft_tui::theme::Theme;
    use xaft_tui::transcript::{LineKind, StyledLine};

    let capture = TestCapture::new(80, 24);
    let mut r = IncrementalRenderer::with_writer(capture);
    let theme = Theme::dark();

    r.init_prompt(&PromptState::default(), &theme).unwrap();
    // Type 3-line input.
    let multi = PromptState {
        lines: vec!["a".into(), "b".into(), "c".into()],
        cursor: xaft_tui::input_bar::Cursor { row: 2, col: 1 },
        scroll_top: 0,
        agent_active: false,
        is_empty: false,
        hidden_above: 0,
        hidden_below: 0,
        autocomplete: None,
        slash_palette: None,
    };
    r.update_prompt(&multi, &theme).unwrap();
    assert_eq!(r.prompt_block_height(), 5);
    // Submit: commit user message, then update prompt to empty.
    let user_line = StyledLine::new("❯ a\nb\nc".to_string(), LineKind::UserMessage);
    r.commit_line(&user_line, &theme).unwrap();
    // After commit, the renderer still has the multi-line prompt cached.
    // The real flow updates the prompt to empty in a subsequent mutation.
    r.update_prompt(&PromptState::default(), &theme).unwrap();
    // After the empty update, the block collapses back to 1 input row.
    assert_eq!(r.prompt_block_height(), 3);
    let output = r.out.plain_text();
    assert!(output.contains("❯ a"), "submitted msg: {output:?}");
    assert!(output.contains("b"), "line b: {output:?}");
    assert!(output.contains("c"), "line c: {output:?}");
}
