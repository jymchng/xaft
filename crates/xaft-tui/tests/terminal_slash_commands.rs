//! `/ps`, `/stop` and the terminal overlay's `s` key through the live
//! `AppState` (parity rows 20 and 22, keybinding row 11).
//!
//! The load-bearing property is the SAME manager: the one `app.rs` hands to
//! the runtime is the one these commands read. These tests attach one manager,
//! start real background processes through it (as the agent's
//! `run_command {"background": true}` would), and prove the slash commands and
//! the overlay key list and stop exactly those processes.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tools::terminals::{TerminalManager, TerminalManagerConfig};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::menu::OverlayStyle;
use xaft_tui::prompt::build_prompt;
use xaft_tui::state::AppState;
use xaft_tui::transcript::RenderMutation;

fn key(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn submit(state: &mut AppState, text: &str) {
    state.input_bar.set_text(text);
    state.handle_event(key(KeyCode::Enter));
}

fn committed(state: &mut AppState) -> Vec<String> {
    state
        .mutations
        .drain(..)
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text),
            _ => None,
        })
        .collect()
}

fn manager(dir: &std::path::Path) -> Arc<TerminalManager> {
    let mut config = TerminalManagerConfig::new("sess-tui", dir);
    config.store_root = dir.join("store");
    config.cancel_grace_s = 1.0;
    Arc::new(TerminalManager::new(config))
}

async fn start_sleeper(m: &TerminalManager) -> String {
    let argv = vec!["/bin/sleep".to_string(), "30".to_string()];
    let out = m
        .start(
            None,
            Some(&argv),
            None,
            0.0,
            None,
            false,
            "sleep 30",
            "tc",
            "oneshot",
            None,
        )
        .await;
    assert_eq!(out["ok"], serde_json::json!(true), "{out}");
    out["terminal_id"].as_str().unwrap().to_string()
}

fn state_with(m: &Arc<TerminalManager>) -> AppState {
    let mut state = AppState::new("");
    state.attach_terminal_manager(Some(Arc::clone(m)));
    state
}

#[test]
fn without_a_manager_both_commands_say_management_is_unavailable() {
    let mut state = AppState::new("");
    for cmd in ["/ps", "/processes", "/stop", "/stop-terminal"] {
        submit(&mut state, cmd);
        let out = committed(&mut state);
        assert!(
            out.iter()
                .any(|l| l.contains("Background terminal management is unavailable.")),
            "{cmd}: {out:?}"
        );
    }
}

/// Keybinding row 11, end to end: `/ps` opens the overlay over the shared
/// manager, the overlay is drawn by the renderer, and `s` stops the terminal.
#[tokio::test(flavor = "multi_thread")]
async fn ps_opens_the_overlay_and_s_stops_the_selected_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let id = start_sleeper(&m).await;
    let mut state = state_with(&m);

    submit(&mut state, "/ps");
    assert!(state.menu_driver.is_active(), "/ps opens the overlay");
    let prompt = build_prompt(&mut state);
    let rows = prompt.overlay.expect("the prompt carries the overlay rows");
    assert!(
        rows[0].text.starts_with("Background Terminals · page 1/1"),
        "{rows:?}"
    );
    let selected = rows
        .iter()
        .find(|r| r.style == OverlayStyle::Selected)
        .expect("a selected row");
    assert!(selected.text.contains(&id) && selected.text.contains("running"));

    state.handle_event(key(KeyCode::Char('s')));
    let waited = m.wait(&id, 30.0).await;
    assert_eq!(waited["state"], serde_json::json!("stopped"), "{waited}");
    assert!(state.menu_driver.is_active(), "`s` keeps the overlay open");
    let rows = state.overlay_snapshot().unwrap();
    assert!(
        rows.iter()
            .any(|r| r.text == format!("Stop requested for {id}.")),
        "{rows:?}"
    );

    // The overlay owns the keyboard: nothing leaked into the input bar.
    assert!(state.input_bar.is_empty());
    state.handle_event(key(KeyCode::Esc));
    assert!(!state.menu_driver.is_active(), "Esc closes the overlay");
    assert!(build_prompt(&mut state).overlay.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_overlay_redraws_when_the_terminal_set_changes() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let mut state = state_with(&m);
    submit(&mut state, "/processes");
    assert!(state.menu_driver.is_active(), "/processes is /ps");
    let _ = build_prompt(&mut state);
    assert!(!state.refresh_overlay_if_changed(), "nothing changed yet");

    let id = start_sleeper(&m).await;
    assert!(
        state.refresh_overlay_if_changed(),
        "a new terminal changes the overlay, so a redraw is queued"
    );
    assert!(
        state
            .mutations
            .iter()
            .any(|m| matches!(m, RenderMutation::UpdatePrompt(p) if p.overlay.is_some()))
    );
    assert!(m.request_stop(&id, true));
    m.wait(&id, 30.0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ps_json_lists_the_shared_managers_records() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let id = start_sleeper(&m).await;
    let mut state = state_with(&m);
    submit(&mut state, "/ps --json");
    let out = committed(&mut state);
    let json_line = out
        .iter()
        .find(|l| l.trim_start().starts_with('['))
        .expect("a JSON line");
    let parsed: serde_json::Value = serde_json::from_str(json_line.trim()).unwrap();
    assert_eq!(parsed[0]["terminal_id"], serde_json::json!(id));
    assert!(
        !state.menu_driver.is_active(),
        "--json prints instead of opening"
    );
    assert!(m.request_stop(&id, true));
    m.wait(&id, 30.0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_and_its_alias_stop_the_shared_managers_terminals() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path());
    let first = start_sleeper(&m).await;
    let second = start_sleeper(&m).await;
    let mut state = state_with(&m);

    submit(&mut state, &format!("/stop-terminal {first}"));
    let out = committed(&mut state);
    assert!(
        out.iter()
            .any(|l| l.contains(&format!("Stop requested for {first}.")))
    );
    assert_eq!(
        m.wait(&first, 30.0).await["state"],
        serde_json::json!("stopped")
    );

    submit(&mut state, "/stop all");
    let out = committed(&mut state);
    assert!(
        out.iter()
            .any(|l| l.contains("Repeat as /stop all --confirm"))
    );
    assert!(
        m.owns_live(&second),
        "an unconfirmed /stop all stops nothing"
    );

    submit(&mut state, "/stop all --confirm");
    let out = committed(&mut state);
    assert!(
        out.iter()
            .any(|l| l.contains("Stop requested for 1 background terminal(s).")),
        "{out:?}"
    );
    assert_eq!(
        m.wait(&second, 30.0).await["state"],
        serde_json::json!("stopped")
    );
}
