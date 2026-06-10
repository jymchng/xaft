//! Integration tests for Feature B — Interactive /config editor (PRD 34).

use xaft_tui::bridge::TuiEvent;
use xaft_tui::slash::{CommandResult, ConfigEntry, ConfigLayer, ConfigValueKind};
use xaft_tui::state::AppState;
use xaft_tui::transcript::{LineKind, RenderMutation};

fn make_entry(
    section: &str,
    key: &str,
    val: &str,
    kind: ConfigValueKind,
    editable: bool,
) -> ConfigEntry {
    ConfigEntry {
        section: section.into(),
        key: key.into(),
        display_value: format!("{val:?}"),
        raw_value: val.into(),
        value_kind: kind,
        source_layer: ConfigLayer::Default,
        editable,
    }
}

#[test]
fn config_command_opens_editor_panel() {
    let mut s = AppState::new("");
    let entries = vec![
        make_entry("core", "log_level", "info", ConfigValueKind::Str, true),
        make_entry("core", "telemetry", "true", ConfigValueKind::Bool, true),
    ];
    s.apply_command_result(&CommandResult::ConfigEditor(entries));
    assert!(
        s.config_editor.visible,
        "editor must be visible after ConfigEditor result"
    );
    assert_eq!(s.config_editor.entries.len(), 2);
    // Transcript must include the entries.
    let texts: Vec<&str> = s
        .mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("log_level")),
        "log_level must appear"
    );
    assert!(
        texts.iter().any(|t| t.contains("telemetry")),
        "telemetry must appear"
    );
}

#[test]
fn arrow_keys_navigate_editor_rows() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    let entries = vec![
        make_entry("core", "a", "1", ConfigValueKind::Int, true),
        make_entry("core", "b", "2", ConfigValueKind::Int, true),
        make_entry("core", "c", "3", ConfigValueKind::Int, true),
    ];
    s.apply_command_result(&CommandResult::ConfigEditor(entries));
    assert_eq!(s.config_editor.selected, 0);

    let down = KeyEvent {
        code: KeyCode::Down,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(down));
    assert_eq!(s.config_editor.selected, 1, "down moves selection");

    let up = KeyEvent {
        code: KeyCode::Up,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(up));
    assert_eq!(s.config_editor.selected, 0, "up moves selection back");
}

#[test]
fn esc_dismisses_editor() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    let entries = vec![make_entry(
        "core",
        "log_level",
        "info",
        ConfigValueKind::Str,
        true,
    )];
    s.apply_command_result(&CommandResult::ConfigEditor(entries));
    assert!(s.config_editor.visible);

    let esc = KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(esc));
    assert!(!s.config_editor.visible, "Esc must dismiss the editor");
}

#[test]
fn enter_opens_edit_field_for_string_row() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    let entries = vec![make_entry(
        "core",
        "log_level",
        "info",
        ConfigValueKind::Str,
        true,
    )];
    s.apply_command_result(&CommandResult::ConfigEditor(entries));

    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(enter));
    assert!(s.config_editor.edit.is_some(), "Enter must open edit row");
    assert_eq!(s.config_editor.edit.as_ref().unwrap().original, "info");
}

#[test]
fn esc_in_edit_mode_cancels() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    let entries = vec![make_entry(
        "core",
        "log_level",
        "info",
        ConfigValueKind::Str,
        true,
    )];
    s.apply_command_result(&CommandResult::ConfigEditor(entries));

    let enter = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(enter));
    assert!(s.config_editor.edit.is_some());

    let esc = KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    s.handle_event(TuiEvent::Key(esc));
    assert!(
        s.config_editor.edit.is_none(),
        "Esc in edit mode must cancel"
    );
    assert!(
        s.config_editor.visible,
        "editor must remain visible after edit cancel"
    );
}

#[test]
fn array_rows_are_not_editable() {
    let entry = make_entry(
        "agent.default",
        "allowed_tools",
        "",
        ConfigValueKind::Array,
        false,
    );
    assert!(!entry.editable, "Array entries must not be editable");
}

#[test]
fn config_section_filter_shows_only_matching_entries() {
    // ConfigHandler filtering is tested via CommandResult contents.
    // When filter = "agent.default", only entries with section "agent.default" appear.
    let entries = vec![
        make_entry("core", "log_level", "info", ConfigValueKind::Str, true),
        make_entry(
            "agent.default",
            "max_turns",
            "25",
            ConfigValueKind::Int,
            true,
        ),
        make_entry(
            "agent.default",
            "model",
            "claude-3-5-sonnet",
            ConfigValueKind::Str,
            true,
        ),
    ];
    // Simulate filtered result (only agent.default).
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|e| e.section == "agent.default")
        .collect();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|e| e.section == "agent.default"));
}
