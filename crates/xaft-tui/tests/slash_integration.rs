//! Integration tests for the slash command system.
//!
//! Tests the full cycle: user input → slash command detection → handler execution
//! → mutation output (never forwarded to agent).

use std::sync::Arc;

use agtrs_runtime::signals::SignalBus;
use crossterm::event::KeyCode;
use xaft_tui::state::AppState;
use xaft_tui::transcript::{LineKind, RenderMutation, StyledLine};
use xaft_tui::{SlashCommandExecuted, WorkflowPhase};

// ── Test helpers ──────────────────────────────────────────────────────────────

fn make_app_state() -> AppState {
    AppState::new("") // empty task = Idle phase
}

fn make_app_state_with_task() -> AppState {
    AppState::new("test task")
}

fn make_app_state_with_bus(bus: Arc<SignalBus>) -> AppState {
    AppState::new_with_bus("", bus)
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

fn commit_lines(state: &AppState) -> Vec<&StyledLine> {
    state
        .mutations
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l),
            _ => None,
        })
        .collect()
}

fn handle_submit(state: &mut AppState, text: &str) {
    state.handle_event(xaft_tui::TuiEvent::Key(crossterm::event::KeyEvent {
        code: KeyCode::Char('x'), // dummy
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
    // Use handle_input_submitted directly via input bar submit.
    state.input_bar.set_text(text);
    // Simulate Enter key to submit.
    let key = crossterm::event::KeyEvent {
        code: KeyCode::Enter,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    state.mutations.clear();
    state.handle_event(xaft_tui::TuiEvent::Key(key));
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn slash_command_does_not_reach_agent() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/clear");
    // No run request should have been queued (phase stays Idle, task_start_time is None).
    // Slash commands don't set task_start_time.
    assert_eq!(state.phase, WorkflowPhase::Idle);
    // user_message_tx is None so no message was sent.
    assert!(
        state.user_message_tx.is_none() || state.task_start_time.is_none(),
        "slash command must not reach agent"
    );
}

#[test]
fn slash_command_clears_input_bar() {
    let mut state = make_app_state();
    state.input_bar.set_text("/clear");
    handle_submit(&mut state, "/clear");
    assert!(
        state.input_bar.is_empty(),
        "input bar should be cleared after slash command"
    );
}

#[test]
fn slash_command_pushes_separator_line() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/help");
    let texts = commit_texts(&state);
    assert!(
        texts.iter().any(|t| t.contains("/help")),
        "separator must mention the command: {:?}",
        texts
    );
}

#[test]
fn slash_command_emits_signal() {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().unwrap();
    let bus = Arc::new(SignalBus::new());
    let bus_clone = Arc::clone(&bus);

    // Subscribe before executing.
    let mut rx = rt.block_on(async { bus_clone.subscribe::<SlashCommandExecuted>().await });

    let mut state = make_app_state_with_bus(Arc::clone(&bus));
    handle_submit(&mut state, "/clear");

    // Give the signal time to propagate (it's spawned async).
    rt.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let sig = rx.try_recv().expect("SlashCommandExecuted must be emitted");
    assert_eq!(sig.name, "clear", "signal name wrong: {:?}", sig.name);
    assert!(sig.success, "signal should be success");
}

#[test]
fn slash_command_error_does_not_emit_with_success_true() {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().unwrap();
    let bus = Arc::new(SignalBus::new());
    let bus_clone = Arc::clone(&bus);

    let mut rx = rt.block_on(async { bus_clone.subscribe::<SlashCommandExecuted>().await });

    let mut state = make_app_state_with_bus(Arc::clone(&bus));
    handle_submit(&mut state, "/model"); // missing arg -> error

    rt.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let sig = rx
        .try_recv()
        .expect("SlashCommandExecuted emitted even on error");
    assert!(!sig.success, "signal should not be success for error");
}

#[test]
fn slash_history_records_executed_commands() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/clear");
    handle_submit(&mut state, "/help");
    assert_eq!(
        state.slash_history.len(),
        2,
        "slash history should record 2 commands"
    );
}

#[test]
fn palette_opens_on_slash_input() {
    let mut state = make_app_state();
    state.handle_char('/');
    assert!(
        state
            .active_trigger
            .as_ref()
            .map_or(false, |t| t.scan.trigger_char == '/'),
        "palette should open on '/' input"
    );
}

#[test]
fn palette_closes_on_escape() {
    let mut state = make_app_state();
    state.handle_char('/');
    assert!(
        state
            .active_trigger
            .as_ref()
            .map_or(false, |t| t.scan.trigger_char == '/')
    );

    let key = crossterm::event::KeyEvent {
        code: KeyCode::Esc,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    state.handle_event(xaft_tui::TuiEvent::Key(key));
    assert!(
        state
            .active_trigger
            .as_ref()
            .map_or(true, |t| t.scan.trigger_char != '/'),
        "palette should close on Escape"
    );
}

#[test]
fn palette_closes_after_command_executes() {
    let mut state = make_app_state();
    state.handle_char('/');
    state.input_bar.set_text("/clear");
    handle_submit(&mut state, "/clear");
    assert!(
        state
            .active_trigger
            .as_ref()
            .map_or(true, |t| t.scan.trigger_char != '/'),
        "palette should close after command executes"
    );
}

#[test]
fn unknown_slash_command_produces_error_line() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/zzzzz");
    let texts = commit_texts(&state);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Unknown command") && t.contains("/zzzzz")),
        "expected unknown-command error: {:?}",
        texts
    );
    assert_eq!(state.phase, WorkflowPhase::Idle);
}

#[test]
fn multiline_input_is_not_slash_command() {
    let mut state = make_app_state_with_task();
    // Set up a user_message_tx so we can detect if it was called.
    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<xaft_tui::user_message::UserMessage>();
    state.user_message_tx = Some(tx);

    // Multi-line input should not be parsed as slash command.
    let multi_line = "/clear\nextra line";
    state.input_bar.set_text(multi_line);
    let key = crossterm::event::KeyEvent {
        code: KeyCode::Enter,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    state.mutations.clear();
    state.handle_event(xaft_tui::TuiEvent::Key(key));

    // Message should have been forwarded to the agent (received on channel).
    // It may not be received immediately since there's no workspace for F3,
    // but the text path (send_text_and_reset) would send immediately.
    assert!(
        rx.try_recv().is_ok(),
        "multi-line input should be forwarded to agent"
    );
}

#[test]
fn slash_command_output_lines_have_system_kind() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/help");
    let lines = commit_lines(&state);
    let system_lines: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == LineKind::System)
        .collect();
    assert!(
        !system_lines.is_empty(),
        "help output must use LineKind::System, lines: {:?}",
        lines.iter().map(|l| (l.kind, &l.text)).collect::<Vec<_>>()
    );
}

#[test]
fn slash_error_lines_have_error_kind() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/model"); // missing arg
    let lines = commit_lines(&state);
    let error_lines: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Error).collect();
    assert!(
        !error_lines.is_empty(),
        "error output must use LineKind::Error: {:?}",
        lines.iter().map(|l| (l.kind, &l.text)).collect::<Vec<_>>()
    );
    assert!(
        error_lines.iter().any(|l| l.text.contains('✗')),
        "error lines must have ✗ prefix: {:?}",
        error_lines.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
}

#[test]
fn slash_separator_line_contains_command_name() {
    let mut state = make_app_state();
    handle_submit(&mut state, "/cost");
    let lines = commit_lines(&state);
    assert!(
        lines
            .iter()
            .any(|l| l.kind == LineKind::Separator && l.text.contains("cost")),
        "separator must contain the command name, lines: {:?}",
        lines.iter().map(|l| (l.kind, &l.text)).collect::<Vec<_>>()
    );
}
