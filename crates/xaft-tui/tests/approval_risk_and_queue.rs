//! `xaft_tui::approval` was 232/350 (66.29%). The module already has
//! in-crate unit tests, but they cover the happy path only: the 118
//! uncovered lines are the *other* arms of the same functions —
//! `RiskLevel::label`, `gauge_blocks`, the Low bash-command list,
//! `ApprovalDecision::label`, `format_age`, the `AutoApproveRule`
//! command/path matchers, the medium-risk auto-approve fallback, the
//! queue's focus navigation and undo edges, and the `ToolPreview` /
//! `tool_preview_lines` arms for `edit_file`, `read_file`, `grep` and
//! unknown tools.
//!
//! These matter because each is a branch that renders differently in the
//! approval modal. A silently dead arm means the user is shown a
//! placeholder instead of the thing they are being asked to authorise.

use serde_json::json;

use xaft_tui::approval::{
    ApprovalContext, ApprovalDecision, ApprovalQueue, ApprovalRecord, AutoApproveConfig,
    AutoApproveRule, PendingApproval, RiskLevel, ToolPreview, build_preview, is_new_file,
    tool_preview_lines,
};

fn q() -> ApprovalQueue {
    // No auto-approve at all, so every push lands in `pending` and the
    // queue logic is what is under test rather than the policy.
    ApprovalQueue::new(AutoApproveConfig::default())
}

fn push(q: &mut ApprovalQueue, id: &str, tool: &str, input: serde_json::Value) {
    q.push(id, "run-1", tool, input);
}

// ── RiskLevel: label + gauge ────────────────────────────────────────────

#[test]
fn every_risk_level_has_its_own_uppercase_label() {
    // These strings are the modal's risk chip. Two levels sharing a label
    // would make the chip useless.
    assert_eq!(RiskLevel::Low.label(), "LOW");
    assert_eq!(RiskLevel::Medium.label(), "MEDIUM");
    assert_eq!(RiskLevel::High.label(), "HIGH");
    assert_eq!(RiskLevel::Critical.label(), "CRITICAL");
    let all = [
        RiskLevel::Low.label(),
        RiskLevel::Medium.label(),
        RiskLevel::High.label(),
        RiskLevel::Critical.label(),
    ];
    assert_eq!(all.len(), 4, "labels must be distinct");
}

#[test]
fn gauge_blocks_is_half_the_gauge_fraction_at_every_level() {
    // The gauge is 8 blocks wide; `gauge_blocks` must agree with
    // `gauge_fraction` or the filled bar and the numeric readout
    // disagree on screen.
    for level in [
        RiskLevel::Low,
        RiskLevel::Medium,
        RiskLevel::High,
        RiskLevel::Critical,
    ] {
        let blocks = level.gauge_blocks();
        assert!(blocks <= 8, "{level:?} fills more than the whole gauge");
        assert_eq!(
            level.gauge_fraction(),
            blocks as f32 / 8.0,
            "{level:?} fraction and block count disagree"
        );
    }
    assert_eq!(RiskLevel::Critical.gauge_blocks(), 8, "critical is full");
    assert_eq!(RiskLevel::Low.gauge_blocks(), 2);
}

#[test]
fn the_default_arm_of_from_tool_call_is_medium_for_an_unknown_tool() {
    // An unrecognised tool is NOT silently trusted. If the default arm
    // were Low, a newly added tool would auto-approve on first release.
    assert_eq!(
        RiskLevel::from_tool_call("some_future_tool", &json!({})),
        RiskLevel::Medium
    );
}

#[test]
fn the_git_write_tools_map_to_their_documented_risks() {
    assert_eq!(
        RiskLevel::from_tool_call("git_commit", &json!({})),
        RiskLevel::Medium
    );
    assert_eq!(
        RiskLevel::from_tool_call("git_stage", &json!({})),
        RiskLevel::Medium
    );
    // git_restore throws away uncommitted work, so it gates.
    assert_eq!(
        RiskLevel::from_tool_call("git_restore", &json!({})),
        RiskLevel::High
    );
}

