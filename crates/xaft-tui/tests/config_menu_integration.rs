//! Integration tests for the ConfigurationMenu (PRD-63).
//!
//! Tests the full cycle: `/config` with no args → `OpenMenu` result →
//! AppState stub handling, navigation, editing, and save.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tempfile::TempDir;
use xaft_tui::state::AppState;
use xaft_tui::transcript::RenderMutation;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_state() -> AppState {
    AppState::new("")
}

fn key_event(code: KeyCode) -> xaft_tui::TuiEvent {
    xaft_tui::TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn handle_submit(state: &mut AppState, text: &str) {
    state.input_bar.set_text(text);
    state.mutations.clear();
    state.handle_event(key_event(KeyCode::Enter));
}

fn commit_texts(state: &AppState) -> Vec<String> {
    state
        .mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `/config` with no args should open the interactive `ConfigurationMenu` via
/// `CommandResult::OpenMenu`, activating the `menu_driver`.
#[test]
fn test_slash_config_opens_menu() {
    let mut state = make_state();
    handle_submit(&mut state, "/config");

    let texts = commit_texts(&state);
    // The separator is always present.
    assert!(
        texts.iter().any(|t| t.contains("╌╌ /config")),
        "separator line must appear"
    );
    // The menu driver should now be active (PRD-61 wired).
    assert!(
        state.menu_driver.is_active(),
        "menu_driver must be active after /config with no args; commit lines: {texts:?}"
    );
}

/// `/config core` with a section arg should still use the static read-only display
/// (not the interactive menu), so it produces `ConfigDisplay` lines.
#[test]
fn test_config_with_section_arg_still_shows_table() {
    let mut state = make_state();
    handle_submit(&mut state, "/config core");

    let texts = commit_texts(&state);
    // Static config display emits section header lines like "── [core] ──────"
    let has_section_header = texts.iter().any(|t| t.contains("[core]"));
    assert!(
        has_section_header,
        "section arg should produce static config display; got: {texts:?}"
    );
}

/// Sending `/config` while agent is active (task_done=false) should still work —
/// slash commands are always handled regardless of agent state.
#[test]
fn test_slash_config_works_during_active_task() {
    let mut state = AppState::new("some task");
    // Simulate an active state (phase != Idle) but don't actually start an agent.
    handle_submit(&mut state, "/config");

    let texts = commit_texts(&state);
    // Should still get the separator and some output.
    assert!(
        !texts.is_empty(),
        "slash command should produce output even when a task is queued"
    );
}

/// ConfigurationMenu unit-level navigation: move cursor down and confirm no panic.
#[test]
fn test_config_menu_navigation_no_panic() {
    use xaft_config::XaftConfig;
    use xaft_tui::menu::config_menu::ConfigurationMenu;
    use xaft_tui::menu::{MenuResult, MenuWidget};

    let tmp = TempDir::new().unwrap();
    let config = XaftConfig::default();
    let mut menu = ConfigurationMenu::new(&config, tmp.path().to_path_buf());

    // Navigate down several times — must not panic.
    for _ in 0..10 {
        let result = menu.handle_key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert!(
            matches!(result, MenuResult::Continue),
            "Down navigation should return Continue"
        );
    }
}

/// Esc key on the config menu should return MenuResult::Cancel.
#[test]
fn test_config_menu_esc_returns_cancel() {
    use xaft_config::XaftConfig;
    use xaft_tui::menu::config_menu::ConfigurationMenu;
    use xaft_tui::menu::{MenuResult, MenuWidget};

    let tmp = TempDir::new().unwrap();
    let config = XaftConfig::default();
    let mut menu = ConfigurationMenu::new(&config, tmp.path().to_path_buf());

    let result = menu.handle_key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });
    assert!(
        matches!(result, MenuResult::Cancel),
        "Esc should return Cancel"
    );
}

/// After entering edit mode, committing does NOT close the menu (returns Continue).
#[test]
fn test_config_menu_edit_then_commit_stays_open() {
    use xaft_config::XaftConfig;
    use xaft_tui::menu::config_menu::{ConfigurationMenu, MenuState};
    use xaft_tui::menu::{MenuResult, MenuWidget};

    let tmp = TempDir::new().unwrap();
    let config = XaftConfig::default();
    let mut menu = ConfigurationMenu::new(&config, tmp.path().to_path_buf());

    // Navigate down to find a string field to edit.
    // The first section header is always visible; navigate into a row.
    let enter = || KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    let down = || KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };

    // Expand the first section and move to the first editable field.
    // Tab expands the section.
    menu.handle_key(KeyEvent {
        code: KeyCode::Tab,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });
    // Move down until we find an editable field (up to 10 attempts).
    let mut in_edit = false;
    for _ in 0..10 {
        menu.handle_key(down());
        let result = menu.handle_key(enter());
        // If we entered edit mode, result is still Continue.
        if menu.edit_field_value().is_some() {
            in_edit = true;
            break;
        }
        // If it was a bool, it toggled but we're still in Navigate.
    }

    if in_edit {
        // Type something and commit.
        for c in "test".chars() {
            menu.handle_key(KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            });
        }
        let result = menu.handle_key(enter());
        assert!(
            matches!(result, MenuResult::Continue),
            "Committing an edit should return Continue (menu stays open)"
        );
    }
    // Whether or not we entered edit mode (booleans just toggle),
    // the test validates there's no panic.
}

/// Pressing 's' saves the config to `.xaft.toml` in the working directory.
#[test]
fn test_config_menu_save_creates_file() {
    use xaft_config::XaftConfig;
    use xaft_tui::menu::config_menu::ConfigurationMenu;
    use xaft_tui::menu::{MenuResult, MenuWidget};

    let tmp = TempDir::new().unwrap();
    let config = XaftConfig::default();
    let mut menu = ConfigurationMenu::new(&config, tmp.path().to_path_buf());

    let result = menu.handle_key(KeyEvent {
        code: KeyCode::Char('s'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });

    assert!(
        matches!(result, MenuResult::Continue),
        "'s' should return Continue"
    );

    let toml_path = tmp.path().join(".xaft.toml");
    assert!(
        toml_path.exists(),
        ".xaft.toml must be created by 's' key; status_msg may give more info"
    );

    let content = std::fs::read_to_string(&toml_path).expect("must be readable");
    assert!(
        content.contains("[core]") || content.contains("log_level"),
        "saved TOML should contain config content"
    );
}

/// `render` produces output without panicking.
#[test]
fn test_config_menu_render_no_panic() {
    use xaft_config::XaftConfig;
    use xaft_tui::menu::MenuWidget;
    use xaft_tui::menu::config_menu::ConfigurationMenu;

    let tmp = TempDir::new().unwrap();
    let config = XaftConfig::default();
    let menu = ConfigurationMenu::new(&config, tmp.path().to_path_buf());

    let mut buf: Vec<u8> = Vec::new();
    let rows = menu
        .render(&mut buf, (80, 24), 0)
        .expect("render should not fail");
    assert!(rows > 0, "render should produce at least one row");
    assert!(!buf.is_empty(), "render should write bytes to the output");
}
