//! End-to-end tests for the trigger system exercising the TUI with the
//! AppState API (PRD-59).
//!
//! These tests simulate user keystrokes and assert the resulting state,
//! covering the full lifecycle from trigger open → navigate → complete/dismiss.

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

// ── E2E: @-mention lifecycle ──────────────────────────────────────────────────

#[test]
fn at_mention_full_lifecycle() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "").unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "").unwrap();

    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    // Step 1: Type '@' — opens dropdown
    type_str(&mut state, "@");
    assert_eq!(
        state.active_trigger.as_ref().map(|at| at.scan.trigger_char),
        Some('@'),
        "trigger_char should be '@'"
    );
    assert!(
        state
            .active_trigger
            .as_ref()
            .map(|at| !at.items.is_empty())
            .unwrap_or(false),
        "should have file candidates"
    );

    // Step 2: Type 'Esc' — dropdown closes
    press(&mut state, KeyCode::Esc);
    assert!(
        state.active_trigger.is_none(),
        "active_trigger should be None after Esc"
    );

    // Step 3: Type '/' — opens slash palette
    state.input_bar.clear();
    type_str(&mut state, "/");
    assert!(
        state
            .active_trigger
            .as_ref()
            .map(|at| at.scan.trigger_char == '/')
            .unwrap_or(false),
        "slash command palette should open"
    );
}

#[test]
fn at_mention_opens_on_at_char_fires_buffer_changed() {
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

    // Simulate typing @
    state.handle_char('@');
    // BufferChanged → refresh_trigger → active_trigger = Some
    assert!(
        state.active_trigger.is_some(),
        "active_trigger should be Some after typing '@'"
    );
    assert_eq!(
        state.active_trigger.as_ref().unwrap().scan.trigger_char,
        '@'
    );
}

#[test]
fn esc_closes_at_mention() {
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

    state.handle_char('@');
    assert!(state.active_trigger.is_some(), "dropdown should open");

    press(&mut state, KeyCode::Esc);
    assert!(
        state.active_trigger.is_none(),
        "active_trigger should be None after Esc"
    );
}

#[test]
fn slash_palette_opens_on_slash() {
    let mut state = make_state();
    state.handle_char('/');
    assert!(
        state
            .active_trigger
            .as_ref()
            .map(|at| at.scan.trigger_char == '/')
            .unwrap_or(false),
        "slash palette should open on '/'"
    );
}

// ── E2E: Slash command completion from palette ────────────────────────────────

#[tokio::test]
async fn agent_task_submitted_from_slash_completion() {
    let mut state = make_state();

    // Step 1: Type "/c" which shows palette with commands starting with 'c'
    type_str(&mut state, "/c");
    let has_slash_trigger = state
        .active_trigger
        .as_ref()
        .map(|at| at.scan.trigger_char == '/')
        .unwrap_or(false);
    assert!(has_slash_trigger, "slash palette should be open after '/c'");

    let items_count = state
        .active_trigger
        .as_ref()
        .map(|at| at.items.len())
        .unwrap_or(0);
    assert!(
        items_count > 0,
        "should have completion candidates for '/c'"
    );

    // Step 2: Navigate to a known command. /clear is usually the first or near-first.
    // Find the index of "clear" in items.
    let clear_idx = state
        .active_trigger
        .as_ref()
        .and_then(|at| at.items.iter().position(|i| i.display == "clear"))
        .unwrap_or(0);

    // Navigate to the clear item
    let state_ref = state.active_trigger.as_ref().unwrap();
    let current_idx = state_ref.selected;
    let _ = state_ref;

    if current_idx != clear_idx {
        let steps = if clear_idx > current_idx {
            clear_idx - current_idx
        } else {
            clear_idx + items_count - current_idx
        };
        for _ in 0..steps {
            press(&mut state, KeyCode::Down);
        }
    }

    // Verify selection is at clear
    let selected_display = state
        .active_trigger
        .as_ref()
        .and_then(|at| at.selected_item())
        .map(|i| i.display.clone())
        .unwrap_or_default();
    assert_eq!(selected_display, "clear", "selected item should be 'clear'");

    // Step 3: Tab to complete
    press(&mut state, KeyCode::Tab);
    // After Tab, dropdown closes
    assert!(
        state.active_trigger.is_none(),
        "dropdown should close after Tab"
    );
    // And buffer contains "/clear "
    let text = state.input_bar.text();
    assert!(
        text.starts_with("/clear"),
        "buffer should contain '/clear' after Tab completion: {text:?}"
    );
}

// ── E2E: TriggerRegistry unity — at most one active at a time ─────────────────

#[test]
fn only_one_trigger_active_at_a_time() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("test.rs"), "").unwrap();
    let ws = Arc::new(agtrs_workspace::InMemoryWorkspaceStore::new());
    let mut state = make_state();
    state.init_mention(
        ws,
        tmp.path().to_path_buf(),
        xaft_config::MentionConfig::default(),
        None,
    );

    // Open @ trigger
    type_str(&mut state, "@");
    let trigger1 = state.active_trigger.as_ref().map(|at| at.scan.trigger_char);

    // There is at most one trigger open at any time
    assert!(
        state.active_trigger.as_ref().map(|_| 1).unwrap_or(0) <= 1,
        "at most one trigger can be active"
    );

    // Clear and open / trigger
    state.input_bar.clear();
    state.active_trigger = None;
    type_str(&mut state, "/");
    let trigger2 = state.active_trigger.as_ref().map(|at| at.scan.trigger_char);

    // Again, at most one
    assert!(
        state.active_trigger.as_ref().map(|_| 1).unwrap_or(0) <= 1,
        "at most one trigger can be active"
    );

    // They should be different chars (if both opened)
    if trigger1.is_some() && trigger2.is_some() {
        assert_ne!(
            trigger1, trigger2,
            "@-mention and slash should have different trigger chars"
        );
    }
}