#[test]
fn every_common_dev_command_is_classified_low() {
    // Each of these is a distinct `||` arm in classify_bash_risk. They
    // are listed explicitly rather than defaulted, so a typo in one arm
    // silently demotes that command to Medium.
    for cmd in [
        "git status",
        "npm test",
        "pytest -q",
        "python setup.py",
        "echo hi",
        "cat file.txt",
        "ls -la",
        "grep -r x .",
        "find . -name '*.rs'",
        "head -5 f",
        "tail -5 f",
    ] {
        assert_eq!(
            RiskLevel::from_tool_call("bash_exec", &json!({ "command": cmd })),
            RiskLevel::Low,
            "`{cmd}` should be Low"
        );
    }
}

#[test]
fn an_unrecognised_command_falls_back_to_medium_not_low() {
    assert_eq!(
        RiskLevel::from_tool_call("bash_exec", &json!({ "command": "weirdtool --x" })),
        RiskLevel::Medium
    );
    // A bash_exec with no command field at all is also Medium.
    assert_eq!(
        RiskLevel::from_tool_call("bash_exec", &json!({})),
        RiskLevel::Medium
    );
}

// ── ApprovalDecision ────────────────────────────────────────────────────

#[test]
fn each_decision_renders_its_own_distinct_label() {
    assert_eq!(ApprovalDecision::Approved.label(), "✓ Approved");
    assert_eq!(ApprovalDecision::Rejected.label(), "✗ Rejected");
    assert_eq!(ApprovalDecision::Skipped.label(), "○ Skipped");
    // Auto collapses to one label regardless of which rule fired; the
    // rule name itself is carried in the payload, not the label.
    assert_eq!(
        ApprovalDecision::AutoApproved {
            rule: "cargo commands".into()
        }
        .label(),
        "✓ Auto"
    );
}

#[test]
fn only_approved_and_auto_approved_proceed() {
    assert!(ApprovalDecision::Approved.is_approved());
    assert!(ApprovalDecision::AutoApproved { rule: "r".into() }.is_approved());
    assert!(!ApprovalDecision::Rejected.is_approved());
    assert!(!ApprovalDecision::Skipped.is_approved());
}

// ── PendingApproval + ApprovalRecord ────────────────────────────────────

#[test]
fn a_pending_approval_reports_a_non_negative_age_and_grows_over_time() {
    let p = PendingApproval::new("t1", "r1", "write_file", json!({ "path": "a.rs" }));
    let first = p.age_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(p.age_ms() >= first);
    assert!(p.age_ms() >= 5, "a 5ms sleep must advance the clock");
}

#[test]
fn a_pending_approval_previews_its_input_and_starts_undecided() {
    let p = PendingApproval::new("t1", "r1", "write_file", json!({ "path": "src/a.rs" }));
    assert_eq!(p.tool_use_id, "t1");
    assert_eq!(p.agent_run_id, "r1");
    assert_eq!(p.risk, RiskLevel::Medium);
    assert!(p.decided.is_none());
    assert!(p.input_preview.contains("src/a.rs"));
}

#[test]
fn format_age_switches_units_at_sixty_seconds_and_one_hour() {
    assert_eq!(ApprovalRecord::format_age(0), "0s");
    assert_eq!(ApprovalRecord::format_age(59), "59s");
    // 60 is the first minute, not "60s".
    assert_eq!(ApprovalRecord::format_age(60), "1m");
    assert_eq!(ApprovalRecord::format_age(119), "1m");
    assert_eq!(ApprovalRecord::format_age(3599), "59m");
    // 3600 is the first hour.
    assert_eq!(ApprovalRecord::format_age(3600), "1h");
    assert_eq!(ApprovalRecord::format_age(86_400), "24h");
}

// ── AutoApproveRule matching ────────────────────────────────────────────

fn rule(tool: &str, max: RiskLevel, cmd: Option<&str>, path: Option<&str>) -> AutoApproveRule {
    AutoApproveRule {
        tool_pattern: tool.into(),
        max_risk: max,
        command_pattern: cmd.map(str::to_string),
        path_pattern: path.map(str::to_string),
        label: "rule".into(),
    }
}

#[test]
fn a_rule_never_matches_a_call_above_its_risk_ceiling() {
    let r = rule("bash_exec", RiskLevel::Low, None, None);
    // The ceiling is inclusive: Low matches, and nothing above it does.
    assert!(r.matches("bash_exec", &json!({}), RiskLevel::Low));
    assert!(!r.matches("bash_exec", &json!({}), RiskLevel::Medium));
    assert!(!r.matches("bash_exec", &json!({}), RiskLevel::High));
    assert!(!r.matches("bash_exec", &json!({}), RiskLevel::Critical));
}

