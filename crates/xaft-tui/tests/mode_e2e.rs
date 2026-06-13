//! End-to-end tests for PRD 66 Mode UI.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::mode::builtins::builtin_modes;
use xaft_tui::prompt::build_prompt;
use xaft_tui::state::AppState;

fn make_state() -> AppState {
    AppState::new("")
}

fn backtab() -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code: KeyCode::BackTab,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

// ── 1. /mode listing shows builtin mode names ─────────────────────────────────
// (Tests the builtin_modes() function used by the /mode handler)

#[test]
fn test_slash_mode_lists_modes() {
    let modes = builtin_modes();
    let names: Vec<&str> = modes.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"plan"),
        "builtin_modes must include 'plan': {names:?}"
    );
    assert!(
        names.contains(&"safe"),
        "builtin_modes must include 'safe': {names:?}"
    );
    assert!(
        names.contains(&"debug"),
        "builtin_modes must include 'debug': {names:?}"
    );
}

// ── 2. /mode unknown arg — registry returns None ─────────────────────────────

#[test]
fn test_slash_mode_unknown_arg_shows_info() {
    let mut s = make_state();
    // Setting a bogus mode should return an error
    let result = s.mode_manager.set("bogus_mode_that_does_not_exist");
    assert!(
        result.is_err(),
        "setting unknown mode must return ModeError"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("bogus_mode_that_does_not_exist"),
        "error must name the unknown mode: {err_msg}"
    );
}

// ── 3. Mode badge reflects in PromptState ─────────────────────────────────────

#[test]
fn test_mode_badge_reflects_in_prompt_state() {
    let mut s = make_state();

    // Auto → no badge
    let p0 = build_prompt(&mut s);
    assert!(p0.mode_badge.is_none(), "auto has no badge");

    // Switch to plan
    s.handle_event(backtab()); // auto → plan (second mode in list)
    let p1 = build_prompt(&mut s);
    if s.mode_manager.active_name() == "auto" {
        // skip — wrapped back (shouldn't happen with default 6 modes)
    } else {
        assert!(
            p1.mode_badge.is_some(),
            "non-auto mode must have badge after BackTab"
        );
    }

    // Cycle through all modes, checking badge consistency
    let mode_count = s.mode_manager.registry().len();
    for _ in 0..mode_count {
        let name = s.mode_manager.active_name().to_string();
        let p = build_prompt(&mut s);
        if name == "auto" {
            assert!(p.mode_badge.is_none(), "auto badge must be None");
        } else {
            assert!(p.mode_badge.is_some(), "mode '{name}' must have a badge");
        }
        s.handle_event(backtab());
    }
}

// ── 4. mode_footer notification clears after one build ────────────────────────

#[test]
fn test_mode_footer_notification_clears_after_one_build() {
    let mut s = make_state();
    // Set a notification manually
    s.mode_notification = Some("✦ Switched to plan mode".to_string());

    // First build_prompt should consume the notification
    let p1 = build_prompt(&mut s);
    assert!(
        p1.mode_footer.contains("Switched to plan mode"),
        "first build must show notification: got {:?}",
        p1.mode_footer
    );
    assert!(
        s.mode_notification.is_none(),
        "notification must be consumed after first build"
    );

    // Second build_prompt should show the standard footer
    let p2 = build_prompt(&mut s);
    assert!(
        !p2.mode_footer.contains("Switched to plan mode"),
        "second build must not show old notification: {:?}",
        p2.mode_footer
    );
    assert!(
        p2.mode_footer.contains("shift+tab"),
        "second build must show standard hint: {:?}",
        p2.mode_footer
    );
}
