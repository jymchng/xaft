//! Integration tests for the menu widget system (PRDs 61 + 62).
//!
//! Tests the full cycle:
//!   - `CommandResult::OpenMenu` → `AppState::apply_command_result` → `menu_driver` active
//!   - Key routing via `handle_event(TuiEvent::Key(..))` when a menu is open
//!   - Menu close on Done / Cancel
//!   - Normal key handling when no menu is open

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::menu::{
    CommandMenuContext, CommandMenuRegistry, MenuDriver, MenuPayload, MenuResult, MenuWidget,
};
use xaft_tui::prompt::build_prompt;
use xaft_tui::slash::CommandResult;
use xaft_tui::{AppState, TuiEvent};

use std::io::{self, Write};

// ── Test widget helpers ───────────────────────────────────────────────────────

/// A widget that returns `Done(Empty)` on Enter, `Cancel` on Esc, and
/// `Continue` on every other key.
struct EchoWidget;

impl MenuWidget for EchoWidget {
    fn render(&self, out: &mut dyn Write, _: (u16, u16), _: usize) -> io::Result<usize> {
        out.write_all(b"[EchoWidget]\n")?;
        Ok(1)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        match key.code {
            KeyCode::Enter => MenuResult::Done(MenuPayload::Empty),
            KeyCode::Esc => MenuResult::Cancel,
            _ => MenuResult::Continue,
        }
    }

    fn title(&self) -> &str {
        "echo"
    }
}

/// A widget that returns `Done(Selected("hello"))` on Enter.
struct SelectWidget;

impl MenuWidget for SelectWidget {
    fn render(&self, out: &mut dyn Write, _: (u16, u16), _: usize) -> io::Result<usize> {
        out.write_all(b"[SelectWidget]\n")?;
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
        "select"
    }
}

// ── Key helpers ───────────────────────────────────────────────────────────────

fn key_event(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_app_state_open_menu_via_command_result() {
    let mut state = AppState::new("");
    assert!(!state.menu_driver.is_active(), "no menu on fresh state");

    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));
    assert!(
        state.menu_driver.is_active(),
        "menu_driver should be active after OpenMenu"
    );
}

#[test]
fn test_app_state_key_routes_to_menu_and_closes_on_done() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));
    assert!(state.menu_driver.is_active());

    // Enter → Done → menu closes
    state.handle_event(key_event(KeyCode::Enter));
    assert!(
        !state.menu_driver.is_active(),
        "menu should close after Done"
    );
}

#[test]
fn test_app_state_esc_closes_menu() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));
    assert!(state.menu_driver.is_active());

    state.handle_event(key_event(KeyCode::Esc));
    assert!(!state.menu_driver.is_active(), "Esc should close the menu");
}

#[test]
fn test_app_state_menu_blocks_normal_key_handling() {
    let mut state = AppState::new("");
    // Ensure input bar starts empty.
    assert!(state.input_bar.is_empty());

    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));
    assert!(state.menu_driver.is_active());

    // Sending 'x' should be consumed by the menu (Continue), NOT go to input_bar.
    state.handle_event(key_event(KeyCode::Char('x')));
    assert!(
        state.input_bar.is_empty(),
        "input_bar should remain empty while menu is active"
    );
    assert!(
        state.menu_driver.is_active(),
        "menu should still be active after Continue"
    );
}

#[test]
fn test_app_state_no_menu_normal_key_works() {
    let mut state = AppState::new("");
    assert!(!state.menu_driver.is_active());

    // Without a menu, 'x' should go to the input bar.
    state.handle_event(key_event(KeyCode::Char('x')));
    assert_eq!(
        state.input_bar.text(),
        "x",
        "char key should reach input_bar when no menu"
    );
}

#[test]
fn test_app_state_menu_done_with_selected_inserts_into_input_bar() {
    let mut state = AppState::new("");
    // Make sure input bar is empty first.
    state.input_bar.clear();

    state.apply_command_result(CommandResult::OpenMenu(Box::new(SelectWidget)));
    // Enter triggers Done(Selected("hello")) which should be inserted into input_bar.
    state.handle_event(key_event(KeyCode::Enter));
    assert!(!state.menu_driver.is_active(), "menu closed after Done");
    assert_eq!(
        state.input_bar.text(),
        "hello",
        "selected text should be inserted into input_bar"
    );
}

#[test]
fn test_command_menu_registry_wired_in_app_state() {
    let mut state = AppState::new("");
    // The registry exists and starts empty (no commands registered by default).
    assert!(
        state.menu_registry.is_empty(),
        "registry should start empty (no built-in menus yet)"
    );
}

#[test]
fn test_prompt_state_menu_active_false_by_default() {
    let mut state = AppState::new("");
    let prompt = build_prompt(&mut state);
    assert!(
        !prompt.menu_active,
        "menu_active should be false on fresh state"
    );
}

#[test]
fn test_prompt_state_menu_active_true_when_driver_active() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));
    let prompt = build_prompt(&mut state);
    assert!(
        prompt.menu_active,
        "menu_active should be true when a menu is open"
    );
}

#[test]
fn test_continue_key_does_not_close_menu() {
    let mut state = AppState::new("");
    state.apply_command_result(CommandResult::OpenMenu(Box::new(EchoWidget)));

    // Tab → Continue (EchoWidget) — menu stays open.
    state.handle_event(key_event(KeyCode::Tab));
    assert!(
        state.menu_driver.is_active(),
        "menu should remain open on Continue key"
    );
}

#[test]
fn test_open_menu_via_registry_factory() {
    let mut state = AppState::new("");
    state.menu_registry.register(
        "testmenu",
        Box::new(|_ctx| Box::new(EchoWidget) as Box<dyn MenuWidget>),
    );

    let opened = state.open_menu_by_name("testmenu");
    assert!(
        opened,
        "open_menu_by_name should return true for known command"
    );
    assert!(
        state.menu_driver.is_active(),
        "menu should be active after opening via registry"
    );
}

#[test]
fn test_open_unknown_menu_returns_false() {
    let mut state = AppState::new("");
    let opened = state.open_menu_by_name("nonexistent");
    assert!(
        !opened,
        "open_menu_by_name should return false for unknown command"
    );
    assert!(!state.menu_driver.is_active());
}