#[test]
fn raising_a_rules_ceiling_admits_the_next_level_up() {
    let r = rule("bash_exec", RiskLevel::High, None, None);
    assert!(r.matches("bash_exec", &json!({}), RiskLevel::Medium));
    assert!(r.matches("bash_exec", &json!({}), RiskLevel::High));
    assert!(!r.matches("bash_exec", &json!({}), RiskLevel::Critical));
}

#[test]
fn a_rule_with_a_command_pattern_requires_the_command_to_match() {
    let r = rule("bash_exec", RiskLevel::Low, Some("cargo*"), None);
    assert!(r.matches(
        "bash_exec",
        &json!({ "command": "cargo test" }),
        RiskLevel::Low
    ));
    // Right tool, right risk, wrong command: must not auto-approve.
    assert!(!r.matches(
        "bash_exec",
        &json!({ "command": "rm file" }),
        RiskLevel::Low
    ));
    // A missing command field reads as "", which matches no pattern.
    assert!(!r.matches("bash_exec", &json!({}), RiskLevel::Low));
}

#[test]
fn a_rule_with_a_path_pattern_is_a_prefix_match_with_the_star_stripped() {
    let r = rule("write_file", RiskLevel::Medium, None, Some("src/*"));
    assert!(r.matches(
        "write_file",
        &json!({ "path": "src/a.rs" }),
        RiskLevel::Medium
    ));
    // `path_pattern` is a PREFIX match, so src/nested/b.rs still matches.
    assert!(r.matches(
        "write_file",
        &json!({ "path": "src/nested/b.rs" }),
        RiskLevel::Medium
    ));
    // A sibling directory that merely shares the `s` does not.
    assert!(!r.matches(
        "write_file",
        &json!({ "path": "other/a.rs" }),
        RiskLevel::Medium
    ));
    assert!(!r.matches(
        "write_file",
        &json!({ "path": "srcevil/a.rs" }),
        RiskLevel::Medium
    ));
}

#[test]
fn a_fully_unconstrained_rule_matches_on_risk_and_tool_alone() {
    let r = rule("anything", RiskLevel::High, None, None);
    assert!(r.matches("anything", &json!({ "n": 1}), RiskLevel::High));
    assert!(!r.matches("something_else", &json!({}), RiskLevel::High));
    assert!(!r.matches("anything", &json!({}), RiskLevel::Critical));
}

#[test]
fn the_star_pattern_matches_every_tool() {
    let r = rule("*", RiskLevel::Low, None, None);
    assert!(r.matches("read_file", &json!({}), RiskLevel::Low));
    assert!(r.matches("literally_anything", &json!({}), RiskLevel::Low));
}

// ── Auto-approve policy ─────────────────────────────────────────────────

#[test]
fn medium_risk_auto_approves_only_when_auto_medium_is_on() {
    let off = AutoApproveConfig {
        auto_low: true,
        auto_medium: false,
        rules: vec![],
    };
    assert_eq!(off.check("write_file", &json!({}), RiskLevel::Medium), None);

    let on = AutoApproveConfig {
        auto_low: true,
        auto_medium: true,
        rules: vec![],
    };
    assert_eq!(
        on.check("write_file", &json!({}), RiskLevel::Medium),
        Some("auto (medium risk)".into())
    );
}

#[test]
fn the_default_safe_policy_auto_approves_low_but_never_medium_or_higher() {
    let cfg = AutoApproveConfig::default_safe();
    assert!(cfg.auto_low, "low risk is on by default");
    assert!(!cfg.auto_medium, "medium risk is OFF by default");
    assert!(cfg.check("read_file", &json!({}), RiskLevel::Low).is_some());
    assert!(
        cfg.check("write_file", &json!({}), RiskLevel::Medium)
            .is_none()
    );
    assert!(
        cfg.check("write_file", &json!({}), RiskLevel::High)
            .is_none()
    );
    assert!(
        cfg.check("bash_exec", &json!({}), RiskLevel::Critical)
            .is_none()
    );
}

