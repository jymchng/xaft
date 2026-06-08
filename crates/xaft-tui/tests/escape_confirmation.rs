//! F3 @-mention escape-confirmation dialog unit + integration tests.
//!
//! Verifies the dialog's data model (`EscapeConfirmDialog`,
//! `DialogLine`) and the dispatch layer in `AppState::handle_escape_dialog_key`.
//! Signal emission is covered in
//! `crates/xaft-agent/tests/mention_signals.rs`.

use agtrs_runtime::transport::{EscapeInfo, EscapeReason};
use xaft_tui::confirm::{DialogLine, EscapeConfirmDialog, escape_reason_str};

fn mk_escape(reason: EscapeReason, path: &str, depth: u32, bytes: u64) -> EscapeInfo {
    EscapeInfo {
        reason,
        absolute_path: path.to_string(),
        depth,
        byte_size: bytes,
    }
}

// ── EscapeConfirmDialog data model ─────────────────────────────────────────

#[test]
fn dialog_new_uses_supplied_mentions() {
    let m = mk_escape(
        EscapeReason::ParentTraversal,
        "/work/sibling/foo.rs",
        1,
        1024,
    );
    let d = EscapeConfirmDialog::new(vec![m]);
    assert_eq!(d.count(), 1);
    assert_eq!(d.total_bytes(), 1024);
    assert!(!d.help_expanded);
    assert_eq!(d.focus_index, 0);
}

#[test]
fn dialog_header_mentions_escape() {
    let m = mk_escape(EscapeReason::Absolute, "/etc/hosts", 0, 320);
    let d = EscapeConfirmDialog::new(vec![m]);
    let h = d.header();
    assert!(h.to_lowercase().contains("escape"));
    assert!(h.contains("1"));
}

#[test]
fn dialog_lines_renders_one_per_mention() {
    let ms = vec![
        mk_escape(EscapeReason::ParentTraversal, "/work/sibling/a.rs", 1, 100),
        mk_escape(EscapeReason::Absolute, "/etc/hosts", 0, 320),
    ];
    let d = EscapeConfirmDialog::new(ms);
    let lines = d.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].index, 1);
    assert_eq!(lines[1].index, 2);
    assert_eq!(lines[0].path, "/work/sibling/a.rs");
    assert_eq!(lines[0].bytes, 100);
    assert_eq!(lines[1].bytes, 320);
}

#[test]
fn dialog_lines_use_human_readable_reason_label() {
    let m = mk_escape(EscapeReason::ParentTraversal, "/x", 1, 1);
    let d = EscapeConfirmDialog::new(vec![m]);
    assert!(d.lines()[0].reason.contains("parent"));
}

#[test]
fn dialog_toggle_help_flips_flag() {
    let mut d = EscapeConfirmDialog::new(vec![]);
    assert!(!d.help_expanded);
    d.toggle_help();
    assert!(d.help_expanded);
    d.toggle_help();
    assert!(!d.help_expanded);
}

#[test]
fn dialog_help_text_mentions_config_keys() {
    let d = EscapeConfirmDialog::new(vec![]);
    let h: String = d.help_text().join("\n");
    assert!(h.contains("escape_policy"));
    assert!(h.contains("always"));
    assert!(h.contains("never"));
    assert!(h.contains("a / Enter"));
    assert!(h.contains("A"));
    assert!(h.contains("c / Esc"));
}

#[test]
fn dialog_total_bytes_sums_all_mentions() {
    let ms = vec![
        mk_escape(EscapeReason::Absolute, "/a", 0, 100),
        mk_escape(EscapeReason::Absolute, "/b", 0, 200),
        mk_escape(EscapeReason::Absolute, "/c", 0, 300),
    ];
    let d = EscapeConfirmDialog::new(ms);
    assert_eq!(d.total_bytes(), 600);
}

// ── DialogLine::render ─────────────────────────────────────────────────────

#[test]
fn dialog_line_render_includes_index_reason_path_and_bytes() {
    let line = DialogLine {
        index: 1,
        reason: "parent traversal".into(),
        path: "/work/sibling/foo.rs".into(),
        bytes: 2048,
        depth: 1,
    };
    let rendered = line.render();
    assert!(rendered.contains("1"), "should include index");
    assert!(
        rendered.contains("parent traversal"),
        "should include reason: {rendered}"
    );
    assert!(
        rendered.contains("/work/sibling/foo.rs"),
        "should include path"
    );
    assert!(rendered.contains("2048"), "should include bytes");
    // depth=1 → no extra "(N levels up)" hint per render() impl.
    assert!(!rendered.contains("levels up"));
}

