//! End-to-end tests for mode system integration with AppState (PRD 64/65).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::AppState;

fn backtab_event() -> KeyEvent {
    KeyEvent {
        code: KeyCode::BackTab,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn test_shift_tab_cycles_mode() {
    let mut state = AppState::new_for_test();
    let initial = state.mode_manager.active_name().to_string();
    state.handle_event(TuiEvent::Key(backtab_event()));
    let after = state.mode_manager.active_name().to_string();
    assert_ne!(initial, after, "mode should change after BackTab");
}

#[test]
fn test_plan_mode_filters_run_request() {
    use std::path::PathBuf;

    let mut state = AppState::new_for_test();
    state.mode_manager.set("plan").unwrap();

    let mut req = xaft_runtime::RunRequest {
        task: "test".into(),
        config: xaft_config::XaftConfig::default(),
        working_dir: PathBuf::from("."),
        headless: true,
        dry_run: true,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: xaft_runtime::WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };

    state.mode_manager.apply_to_run_request(&mut req);

    assert!(
        req.mode_system_patch.is_some(),
        "plan mode must set system patch"
    );
    assert!(
        req.mode_tool_filter.is_some(),
        "plan mode must set tool filter"
    );

    let filter = req.mode_tool_filter.unwrap();
    assert!(filter("read_file"));
    assert!(!filter("write_file"));
}

#[test]
fn test_mode_notification_set_on_cycle() {
    let mut state = AppState::new_for_test();
    assert!(
        state.mode_notification.is_none(),
        "no notification initially"
    );
    state.handle_event(TuiEvent::Key(backtab_event()));
    // The notification is consumed by push_prompt_update() during the BackTab handler.
    // Verify mode changed instead — mode should now be "plan" (second mode).
    assert_ne!(
        state.mode_manager.active().name,
        "auto",
        "mode should have changed after BackTab"
    );
    // The UpdatePrompt mutation should contain the switch notification in mode_footer.
    let has_switch_notif = state.mutations.iter().any(|m| {
        if let xaft_tui::transcript::RenderMutation::UpdatePrompt(p) = m {
            p.mode_footer.contains("Switched")
        } else {
            false
        }
    });
    assert!(
        has_switch_notif,
        "UpdatePrompt mutation should have switch notification in mode_footer"
    );
}