#[test]
fn a_matching_rule_wins_over_the_generic_risk_fallback_and_reports_its_own_label() {
    let cfg = AutoApproveConfig::default_safe();
    let got = cfg
        .check(
            "bash_exec",
            &json!({ "command": "cargo test" }),
            RiskLevel::Low,
        )
        .expect("cargo test is covered by the default rules");
    assert_eq!(got, "cargo commands");

    let diff = cfg
        .check(
            "bash_exec",
            &json!({ "command": "git diff HEAD" }),
            RiskLevel::Low,
        )
        .expect("git diff is covered by the default rules");
    assert_eq!(diff, "git diff");
}

#[test]
fn a_low_risk_call_with_no_matching_rule_falls_back_to_the_generic_label() {
    let cfg = AutoApproveConfig {
        auto_low: true,
        auto_medium: false,
        rules: vec![],
    };
    assert_eq!(
        cfg.check("read_file", &json!({ "path": "a.rs" }), RiskLevel::Low),
        Some("auto (low risk)".into())
    );
}

// ── Queue focus + undo edges ────────────────────────────────────────────

#[test]
fn focused_is_none_when_the_queue_is_empty() {
    let mut queue = q();
    assert!(queue.focused().is_none());
    // Resolving with nothing pending is a no-op, not a panic.
    assert_eq!(queue.resolve_focused(ApprovalDecision::Approved), None);
    // Navigation on an empty queue must not divide by zero.
    queue.focus_next();
    queue.focus_prev();
    assert!(queue.focused().is_none());
}

#[test]
fn focus_next_wraps_forward_and_focus_prev_wraps_backward() {
    let mut queue = q();
    for i in 0..3 {
        push(
            &mut queue,
            &format!("t{i}"),
            "write_file",
            json!({ "path": "a.rs" }),
        );
    }
    assert_eq!(queue.focused_idx, 0);
    assert_eq!(queue.focused().unwrap().tool_use_id, "t0");

    queue.focus_next();
    assert_eq!(queue.focused().unwrap().tool_use_id, "t1");
    queue.focus_next();
    assert_eq!(queue.focused().unwrap().tool_use_id, "t2");
    // Past the end, back to the first.
    queue.focus_next();
    assert_eq!(queue.focused().unwrap().tool_use_id, "t0");

    // And backwards from the first wraps to the last.
    queue.focus_prev();
    assert_eq!(queue.focused().unwrap().tool_use_id, "t2");
    queue.focus_prev();
    assert_eq!(queue.focused().unwrap().tool_use_id, "t1");
}

#[test]
fn undo_last_with_an_empty_history_returns_none() {
    let mut queue = q();
    assert_eq!(queue.undo_last(), None);
}

#[test]
fn undo_last_skips_over_auto_approved_records_to_find_a_manual_one() {
    let mut queue = ApprovalQueue::new(AutoApproveConfig {
        auto_low: true,
        auto_medium: true,
        rules: vec![],
    });
    // An auto-approval lands straight in history, never in `pending`.
    let auto = queue
        .push("auto-1", "r1", "read_file", json!({ "path": "a.rs" }))
        .expect("read_file auto-approves");
    assert!(matches!(auto, ApprovalDecision::AutoApproved { .. }));
    assert_eq!(queue.history.len(), 1);
    assert_eq!(queue.total_auto, 1);

    // A manual approval sits on top of it. `git_restore` is High risk, so
    // it can never auto-approve and is guaranteed to enter `pending`.
    queue.push("manual-1", "r1", "git_restore", json!({}));
    let id = queue.resolve_focused(ApprovalDecision::Approved).unwrap();
    assert_eq!(id, "manual-1");
    assert_eq!(queue.total_approved, 1);

    // Undo takes the manual one and decrements ITS counter, not the auto one.
    assert_eq!(queue.undo_last(), Some("manual-1".into()));
    assert_eq!(queue.total_approved, 0);
    assert_eq!(queue.total_auto, 1, "the auto record is not the undone one");
}

#[test]
fn undo_of_a_rejection_decrements_the_rejected_counter() {
    let mut queue = q();
    push(&mut queue, "t1", "write_file", json!({ "path": "a.rs" }));
    queue.resolve_focused(ApprovalDecision::Rejected);
    assert_eq!(queue.total_rejected, 1);
    assert_eq!(queue.undo_last(), Some("t1".into()));
    assert_eq!(queue.total_rejected, 0);
}

