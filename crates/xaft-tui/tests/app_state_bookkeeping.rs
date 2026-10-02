//! Bookkeeping side of `AppState` — the small, stateful helpers that carry
//! user-visible consequences but had no test referencing them at all.
//!
//! Five methods were completely unreferenced by the suite before this file:
//! `push_separator`, `take_mode_notification`, `check_ctrl_c_timeout`,
//! `flush_tool_group` and `init_skills`. Each is only a handful of lines, so
//! they are cheap to cover — but they are exactly the kind of code that rots,
//! because nothing calls them by name in a test and a regression shows up only
//! as a subtly wrong line in a terminal nobody is watching.
//!
//! What is asserted here is behaviour a user would notice:
//!
//! * a separator reaches the scroll buffer, so `push_separator` is not a
//!   no-op that silently drops the visual break between tasks;
//! * the mode-switch notification is delivered **once** (taking it clears the
//!   slot), so the banner cannot be printed twice;
//! * a second Ctrl+C quits *only after* the 2s grace period, and the resume
//!   hint differs depending on whether a session exists;
//! * the tool-group overflow line is flushed to the scroll buffer at a
//!   conversation boundary rather than being left only in the live footer,
//!   and is correctly suppressed for a single call;
//! * `$` picks up the skills actually loaded, and stops offering the ones that
//!   were not — the picker is driven by records, not a hardcoded list.

use std::time::{Duration, Instant};

use xaft_skills::Skill;
use xaft_tui::AppState;
use xaft_tui::state::commit_line_texts;
use xaft_tui::transcript::LineKind;

// ── push_separator ──────────────────────────────────────────────────────────

#[test]
fn push_separator_emits_a_separator_commit_line() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();

    s.push_separator();

    let texts = commit_line_texts(&s.mutations);
    assert_eq!(
        texts.len(),
        1,
        "push_separator must commit exactly one line, got {texts:?}"
    );
    // The separator is a horizontal rule: a run of box-drawing dashes.
    assert!(
        texts[0].chars().all(|c| c == '─'),
        "separator should be all U+2500, got {:?}",
        texts[0]
    );
    assert!(texts[0].len() >= 40, "separator too short: {:?}", texts[0]);
}

#[test]
fn push_separator_is_tagged_as_a_separator_line() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    s.push_separator();

    match &s.mutations[0] {
        xaft_tui::RenderMutation::CommitLine(line) => {
            assert_eq!(line.kind, LineKind::Separator);
        }
        other => panic!("expected CommitLine, got {other:?}"),
    }
}

#[test]
fn push_separator_appends_rather_than_replaces() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();

    s.push_separator();
    s.push_separator();

    // Two tasks produce two rules; the second must not overwrite the first.
    assert_eq!(commit_line_texts(&s.mutations).len(), 2);
}

// ── take_mode_notification ──────────────────────────────────────────────────

#[test]
fn take_mode_notification_is_none_when_never_set() {
    let mut s = AppState::new_for_test();
    assert!(s.take_mode_notification().is_none());
}

#[test]
fn take_mode_notification_returns_then_clears() {
    let mut s = AppState::new_for_test();
    s.mode_notification = Some("✦ Switched to plan mode".to_string());

    let first = s.take_mode_notification();
    assert_eq!(first.as_deref(), Some("✦ Switched to plan mode"));

    // One-shot: the banner must not be reprinted on the next frame.
    assert!(
        s.take_mode_notification().is_none(),
        "notification must be consumed, not repeated"
    );
    assert!(s.mode_notification.is_none());
}

#[test]
fn take_mode_notification_preserves_an_emptied_string() {
    // An empty notification is still `Some("")` — a distinct state from
    // `None`, and `take` must not conflate the two.
    let mut s = AppState::new_for_test();
    s.mode_notification = Some(String::new());
    assert_eq!(s.take_mode_notification().as_deref(), Some(""));
    assert!(s.take_mode_notification().is_none());
}

