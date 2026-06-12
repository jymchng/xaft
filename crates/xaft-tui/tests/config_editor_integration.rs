//! Integration tests for `/config` display (PRD 55 — replaces PRD 34 Feature B).
//!
//! The config handler now produces a read-only, section-grouped `ConfigDisplay`
//! result that is rendered as committed `StyledLine`s.  There is no interactive
//! navigation state — committed lines are append-only and cannot be navigated.

use xaft_tui::bridge::TuiEvent;
use xaft_tui::slash::{CommandResult, ConfigLayer, ConfigRow, ConfigSection, ConfigValueKind};
use xaft_tui::state::AppState;
use xaft_tui::transcript::RenderMutation;

fn make_section(name: &str, rows: Vec<ConfigRow>) -> ConfigSection {
    ConfigSection {
        name: name.to_string(),
        rows,
    }
}

fn make_row(
    key: &str,
    val: &str,
    kind: ConfigValueKind,
    layer: ConfigLayer,
    overridden: bool,
) -> ConfigRow {
    ConfigRow {
        key: key.to_string(),
        display_value: val.to_string(),
        value_kind: kind,
        source_layer: layer,
        is_overridden: overridden,
    }
}

fn commit_texts(s: &AppState) -> Vec<String> {
    s.mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn config_display_produces_section_header_and_rows() {
    let mut s = AppState::new("");
    let sections = vec![make_section(
        "core",
        vec![
            make_row(
                "log_level",
                "\"info\"",
                ConfigValueKind::Str,
                ConfigLayer::Default,
                false,
            ),
            make_row(
                "telemetry",
                "true",
                ConfigValueKind::Bool,
                ConfigLayer::Default,
                false,
            ),
        ],
    )];
    s.apply_command_result(&CommandResult::ConfigDisplay(sections));
    let texts = commit_texts(&s);
    assert!(texts.iter().any(|t| t.contains("[core]")), "header missing");
    assert!(texts.iter().any(|t| t.contains("log_level")));
    assert!(texts.iter().any(|t| t.contains("telemetry")));
}

#[test]
fn overridden_value_gets_asterisk_prefix() {
    let mut s = AppState::new("");
    let sections = vec![make_section(
        "tui",
        vec![
            make_row(
                "theme",
                "\"light\"",
                ConfigValueKind::Str,
                ConfigLayer::Project,
                true,
            ),
            make_row(
                "mouse",
                "false",
                ConfigValueKind::Bool,
                ConfigLayer::Default,
                false,
            ),
        ],
    )];
    s.apply_command_result(&CommandResult::ConfigDisplay(sections));
    let texts = commit_texts(&s);
    let theme_line = texts.iter().find(|t| t.contains("theme")).unwrap();
    assert!(
        theme_line.contains('*'),
        "overridden must have *: {theme_line:?}"
    );
    let mouse_line = texts.iter().find(|t| t.contains("mouse")).unwrap();
    assert!(
        !mouse_line.trim_start().starts_with('*'),
        "default must not have *: {mouse_line:?}"
    );
}

#[test]
fn footer_hint_always_appears() {
    let mut s = AppState::new("");
    s.apply_command_result(&CommandResult::ConfigDisplay(vec![make_section(
        "core",
        vec![make_row(
            "log_level",
            "\"info\"",
            ConfigValueKind::Str,
            ConfigLayer::Default,
            false,
        )],
    )]));
    let texts = commit_texts(&s);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("* = overridden") && t.contains("/config set")),
        "footer hint missing"
    );
}

#[test]
fn up_down_keys_do_not_enter_navigation_mode() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let mut s = AppState::new("");
    s.apply_command_result(&CommandResult::ConfigDisplay(vec![make_section(
        "core",
        vec![make_row(
            "log_level",
            "\"info\"",
            ConfigValueKind::Str,
            ConfigLayer::Default,
            false,
        )],
    )]));
    s.mutations.clear();
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter, KeyCode::Esc] {
        s.handle_event(TuiEvent::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }));
    }
    assert!(!s.should_quit);
}

#[test]
fn source_layer_appears_in_row() {
    let mut s = AppState::new("");
    s.apply_command_result(&CommandResult::ConfigDisplay(vec![make_section(
        "core",
        vec![make_row(
            "log_level",
            "\"warn\"",
            ConfigValueKind::Str,
            ConfigLayer::Project,
            true,
        )],
    )]));
    let texts = commit_texts(&s);
    let row = texts.iter().find(|t| t.contains("log_level")).unwrap();
    assert!(row.contains("project"), "source layer missing: {row:?}");
}

#[test]
fn long_display_value_truncated() {
    let mut s = AppState::new("");
    let long_val = "\"".to_string() + &"x".repeat(60) + "\"";
    s.apply_command_result(&CommandResult::ConfigDisplay(vec![make_section(
        "core",
        vec![make_row(
            "data_dir",
            &long_val,
            ConfigValueKind::Str,
            ConfigLayer::Project,
            true,
        )],
    )]));
    let texts = commit_texts(&s);
    let row = texts.iter().find(|t| t.contains("data_dir")).unwrap();
    assert!(row.contains('…'), "long value must be truncated: {row:?}");
}

#[test]
fn multiple_sections_all_appear() {
    let mut s = AppState::new("");
    s.apply_command_result(&CommandResult::ConfigDisplay(vec![
        make_section(
            "core",
            vec![make_row(
                "log_level",
                "\"info\"",
                ConfigValueKind::Str,
                ConfigLayer::Default,
                false,
            )],
        ),
        make_section(
            "tui",
            vec![make_row(
                "theme",
                "\"dark\"",
                ConfigValueKind::Str,
                ConfigLayer::Default,
                false,
            )],
        ),
    ]));
    let texts = commit_texts(&s);
    assert!(texts.iter().any(|t| t.contains("[core]")));
    assert!(texts.iter().any(|t| t.contains("[tui]")));
}

#[test]
fn config_handler_produces_config_display() {
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::Arc;
    use xaft_config::XaftConfig;
    use xaft_tui::slash::commands::config::ConfigHandler;
    use xaft_tui::slash::registry::SlashHandler;
    use xaft_tui::slash::{AgentStatsMap, CommandContext};

    let handler = ConfigHandler::new(Arc::new(XaftConfig::default()));
    let ctx = CommandContext {
        args: String::new(),
        command: xaft_tui::slash::SlashCommand::Config,
        signals: Arc::new(SignalBus::new()),
        config: Arc::new(XaftConfig::default()),
        session_id: None,
        working_dir: PathBuf::from("."),
        terminal_cols: 80,
        llm_stats: Arc::new(std::sync::RwLock::new(AgentStatsMap::new())),
        conversation_store: None,
        session_store: None,
    };
    assert!(
        matches!(handler.execute(ctx), CommandResult::ConfigDisplay(_)),
        "handler must return ConfigDisplay, not ConfigEditor"
    );
}