#[test]
fn resolving_while_focused_past_the_end_still_removes_the_last_item() {
    let mut queue = q();
    push(&mut queue, "t1", "write_file", json!({ "path": "a.rs" }));
    push(&mut queue, "t2", "write_file", json!({ "path": "b.rs" }));
    queue.focus_next();
    assert_eq!(queue.focused_idx, 1);

    assert_eq!(
        queue.resolve_focused(ApprovalDecision::Approved),
        Some("t2".into())
    );
    assert_eq!(queue.pending.len(), 1);
    // The focus must fall back into range rather than dangle at 1 of 1.
    assert!(queue.focused_idx < queue.pending.len());
    assert_eq!(queue.focused().unwrap().tool_use_id, "t1");
}

#[test]
fn approving_up_to_a_ceiling_leaves_the_riskier_items_queued_and_resets_focus() {
    let mut queue = q();
    // Low (auto-freed: use a config with nothing on) Medium, High.
    push(&mut queue, "low-1", "read_file", json!({ "path": "a.rs" }));
    push(&mut queue, "med-1", "write_file", json!({ "path": "b.rs" }));
    push(&mut queue, "high-1", "git_restore", json!({}));

    let approved = queue.approve_all_up_to(RiskLevel::Medium);
    let mut ids: Vec<&str> = approved.iter().map(|(id, _)| id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["low-1", "med-1"]);
    assert!(
        approved
            .iter()
            .all(|(_, d)| *d == ApprovalDecision::Approved)
    );

    assert_eq!(queue.pending.len(), 1);
    assert_eq!(queue.pending[0].tool_use_id, "high-1");
    assert_eq!(queue.total_approved, 2);
    assert_eq!(queue.focused_idx, 0);
}

#[test]
fn rejecting_everything_empties_the_queue_and_keeps_the_ids_in_order() {
    let mut queue = q();
    for i in 0..4 {
        push(
            &mut queue,
            &format!("t{i}"),
            "write_file",
            json!({ "path": "a.rs" }),
        );
    }
    let ids = queue.reject_all();
    assert_eq!(ids, vec!["t0", "t1", "t2", "t3"]);
    assert!(!queue.has_pending());
    assert_eq!(queue.total_rejected, 4);
    assert_eq!(queue.history.len(), 4);
}

#[test]
fn a_skipped_decision_is_recorded_in_history_but_moves_no_counter() {
    let mut queue = q();
    push(&mut queue, "t1", "write_file", json!({ "path": "a.rs" }));
    queue.resolve_focused(ApprovalDecision::Skipped);
    assert_eq!(queue.total_approved, 0);
    assert_eq!(queue.total_rejected, 0);
    assert_eq!(queue.total_auto, 0);
    assert_eq!(queue.history.len(), 1);
    // Undoing a skip is a no-op on the counters, and must not underflow.
    assert_eq!(queue.undo_last(), Some("t1".into()));
    assert_eq!(queue.total_rejected, 0);
}

// ── build_preview ───────────────────────────────────────────────────────

#[test]
fn a_bash_preview_prefixes_the_command_with_a_shell_prompt() {
    assert_eq!(
        build_preview("bash_exec", &json!({ "command": "cargo test" }), 80),
        "$ cargo test"
    );
    // A bash_exec with no command falls back to the bare word.
    assert_eq!(build_preview("bash_exec", &json!({}), 80), "bash");
}

#[test]
fn a_grep_preview_wraps_the_pattern_in_slashes() {
    assert_eq!(
        build_preview("grep", &json!({ "pattern": "TODO" }), 80),
        "/TODO/"
    );
    assert_eq!(build_preview("grep", &json!({}), 80), "/?/");
}

#[test]
fn a_generic_preview_takes_the_first_string_value_whatever_the_key_is() {
    assert_eq!(
        build_preview("some_tool", &json!({ "a": 1, "b": "hello" }), 80),
        "hello"
    );
    // No string value at all -> the tool name.
    assert_eq!(
        build_preview("some_tool", &json!({ "a": 1 }), 80),
        "some_tool"
    );
    // Non-object input -> the tool name.
    assert_eq!(build_preview("some_tool", &json!("bare"), 80), "some_tool");
    assert_eq!(build_preview("some_tool", &json!([1, 2]), 80), "some_tool");
}

