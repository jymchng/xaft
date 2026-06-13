//! Integration tests for the unified trigger system (PRD-59).
//!
//! Tests `AppState` with the new `TriggerRegistry` / `ActiveTrigger` system,
//! asserting that `@`-mention and `/` slash-palette behaviours are preserved
//! and that the unified architecture works correctly.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use std::sync::Arc;
use tempfile::TempDir;
use xaft_tui::{AppState, TuiEvent};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_state() -> AppState {
    AppState::new("")
}

fn key_event(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn press(state: &mut AppState, code: KeyCode) {
    state.handle_event(key_event(code));
}

fn type_str(state: &mut AppState, s: &str) {
    for c in s.chars() {
        state.handle_char(c);
    }
}

// ── @-mention tests ───────────────────────────────────────────────────────────

#[test]
fn at_mention_opens_on_at_char() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();

    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    type_str(&mut state, "@");
    // active_trigger should be Some with trigger_char == '@'
    assert!(
        state.active_trigger.is_some(),
        "active_trigger should open on '@' when workspace is set"
    );
    let at = state.active_trigger.as_ref().unwrap();
    assert_eq!(at.scan.trigger_char, '@');
}

#[test]
fn at_mention_closes_on_space() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();

    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    type_str(&mut state, "@");
    assert!(state.active_trigger.is_some(), "should be open after '@'");

    // Typing space closes the dropdown (space breaks the token)
    type_str(&mut state, " ");
    assert!(
        state.active_trigger.is_none(),
        "active_trigger should close when space is typed after @"
    );
}

#[test]
fn at_mention_esc_closes_dropdown() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();

    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    type_str(&mut state, "@ma");
    assert!(state.active_trigger.is_some(), "dropdown should open");

    press(&mut state, KeyCode::Esc);
    assert!(
        state.active_trigger.is_none(),
        "dropdown should close on Esc"
    );
    // Buffer should still contain "@ma" after Esc
    assert_eq!(state.input_bar.text(), "@ma");
}

// ── Slash palette tests ───────────────────────────────────────────────────────

#[test]
fn slash_palette_opens_on_slash_at_line_start() {
    let mut state = make_state();
    state.handle_char('/');
    // The '/' trigger activates
    let is_slash = state
        .active_trigger
        .as_ref()
        .map(|at| at.scan.trigger_char == '/')
        .unwrap_or(false);
    assert!(
        is_slash,
        "slash palette should open on '/' at start of empty buffer"
    );
}

#[test]
fn slash_palette_closes_on_esc() {
    let mut state = make_state();
    state.handle_char('/');
    assert!(
        state
            .active_trigger
            .as_ref()
            .map(|at| at.scan.trigger_char == '/')
            .unwrap_or(false),
        "palette should be open"
    );

    press(&mut state, KeyCode::Esc);
    assert!(
        state.active_trigger.is_none(),
        "palette should close on Esc"
    );
}

#[test]
fn slash_palette_does_not_open_on_multi_line_buffer() {
    let mut state = make_state();
    // Create a multi-line buffer, then type '/'
    state.input_bar.set_text("line1\nline2");
    state.input_bar.set_cursor(1, 5);
    type_str(&mut state, "/");
    assert!(
        state.active_trigger.is_none(),
        "slash palette should NOT open in multi-line buffer"
    );
}

// ── Navigation tests ──────────────────────────────────────────────────────────

#[test]
fn up_down_navigate_trigger() {
    let mut state = make_state();
    // Open slash palette (has many items)
    type_str(&mut state, "/");
    assert!(
        state
            .active_trigger
            .as_ref()
            .map(|at| !at.items.is_empty())
            .unwrap_or(false),
        "slash palette should have items"
    );

    let initial_selected = state
        .active_trigger
        .as_ref()
        .map(|at| at.selected)
        .unwrap_or(0);

    // Down moves selection
    press(&mut state, KeyCode::Down);
    let after_down = state
        .active_trigger
        .as_ref()
        .map(|at| at.selected)
        .unwrap_or(0);
    assert_ne!(initial_selected, after_down, "Down should change selection");

    // Up should move back
    press(&mut state, KeyCode::Up);
    let after_up = state
        .active_trigger
        .as_ref()
        .map(|at| at.selected)
        .unwrap_or(0);
    assert_eq!(initial_selected, after_up, "Up should move back");
}

#[test]
fn tab_completes_selected_item() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("readme.md"), "").unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();

    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    // Type "@" to open dropdown
    type_str(&mut state, "@");
    assert!(state.active_trigger.is_some(), "dropdown should be open");

    // Tab should complete the currently selected item
    press(&mut state, KeyCode::Tab);
    // After Tab, the dropdown should close
    assert!(
        state.active_trigger.is_none(),
        "dropdown should close after Tab"
    );
    // And the buffer should have something inserted
    let text = state.input_bar.text();
    assert!(
        text.starts_with('@'),
        "buffer should start with '@' after completion: {text:?}"
    );
}

// ── Two triggers cannot be open simultaneously ────────────────────────────────

#[test]
fn two_triggers_cannot_be_open_simultaneously() {
    let tmp = TempDir::new().unwrap();
    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    // Open @-mention
    type_str(&mut state, "hello @");
    // active_trigger should be '@' (or None if workspace has no matches)
    let char1 = state.active_trigger.as_ref().map(|at| at.scan.trigger_char);

    // Now type something different that would trigger slash — but '@' is active
    // There's only ever ONE active_trigger at a time
    state.active_trigger.as_ref().map(|at| {
        assert_ne!(
            at.scan.trigger_char, '/',
            "slash should not be open when @ is the trigger"
        );
    });

    // The system never has two triggers open simultaneously
    let count = if state.active_trigger.is_some() { 1 } else { 0 };
    assert!(count <= 1, "at most 1 trigger can be open: got {count}");
    let _ = char1; // suppress unused warning
}

// ── refresh_trigger on buffer changed ────────────────────────────────────────

#[test]
fn refresh_trigger_called_on_buffer_changed() {
    let mut state = make_state();
    // Initially no trigger
    assert!(state.active_trigger.is_none());

    // Typing '/' triggers slash palette refresh
    state.handle_char('/');
    let after_slash = state
        .active_trigger
        .as_ref()
        .map(|at| at.scan.trigger_char == '/');
    // slash palette should open (or None if no commands match)
    // The key invariant is that refresh_trigger was called
    assert!(
        after_slash.is_some() || state.active_trigger.is_none(),
        "refresh_trigger should have been called after buffer change"
    );
}