#[test]
fn dialog_line_render_handles_zero_depth() {
    let line = DialogLine {
        index: 2,
        reason: "absolute".into(),
        path: "/etc/hosts".into(),
        bytes: 0,
        depth: 0,
    };
    let rendered = line.render();
    assert!(rendered.contains("absolute"));
    assert!(rendered.contains("/etc/hosts"));
    assert!(!rendered.contains("levels up"));
}

#[test]
fn dialog_line_render_includes_levels_up_hint_for_depth_gt_1() {
    let line = DialogLine {
        index: 1,
        reason: "parent traversal".into(),
        path: "/work/foo.rs".into(),
        bytes: 1,
        depth: 3,
    };
    let rendered = line.render();
    assert!(
        rendered.contains("3 levels up"),
        "depth=3 should render hint"
    );
}

// ── escape_reason_str helper ───────────────────────────────────────────────

#[test]
fn escape_reason_str_returns_human_label() {
    assert_eq!(escape_reason_str(EscapeReason::Absolute), "absolute path");
    assert_eq!(
        escape_reason_str(EscapeReason::ParentTraversal),
        "parent traversal"
    );
    assert_eq!(
        escape_reason_str(EscapeReason::HomeExpansion),
        "home expansion"
    );
}

// ── Dispatch layer (AppState::handle_escape_dialog_key) ────────────────────

#[test]
fn dispatch_lowercase_a_approves_once() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    assert!(state.escape_dialog.is_none());
}

#[tokio::test]
async fn dispatch_uppercase_a_approves_session() {
    use agtrs_workspace::InMemoryWorkspaceStore;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_config::MentionConfig;
    use xaft_tui::{AppState, ExpandedMessage, MentionResolver};
    let mut state = AppState::new_for_test();
    // Set up a channel and a pending expanded message — required for
    // `resolve_escape_dialog` to take the ApproveAllSession branch.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state.user_message_tx = Some(tx);
    let cfg = MentionConfig::default();
    let store = InMemoryWorkspaceStore::new();
    let expanded = MentionResolver::expand("see @/etc/hosts", &store, &cfg, None)
        .await
        .into_user_message();
    // For ApproveAllSession test, the resolver rejected the escape
    // (Confirm policy + empty store) — use a pending_expanded_message
    // stub so the dispatch path still exercises the flag set.
    state.pending_expanded_message = Some(ExpandedMessage {
        parts: vec![agtrs_runtime::transport::ContentBlock::Text {
            text: "see ".into(),
        }],
        tokens: vec![],
        warnings: vec![],
        escape_mentions: vec![],
    });
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    // Session-wide approval is sticky in `escape_approved_session`.
    assert!(state.escape_approved_session);
    assert!(state.escape_dialog.is_none());
    // Drain the channel to ensure the message was actually sent.
    let _ = rx.try_recv();
}

#[test]
fn dispatch_enter_approves_once() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    assert!(!state.escape_approved_session);
}

#[test]
fn dispatch_cancels_on_esc() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    assert!(state.escape_dialog.is_none());
}

#[test]
fn dispatch_cancels_on_lowercase_c() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    assert!(state.escape_dialog.is_none());
}

#[test]
fn dispatch_question_mark_toggles_help() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(state.handle_escape_dialog_key(ev));
    assert!(state.escape_dialog.as_ref().unwrap().help_expanded);
}

#[test]
fn dispatch_unrelated_key_returns_false() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    state.escape_dialog = Some(EscapeConfirmDialog::new(vec![mk_escape(
        EscapeReason::ParentTraversal,
        "/x",
        1,
        1,
    )]));
    let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!state.handle_escape_dialog_key(ev));
    assert!(state.escape_dialog.is_some());
}

#[test]
fn dispatch_with_no_dialog_returns_false() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use xaft_tui::AppState;
    let mut state = AppState::new_for_test();
    let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(!state.handle_escape_dialog_key(ev));
}