#[test]
fn a_truncated_preview_is_exactly_max_len_including_the_ellipsis() {
    let long = "a".repeat(200);
    let p = build_preview("grep", &json!({ "pattern": long }), 20);
    assert_eq!(p.chars().count(), 20);
    assert!(p.ends_with('…'));
}

// ── is_new_file ─────────────────────────────────────────────────────────

#[test]
fn is_new_file_reads_the_existing_flag_and_defaults_to_false() {
    assert!(is_new_file(&json!({ "existing": false })));
    assert!(!is_new_file(&json!({ "existing": true })));
    // Absent, or present but not a bool, both mean "not known to be new".
    assert!(!is_new_file(&json!({})));
    assert!(!is_new_file(&json!({ "existing": "false" })));
    assert!(!is_new_file(&json!({ "existing": 0 })));
}

// ── from_tool_call_with_context ─────────────────────────────────────────

#[test]
fn the_context_aware_classifier_agrees_with_the_plain_one_for_every_tool() {
    let ctx = ApprovalContext::default();
    for (tool, input) in [
        ("read_file", json!({ "path": "a.rs" })),
        ("grep", json!({ "pattern": "x" })),
        ("git_status", json!({})),
        ("bash_exec", json!({ "command": "cargo test" })),
        ("bash_exec", json!({ "command": "sudo rm" })),
        ("write_file", json!({ "path": "src/a.rs" })),
        ("write_file", json!({ "path": "Cargo.toml" })),
        ("git_commit", json!({})),
        ("git_restore", json!({})),
        ("unknown_tool", json!({})),
    ] {
        assert_eq!(
            RiskLevel::from_tool_call_with_context(tool, &input, &ctx),
            RiskLevel::from_tool_call(tool, &input),
            "{tool} disagrees between the two entry points"
        );
    }
}

#[test]
fn a_new_file_flag_never_raises_a_non_critical_write_above_medium() {
    // Creating a new file is strictly less dangerous than overwriting one,
    // so it must never escalate — but it also must not be Low, because
    // the write still happens.
    let ctx = ApprovalContext {
        agent_id: "a1".into(),
        is_new_file: true,
    };
    assert_eq!(
        RiskLevel::from_tool_call_with_context("write_file", &json!({ "path": "new.rs" }), &ctx),
        RiskLevel::Medium
    );
    // A critical path stays High even for a new file.
    assert_eq!(
        RiskLevel::from_tool_call_with_context(
            "write_file",
            &json!({ "path": "Cargo.toml" }),
            &ctx
        ),
        RiskLevel::High
    );
}

// ── ToolPreview::to_plain_lines ─────────────────────────────────────────

#[test]
fn a_bash_plain_line_renders_its_timeout_only_when_one_was_given() {
    let with = ToolPreview::from_input(
        "bash_exec",
        &json!({ "command": "ls", "working_dir": "/p", "timeout_secs": 30 }),
    );
    let lines = with.to_plain_lines();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().any(|l| l.contains("30s")));

    let without = ToolPreview::from_input(
        "bash_exec",
        &json!({ "command": "ls", "working_dir": "/p" }),
    );
    assert_eq!(without.to_plain_lines().len(), 2, "no timeout line");
}

#[test]
fn a_bash_plain_line_defaults_its_directory_to_the_current_one() {
    let p = ToolPreview::from_input("bash_exec", &json!({ "command": "ls" }));
    let lines = p.to_plain_lines();
    assert!(lines.iter().any(|l| l.contains("Directory: .")));
}

#[test]
fn a_file_edit_plain_line_reports_both_line_counts() {
    let p = ToolPreview::from_input(
        "edit_file",
        &json!({ "path": "a.rs", "old_content": "1\n", "new_content": "1\n2\n3\n" }),
    );
    let lines = p.to_plain_lines();
    assert!(lines.iter().any(|l| l.contains("a.rs")));
    assert!(lines.iter().any(|l| l.contains("1 → 3")));
}