// ── check_ctrl_c_timeout / the double-Ctrl+C quit ───────────────────────────

#[test]
fn ctrl_c_timeout_does_nothing_when_cancel_not_requested() {
    let mut s = AppState::new_for_test();
    // An old first-press timestamp but no outstanding cancel: the second press
    // never happened, so the user must not be kicked out.
    s.cancel_requested = false;
    s.first_ctrl_c_at = Some(Instant::now() - Duration::from_secs(30));

    s.check_ctrl_c_timeout();

    assert!(!s.should_quit, "must not quit without a pending cancel");
    assert!(s.exit_resume_hint.is_none());
}

#[test]
fn ctrl_c_timeout_does_nothing_before_the_grace_period() {
    let mut s = AppState::new_for_test();
    s.cancel_requested = true;
    s.first_ctrl_c_at = Some(Instant::now());

    s.check_ctrl_c_timeout();

    assert!(
        !s.should_quit,
        "a single Ctrl+C must not quit; the 2s window is the whole point"
    );
    assert!(s.exit_resume_hint.is_none());
}

#[test]
fn ctrl_c_timeout_quits_after_the_grace_period_without_a_session() {
    let mut s = AppState::new_for_test();
    s.cancel_requested = true;
    s.first_ctrl_c_at = Some(Instant::now() - Duration::from_secs(3));

    s.check_ctrl_c_timeout();

    assert!(s.should_quit, "cancel older than 2s must trigger the quit");
    // No session id, so the hint must not tell the user to `--resume <id>`.
    let hint = s
        .exit_resume_hint
        .as_deref()
        .expect("quitting must record a resume hint");
    assert!(!hint.contains("--resume"), "hint must not name a session: {hint}");
    assert!(hint.contains("--continue"), "hint should mention --continue");
}

#[test]
fn ctrl_c_timeout_is_idempotent_once_quit() {
    let mut s = AppState::new_for_test();
    s.cancel_requested = true;
    s.first_ctrl_c_at = Some(Instant::now() - Duration::from_secs(3));

    s.check_ctrl_c_timeout();
    let first = s.exit_resume_hint.clone();
    s.check_ctrl_c_timeout();

    assert!(s.should_quit);
    assert_eq!(
        s.exit_resume_hint, first,
        "re-checking must not rewrite the hint"
    );
}

#[test]
fn ctrl_c_timeout_without_a_timestamp_does_not_quit() {
    // `cancel_requested` set but `first_ctrl_c_at` missing: the `if let Some`
    // guard must swallow this rather than panic or force a quit.
    let mut s = AppState::new_for_test();
    s.cancel_requested = true;
    s.first_ctrl_c_at = None;

    s.check_ctrl_c_timeout();

    assert!(!s.should_quit);
    assert!(s.exit_resume_hint.is_none());
}

// ── flush_tool_group ───────────────────────────────────────────────────────

#[test]
fn flush_tool_group_adds_nothing_for_a_single_call() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    s.tool_group.record_completed("read_file");

    s.flush_tool_group();

    assert!(
        commit_line_texts(&s.mutations).is_empty(),
        "one call has no overflow to collapse, so nothing should be committed"
    );
}

#[test]
fn flush_tool_group_commits_the_overflow_line() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    for name in ["read_file", "grep", "run_command"] {
        s.tool_group.record_completed(name);
    }

    s.flush_tool_group();

    let texts = commit_line_texts(&s.mutations);
    assert_eq!(texts.len(), 1, "expected the overflow line, got {texts:?}");
    // agenthicc parity: "...and N more tool calls" with N = count - 1.
    assert!(
        texts[0].contains("and 2 more tool calls"),
        "expected 'and 2 more tool calls', got {:?}",
        texts[0]
    );
}

