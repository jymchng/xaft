//! End-to-end tests for the menu widget system (PRDs 61 + 62).
//!
//! These tests simulate the full user interaction loop via `AppState` events,
//! exercising the menu open → navigate → close lifecycle alongside the normal
//! TUI event flow.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::menu::{MenuPayload, MenuResult, MenuWidget};
use xaft_tui::slash::CommandResult;
use xaft_tui::transcript::{LineKind, RenderMutation};
use xaft_tui::{AppState, TuiEvent};

use std::io::{self, Write};

// ── Test widgets ──────────────────────────────────────────────────────────────

/// Widget that returns `Done(Selected("hello"))` on Enter, `Cancel` on Esc.
struct DoneWidget;

impl MenuWidget for DoneWidget {
    fn render(&self, out: &mut dyn Write, _: (u16, u16), _: usize) -> io::Result<usize> {
        out.write_all(b"[DoneWidget]\n")?;
        Ok(1)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            KeyCode::Enter => MenuResult::Done(MenuPayload::Selected("hello".to_string())),
            KeyCode::Esc => MenuResult::Cancel,
            _ => MenuResult::Continue,
        }
    }

    fn title(&self) -> &str {
        "done"
    }
}

/// Widget that returns `Continue` on all keys — never finishes on its own.
struct StickyWidget;

impl MenuWidget for StickyWidget {
    fn render(&self, out: &mut dyn Write, _: (u16, u16), _: usize) -> io::Result<usize> {
        out.write_all(b"[StickyWidget]\n")?;
        Ok(1)
    }

    fn handle_key(&mut self, _key: KeyEvent) -> MenuResult {
        MenuResult::Continue
    }

    fn title(&self) -> &str {
        "sticky"
    }
}

// ── Key/event helpers ─────────────────────────────────────────────────────────

fn key_event(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn commit_line_texts(state: &AppState) -> Vec<String> {
    state
        .mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect()
}

// ── E2E tests ─────────────────────────────────────────────────────────────────

/// Registering a factory in menu_registry and calling `open_menu_by_name`
/// activates the menu driver.
#[test]
fn test_menu_opens_when_slash_command_registered() {
    let mut state = AppState::new("");
    state.menu_registry.register(
        "testmenu",
        Box::new(|_ctx| Box::new(DoneWidget) as Box<dyn MenuWidget>),
    );

    let opened = state.open_menu_by_name("testmenu");
    assert!(opened, "should open menu for registered command");
    assert!(state.menu_driver.is_active(), "menu_driver must be active");
}

/// Opening a menu via command result, then Enter → Done(Selected("hello"))
/// inserts the text into the input bar.
#[test]
fn test_menu_done_inserts_into_input_bar() {
    let mut state = AppState::new("");
    state.input_bar.clear();

    state.apply_command_result(CommandResult::OpenMenu(Box::new(DoneWidget)));
    assert!(state.menu_driver.is_active());

    state.handle_event(key_event(KeyCode::Enter));
    assert!(!state.menu_driver.is_active(), "menu closed after Done");
    assert_eq!(
        state.input_bar.text(),
        "hello",
        "selected text in input bar"
    );
}

/// Opening a menu then pressing Esc closes it and returns to normal input.
#[test]
fn test_menu_cancel_returns_to_input() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(DoneWidget)));
    assert!(state.menu_driver.is_active());

    state.handle_event(key_event(KeyCode::Esc));
    assert!(!state.menu_driver.is_active(), "Esc closes the menu");

    // Normal input bar should work after the menu is closed.
    state.handle_event(key_event(KeyCode::Char('a')));
    assert_eq!(
        state.input_bar.text(),
        "a",
        "input_bar accepts keys after menu closes"
    );
}

/// While a menu is open, sending character keys that would normally affect the
/// input bar are captured by the menu (returned Continue) — input_bar unchanged.
/// Agent state is not disturbed when a menu is opened and closed.
#[test]
fn test_agent_task_not_interrupted_by_menu() {
    let mut state = AppState::new("fix the bug");

    // Simulate an agent running.
    state.handle_event(TuiEvent::LlmCallStarting {
        agent_name: "planner".to_string(),
        call_index: 0,
    });

    // Open a menu — agent state should be unaffected.
    // Use DoneWidget because StickyWidget ignores Esc.
    state.apply_command_result(CommandResult::OpenMenu(Box::new(DoneWidget)));
    assert!(state.menu_driver.is_active());

    // The phase / agent tracking should be unchanged.
    assert_eq!(state.current_agent, "planner");

    // Send Esc to close the menu (DoneWidget returns Cancel on Esc).
    state.handle_event(key_event(KeyCode::Esc));
    assert!(
        !state.menu_driver.is_active(),
        "menu should be closed after Esc"
    );

    // Agent state still intact.
    assert_eq!(state.current_agent, "planner");
}

/// Multiple menus can be opened sequentially; each replaces the previous.
#[test]
fn test_sequential_menus() {
    let mut state = AppState::new("");

    state.apply_command_result(CommandResult::OpenMenu(Box::new(StickyWidget)));
    assert!(state.menu_driver.is_active());
    assert_eq!(state.menu_driver.widget().unwrap().title(), "sticky");

    // Open a second menu — replaces the first.
    state.apply_command_result(CommandResult::OpenMenu(Box::new(DoneWidget)));
    assert!(state.menu_driver.is_active());
    assert_eq!(state.menu_driver.widget().unwrap().title(), "done");

    // Close via Esc.
    state.handle_event(key_event(KeyCode::Esc));
    assert!(!state.menu_driver.is_active());
}

/// `CommandResult::Error` produces a commit line with "✗" prefix.
#[test]
fn test_command_result_error_produces_error_line() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::Error("something broke".to_string()));

    let texts = commit_line_texts(&state);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("✗") && t.contains("something broke")),
        "error line missing: {texts:?}"
    );
}

/// `CommandResult::Lines` produces system commit lines.
#[test]
fn test_command_result_lines_produce_system_lines() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::Lines(vec![
        "line one".to_string(),
        "line two".to_string(),
    ]));

    let texts = commit_line_texts(&state);
    assert!(
        texts.iter().any(|t| t.contains("line one")),
        "line one missing: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("line two")),
        "line two missing: {texts:?}"
    );
}

/// After a menu closes, the prompt update mutation is present.
#[test]
fn test_menu_close_emits_prompt_update() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(DoneWidget)));
    state.mutations.clear(); // Clear open mutations.

    state.handle_event(key_event(KeyCode::Esc));

    let has_prompt_update = state
        .mutations
        .iter()
        .any(|m| matches!(m, RenderMutation::UpdatePrompt(_)));
    assert!(
        has_prompt_update,
        "prompt update mutation expected after menu close"
    );
}