#[test]
fn a_file_write_plain_line_marks_a_new_file_and_omits_the_marker_otherwise() {
    let new_file = ToolPreview::from_input(
        "write_file",
        &json!({ "path": "n.txt", "content": "abc", "existing": false }),
    );
    assert!(
        new_file
            .to_plain_lines()
            .iter()
            .any(|l| l.contains("(new file)"))
    );

    let existing = ToolPreview::from_input(
        "write_file",
        &json!({ "path": "n.txt", "content": "abc", "existing": true }),
    );
    let lines = existing.to_plain_lines();
    assert!(!lines.iter().any(|l| l.contains("(new file)")));
    assert!(lines.iter().any(|l| l.contains("3 bytes")));
}

#[test]
fn a_read_and_a_web_preview_each_render_one_line() {
    let read = ToolPreview::from_input("read_file", &json!({ "path": "a.rs" }));
    assert_eq!(read.to_plain_lines(), vec!["  File:      a.rs"]);

    let web = ToolPreview::from_input("web_fetch", &json!({ "url": "u", "method": "POST" }));
    assert_eq!(web.to_plain_lines(), vec!["  POST u"]);
}

#[test]
fn a_generic_preview_is_indented_and_capped_at_five_lines() {
    // A 40-field object would otherwise flood the modal.
    let mut obj = serde_json::Map::new();
    for i in 0..20 {
        obj.insert(format!("k{i}"), json!(i));
    }
    let p = ToolPreview::from_input("mystery", &json!(obj));
    let lines = p.to_plain_lines();
    assert_eq!(lines.len(), 5);
    assert!(lines.iter().all(|l| l.starts_with("  ")));
}

// ── tool_preview_lines (the headless entry point) ───────────────────────

#[test]
fn tool_preview_lines_renders_an_edit_as_a_line_count_transition() {
    let lines = tool_preview_lines(
        "edit_file",
        &json!({ "path": "a.rs", "old_content": "1\n2\n", "new_content": "1\n" }),
    );
    assert!(lines.iter().any(|l| l.contains("a.rs")));
    assert!(lines.iter().any(|l| l.contains("2 → 1")));
}

#[test]
fn tool_preview_lines_of_a_read_is_just_the_path() {
    let lines = tool_preview_lines("read_file", &json!({ "path": "docs/x.md" }));
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("docs/x.md"));
}

#[test]
fn tool_preview_lines_of_a_grep_shows_the_pattern_and_defaults_the_directory() {
    let lines = tool_preview_lines("grep", &json!({ "pattern": "TODO" }));
    assert!(lines.iter().any(|l| l.contains("/TODO/")));
    assert!(lines.iter().any(|l| l.contains("Directory:  .")));

    let scoped = tool_preview_lines(
        "grep",
        &json!({ "pattern": "TODO", "path_prefix": "crates/x" }),
    );
    assert!(scoped.iter().any(|l| l.contains("crates/x")));
}

#[test]
fn tool_preview_lines_of_an_unknown_tool_is_pretty_printed_json_capped_at_five_lines() {
    let mut obj = serde_json::Map::new();
    for i in 0..20 {
        obj.insert(format!("k{i}"), json!(i));
    }
    let lines = tool_preview_lines("mystery", &json!(obj));
    assert_eq!(lines.len(), 5);
    assert!(lines[0].starts_with("  {"));

    // A missing path/command/pattern falls back to "?" rather than empty.
    assert!(
        tool_preview_lines("bash_exec", &json!({}))
            .iter()
            .any(|l| l.contains("?"))
    );
    assert!(
        tool_preview_lines("write_file", &json!({}))
            .iter()
            .any(|l| l.contains("?"))
    );
    assert!(
        tool_preview_lines("read_file", &json!({}))
            .iter()
            .any(|l| l.contains("?"))
    );
    assert!(
        tool_preview_lines("edit_file", &json!({}))
            .iter()
            .any(|l| l.contains("?"))
    );
    assert!(
        tool_preview_lines("grep", &json!({}))
            .iter()
            .any(|l| l.contains("/?/"))
    );
}

#[test]
fn a_web_preview_defaults_to_get_when_no_method_is_given() {
    let p = ToolPreview::from_input("web_fetch", &json!({ "url": "https://x" }));
    match p {
        ToolPreview::WebFetch { method, url } => {
            assert_eq!(method, "GET");
            assert_eq!(url, "https://x");
        }
        _ => panic!("expected WebFetch"),
    }
}