#[test]
fn flush_tool_group_singularises_for_exactly_two_calls() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    s.tool_group.record_completed("a");
    s.tool_group.record_completed("b");

    s.flush_tool_group();

    let texts = commit_line_texts(&s.mutations);
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].contains("and 1 more tool call") && !texts[0].contains("calls"),
        "two calls must read '1 more tool call', got {:?}",
        texts[0]
    );
}

#[test]
fn flush_tool_group_tags_the_line_as_a_tool_result() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    s.tool_group.record_completed("a");
    s.tool_group.record_completed("b");

    s.flush_tool_group();

    match &s.mutations[0] {
        xaft_tui::RenderMutation::CommitLine(line) => {
            assert_eq!(line.kind, LineKind::ToolResult);
        }
        other => panic!("expected CommitLine, got {other:?}"),
    }
}

#[test]
fn flush_tool_group_closes_the_group() {
    let mut s = AppState::new_for_test();
    s.mutations.clear();
    s.tool_group.record_completed("a");
    s.tool_group.record_completed("b");
    s.flush_tool_group();
    let after_first = s.mutations.len();

    // A second flush with no new calls must not re-emit the same line.
    s.flush_tool_group();

    assert_eq!(s.mutations.len(), after_first, "group was not closed");
    assert!(
        !s.tool_group.is_open(),
        "the group must be closed after a flush"
    );
}

// ── init_skills ─────────────────────────────────────────────────────────────

fn skill(name: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: format!("{name} skill"),
        tags: vec![],
        body: String::new(),
        source_path: format!("/tmp/{name}.md"),
        priority: 0,
    }
}

#[test]
fn init_skills_installs_a_dollar_handler() {
    let mut s = AppState::new_for_test();
    s.init_skills(vec![skill("review")]);
    assert!(
        s.trigger_registry.get('$').is_some(),
        "the $ trigger must be registered for the skill picker"
    );
}

#[test]
fn init_skills_with_no_skills_still_leaves_a_handler() {
    // `replace` asserts a handler is already registered, so the empty list
    // must not panic and must leave the picker inert rather than absent.
    let mut s = AppState::new_for_test();
    s.init_skills(vec![]);
    assert!(s.trigger_registry.get('$').is_some());
}

#[test]
fn init_skills_is_idempotent() {
    // `app.rs` may call this more than once across a re-init; a second call
    // must replace cleanly rather than panic on the `replace` assertion.
    let mut s = AppState::new_for_test();
    s.init_skills(vec![skill("review")]);
    s.init_skills(vec![skill("deploy")]);
    assert!(s.trigger_registry.get('$').is_some());
}

#[test]
fn init_skills_offers_the_loaded_skill_after_the_dollar_trigger() {
    let mut s = AppState::new_for_test();
    s.init_skills(vec![skill("review")]);

    s.input_bar.set_text("$rev");
    s.refresh_trigger();

    let active = s
        .active_trigger
        .as_ref()
        .expect("typing after '$' must open the skill picker");
    assert_eq!(active.scan.trigger_char, '$');
    assert_eq!(active.scan.prefix, "rev");
    assert!(
        !active.items.is_empty(),
        "the loaded 'review' skill should match the prefix"
    );
    assert!(
        active.items.iter().any(|i| i.display.contains("review")),
        "expected the 'review' skill in {:?}",
        active.items.iter().map(|i| &i.display).collect::<Vec<_>>()
    );
}

#[test]
fn init_skills_does_not_offer_skills_that_were_not_loaded() {
    let mut s = AppState::new_for_test();
    s.init_skills(vec![skill("review")]);

    s.input_bar.set_text("$deploy");
    s.refresh_trigger();

    // `$deploy` is not a loaded skill, so the picker must come back empty
    // rather than offering something the runtime cannot inject.
    let active = s
        .active_trigger
        .as_ref()
        .expect("picker still opens after '$'");
    assert!(
        active.items.is_empty(),
        "unloaded skill must not be offered, got {:?}",
        active.items.iter().map(|i| &i.display).collect::<Vec<_>>()
    );
}
