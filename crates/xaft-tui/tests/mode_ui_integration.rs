//! Integration tests for PRD 66 Mode UI.
//!
//! Tests cover Shift+Tab cycling, mode_notification, prompt badge/footer.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use xaft_tui::bridge::TuiEvent;
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

// ── 1. Shift+Tab cycles mode ──────────────────────────────────────────────────

#[test]
fn test_shift_tab_cycles_mode() {
    let mut s = make_state();
    // Initial mode is "auto"
    assert_eq!(s.mode_manager.active_name(), "auto");
    // Send BackTab
    s.handle_event(backtab());
    // Mode should change away from "auto"
    let new_mode = s.mode_manager.active_name().to_string();
    assert_ne!(
        new_mode, "auto",
        "Shift+Tab must cycle away from auto: got {new_mode}"
    );
}

// ── 2. Shift+Tab wraps after all modes ────────────────────────────────────────

#[test]
fn test_shift_tab_wraps_after_all_modes() {
    let mut s = make_state();
    let mode_count = s.mode_manager.registry().len();
    assert!(mode_count >= 2, "need at least 2 modes to wrap");

    // Cycle through all modes
    for _ in 0..mode_count {
        s.handle_event(backtab());
    }

    // Should be back to "auto" (or wherever we started — the index wraps)
    assert_eq!(
        s.mode_manager.active_name(),
        "auto",
        "after cycling all {} modes must be back at auto",
        mode_count
    );
}

// ── 3. mode_notification is consumed by UpdatePrompt mutation ────────────────
// The BackTab handler sets mode_notification then immediately calls
// push_prompt_update() which builds the prompt (consuming the notification).
// So after handle_event returns, mode_notification is None — but the
// UpdatePrompt mutation carries the notification text in mode_footer.

#[test]
fn test_mode_notification_set_on_shift_tab() {
    let mut s = make_state();
    assert!(
        s.mode_notification.is_none(),
        "no notification before first cycle"
    );
    s.handle_event(backtab());
    // Notification is consumed immediately by push_prompt_update() inside the handler.
    // Verify the UpdatePrompt mutation carries a footer with the switch message.
    let update = s.mutations.iter().find_map(|m| {
        if let xaft_tui::transcript::RenderMutation::UpdatePrompt(p) = m {
            Some(p.clone())
        } else {
            None
        }
    });
    let prompt = update.expect("BackTab must produce UpdatePrompt mutation");
    assert!(
        prompt.mode_footer.contains("Switched to") || prompt.mode_footer.contains("shift+tab"),
        "mode_footer must contain switch message or hint: {:?}",
        prompt.mode_footer
    );
}

// ── 4. mode_footer changes in PromptState ────────────────────────────────────

#[test]
fn test_mode_footer_changes_in_prompt() {
    let mut s = make_state();
    // Switch to plan mode directly to get a stable mode name
    s.mode_manager.set("plan").expect("plan mode must exist");
    // build_prompt now (no notification pending) — should show standing hint
    let p = build_prompt(&mut s);
    assert!(
        p.mode_footer.contains("plan"),
        "mode_footer must contain mode name 'plan': got {:?}",
        p.mode_footer
    );
    assert!(
        p.mode_footer.contains("shift+tab") || p.mode_footer.contains("Plan"),
        "mode_footer must show plan mode hint: got {:?}",
        p.mode_footer
    );
}

// ── 5. Auto mode has no badge ─────────────────────────────────────────────────

#[test]
fn test_auto_mode_has_no_badge() {
    let mut s = make_state();
    assert_eq!(s.mode_manager.active_name(), "auto");
    let p = build_prompt(&mut s);
    assert!(
        p.mode_badge.is_none(),
        "Auto mode must have no badge, got: {:?}",
        p.mode_badge
    );
}

// ── 6. Non-auto mode has badge ────────────────────────────────────────────────

#[test]
fn test_non_auto_mode_has_badge() {
    let mut s = make_state();
    // Switch to Plan mode
    s.mode_manager.set("plan").expect("plan mode must exist");
    let p = build_prompt(&mut s);
    assert!(p.mode_badge.is_some(), "Plan mode must have a badge");
    let badge = p.mode_badge.unwrap();
    assert!(
        badge.contains("PLAN"),
        "badge must contain 'PLAN': got {badge:?}"
    );
}
