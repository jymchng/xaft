//! End-to-end checks that the new agenthicc-parity slash commands are wired
//! into the live TUI, not just unit-testable in isolation.
//!
//! The unit tests in `slash/commands/*` prove each handler's output. These
//! prove the two things that unit tests cannot: that `AppState` dispatches
//! the new commands, and that `/cancel` actually reaches the same atomic the
//! run loop drains (a handler that raises a flag nobody reads would pass
//! every unit test and cancel nothing).

use std::sync::atomic::Ordering;

use xaft_tui::slash::parser::SlashCommandParser;
use xaft_tui::slash::{CommandResult, SlashCommand};
use xaft_tui::state::AppState;

fn lines_of(result: CommandResult) -> Vec<String> {
    match result {
        CommandResult::Lines(v) => v,
        other => panic!("expected CommandResult::Lines, got {other:?}"),
    }
}

/// Dispatch a slash command through AppState exactly as the input loop does.
fn run(state: &mut AppState, input: &str) -> CommandResult {
    let command = match SlashCommandParser::parse(input) {
        Some(Ok(c)) => c,
        Some(Err(e)) => panic!("{input:?} failed to parse: {e}"),
        None => panic!("{input:?} did not parse as a slash command"),
    };
    // Derive args the way AppState does: everything after the trigger word.
    let after = input.trim_start().trim_start_matches('/');
    let args = match after.find(char::is_whitespace) {
        Some(i) => after[i + 1..].trim().to_string(),
        None => String::new(),
    };
    let handler = state.slash_registry.get(&command);
    handler.execute(state.build_command_context(args, command))
}

#[test]
fn app_state_dispatches_status() {
    let mut state = AppState::new("");
    let out = lines_of(run(&mut state, "/status"));
    assert!(
        out.iter().any(|l| l.starts_with("Session:")),
        "/status must report the session: {out:?}"
    );
    assert!(
        out.iter().any(|l| l.starts_with("Model:")),
        "/status must report the model: {out:?}"
    );
}

#[test]
fn app_state_dispatches_commands_and_the_new_rows_are_listed() {
    let mut state = AppState::new("");
    let out = lines_of(run(&mut state, "/commands"));
    let text = out.join("\n");
    for expected in [
        "/cancel",
        "/commands",
        "/expand",
        "/history",
        "/models",
        "/status",
        "/tools",
    ] {
        assert!(
            text.contains(expected),
            "/commands omits {expected}:\n{text}"
        );
    }
}

#[test]
fn app_state_dispatches_models() {
    let mut state = AppState::new("");
    let out = lines_of(run(&mut state, "/models"));
    assert!(
        out[0].contains("LLM Providers"),
        "expected a provider table, got {out:?}"
    );
}

#[test]
fn interrupt_is_an_alias_for_cancel() {
    assert_eq!(
        SlashCommandParser::parse("/cancel"),
        Some(Ok(SlashCommand::Cancel))
    );
    assert_eq!(
        SlashCommandParser::parse("/interrupt"),
        Some(Ok(SlashCommand::Cancel))
    );
}

/// The load-bearing test: the flag `/cancel` raises must be the same atomic
/// the run loop drains on `AppState`.
#[test]
fn cancel_reaches_the_state_flag_the_run_loop_drains() {
    let mut state = AppState::new("");

    // Nothing running -> no cancellation, and the flag stays clear.
    let out = lines_of(run(&mut state, "/cancel"));
    assert_eq!(out, vec!["No active intent to cancel.".to_string()]);
    assert!(
        !state.cancel_active_intent.load(Ordering::SeqCst),
        "nothing was running, so nothing should be queued for cancellation"
    );

    // Simulate the run loop's `intent_running = true`, then cancel.
    state.intent_running.store(true, Ordering::SeqCst);
    let out = lines_of(run(&mut state, "/interrupt"));
    assert_eq!(
        out,
        vec!["Cancellation requested for the active run.".to_string()],
        "/interrupt must behave exactly like /cancel"
    );
    assert!(
        state.cancel_active_intent.load(Ordering::SeqCst),
        "/cancel must raise the flag on the shared AppState atomic"
    );

    // The run loop drains it (swap(false)) and stops the intent.
    assert!(state.cancel_active_intent.swap(false, Ordering::SeqCst));
    assert!(!state.cancel_active_intent.load(Ordering::SeqCst));
    state.intent_running.store(false, Ordering::SeqCst);
}

#[test]
fn history_reads_the_states_shared_store() {
    let mut state = AppState::new("");
    // The `#` trigger and `/history` must see the same store, otherwise
    // `/history` would list nothing while `#` recalls entries.
    let out = lines_of(run(&mut state, "/history"));
    assert_eq!(out, vec!["No input history yet.".to_string()]);

    state.input_history.write().expect("lock").push(
        "do the thing".into(),
        xaft_tui::trigger::history::HistoryKind::AgentTask,
    );

    let out = lines_of(run(&mut state, "/history"));
    assert!(
        out.iter().any(|l| l.contains("do the thing")),
        "/history must see entries the # trigger recorded: {out:?}"
    );
}

/// Every agenthicc command the parity matrix marks ✅ must actually resolve.
///
/// This is the test that would have caught `/usage`. The matrix called row 23
/// done because `/usage` is listed in `CommandMeta::aliases` for `/cost` — but
/// the parser resolves a trigger by table KEY, not by scanning the alias list,
/// and `usage` had no key of its own. So `/usage` answered "Unknown command"
/// while the matrix, `/help` and `/commands` all advertised it.
///
/// The list is agenthicc's own names, not xaft's, so it fails if a command is
/// ever renamed away from the spelling agenthicc uses.
#[test]
fn every_agenthicc_command_the_matrix_marks_done_actually_parses() {
    // agenthicc commands listed ✅ in section 2 of docs/parity-matrix.md, each
    // with the minimum argument it needs. `/model` legitimately rejects a
    // missing name — that is argument validation, not an unknown command, so
    // the test supplies a name rather than conflating the two.
    const MATRIX_DONE: &[(&str, &str)] = &[
        ("cancel", ""),
        ("clear", ""),
        ("commands", ""),
        ("tools", ""),
        ("config", ""),
        ("expand", ""),
        ("help", ""),
        ("history", ""),
        ("init", ""),
        ("mcp", ""),
        ("model", "some-model"),
        ("models", ""),
        ("status", ""),
        ("usage", ""),
        ("mode", ""),
        ("compact", ""),
        ("replay", ""),
    ];
    let state = AppState::new("");
    let mut broken = Vec::new();
    for (name, args) in MATRIX_DONE {
        let input = if args.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {args}")
        };
        match SlashCommandParser::parse(&input) {
            Some(Ok(cmd)) => {
                // `get` panics when a command has no registered handler, which
                // is the other half of "the row is really done".
                let handler = state.slash_registry.get(&cmd);
                assert!(
                    !handler.description().is_empty(),
                    "/{name} has an empty description"
                );
            }
            Some(Err(e)) => broken.push(format!("/{name}: {e}")),
            None => broken.push(format!("/{name}: not recognised as a command")),
        }
    }
    assert!(
        broken.is_empty(),
        "the matrix marks these ✅ but they do not dispatch:\n  {}",
        broken.join("\n  ")
    );
}

/// The same list, run through the full dispatch path used by the input loop.
///
/// `every_agenthicc_command_the_matrix_marks_done_actually_parses` proves the
/// parser and registry agree. This proves the command survives the round trip
/// through `AppState` as well, which is where the original `/status` and
/// `/tools` defect lived.
#[test]
fn matrix_done_commands_survive_full_dispatch() {
    // Commands that are safe to execute headlessly with no session, store or
    // terminal attached. The rest are covered by the parse+registry test above
    // or by their own dedicated integration tests.
    const DISPATCHABLE: &[&str] = &[
        "commands", "help", "history", "models", "status", "tools", "usage", "cancel",
    ];
    let mut state = AppState::new("");
    for name in DISPATCHABLE {
        let out = run(&mut state, &format!("/{name}"));
        let text = match &out {
            CommandResult::Lines(v) => v.join("\n"),
            CommandResult::Error(e) => e.clone(),
            other => panic!("/{name} produced an unexpected result: {other:?}"),
        };
        assert!(
            !text.contains("Unknown command"),
            "/{name} answered 'Unknown command': {text}"
        );
        assert!(
            !text.is_empty(),
            "/{name} produced no output at all — a silently empty command \
             looks implemented but does nothing"
        );
    }
}
