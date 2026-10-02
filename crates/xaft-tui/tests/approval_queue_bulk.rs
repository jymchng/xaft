//! Approval-queue bulk operations and preview geometry.
//!
//! `ApprovalQueue` is the gate every destructive tool call passes through, so
//! the bulk operations that drain it — `approve_all_up_to` and `reject_all` —
//! carry real consequences: they decide, in one keystroke, whether several
//! pending tool calls run or are refused. Each has to return exactly the
//! tool_use_ids the caller must forward to the gate, and must not touch the
//! items on the other side of the risk threshold.
//!
//! `ToolPreview::content_height` drives the approval dialog's layout, so a
//! wrong value shows up as a preview that overflows its frame or a dialog that
//! is mostly empty space.
//!
//! The risk ordering these tests rely on is the declaration order of
//! `RiskLevel` (Low < Medium < High < Critical), so `risk <= max_risk` is a
//! genuine threshold rather than an arbitrary comparison.

use serde_json::json;

use xaft_tui::approval::{ApprovalDecision, ApprovalQueue, RiskLevel, ToolPreview};
use xaft_tui::AutoApproveConfig;

/// A queue with no auto-approve rules, so everything reaches `pending`.
fn manual_queue() -> ApprovalQueue {
    ApprovalQueue::new(AutoApproveConfig::default())
}

/// Inputs chosen so `RiskLevel::from_tool_call` lands on each level.
fn low_risk_input() -> serde_json::Value {
    json!({"path": "README.md"})
}

fn pending_with_risk(tool: &str, input: serde_json::Value) -> xaft_tui::approval::PendingApproval {
    xaft_tui::approval::PendingApproval::new("id", "run", tool, input)
}

#[test]
fn risk_levels_are_ordered_by_declaration() {
    // The bulk-approve threshold is a `<=` comparison, so this ordering is
    // load-bearing, not cosmetic.
    assert!(RiskLevel::Low < RiskLevel::Medium);
    assert!(RiskLevel::Medium < RiskLevel::High);
    assert!(RiskLevel::High < RiskLevel::Critical);
}

// ── approve_all_up_to ───────────────────────────────────────────────────────

#[test]
fn approve_all_up_to_approves_only_at_or_below_the_threshold() {
    let mut q = manual_queue();
    let low = pending_with_risk("read_file", low_risk_input());
    let high = pending_with_risk("run_command", json!({"command": "rm -rf /tmp/x"}));
    let (low_id, high_id) = (low.tool_use_id.clone(), high.tool_use_id.clone());
    let (low_risk, high_risk) = (low.risk, high.risk);
    assert!(
        low_risk < high_risk,
        "fixture must span two risk levels ({low_risk:?} vs {high_risk:?})"
    );
    q.pending.push_back(low);
    q.pending.push_back(high);

    let approved = q.approve_all_up_to(low_risk);

    let ids: Vec<&str> = approved.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec![low_id], "only the low-risk item may be approved");
    assert_eq!(approved[0].1, ApprovalDecision::Approved);
    // The high-risk item must survive for the user to decide.
    assert!(q.has_pending(), "the above-threshold item must remain queued");
    assert_eq!(q.pending.back().unwrap().tool_use_id, high_id);
}

#[test]
fn approve_all_up_to_returns_every_approved_id_exactly_once() {
    // The contract that matters to the caller is the *set*: every item at or
    // below the threshold is returned once, each paired with Approved, so the
    // gate can be told about each one. Order is not part of that contract —
    // the implementation drains indices in reverse to keep them valid, which
    // makes the result newest-first (pinned by the next test).
    let mut q = manual_queue();
    for i in 0..3 {
        let mut item = pending_with_risk("read_file", low_risk_input());
        item.tool_use_id = format!("id-{i}");
        q.pending.push_back(item);
    }

    let approved = q.approve_all_up_to(RiskLevel::Critical);

    let mut ids: Vec<&str> = approved.iter().map(|(id, _)| id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["id-0", "id-1", "id-2"]);
    assert!(
        approved
            .iter()
            .all(|(_, d)| *d == ApprovalDecision::Approved),
        "every returned decision must be Approved"
    );
    assert!(!q.has_pending(), "everything was approved");
}

#[test]
fn approve_all_up_to_drains_in_reverse_and_therefore_reports_newest_first() {
    // Documents current behaviour rather than endorsing it: the reverse
    // drain is required to keep the pending indices valid, and it leaks into
    // the returned order. Callers forward by id, so this is cosmetic — but it
    // is pinned so that a future reordering shows up as an intentional change
    // instead of a surprise.
    let mut q = manual_queue();
    for i in 0..3 {
        let mut item = pending_with_risk("read_file", low_risk_input());
        item.tool_use_id = format!("id-{i}");
        q.pending.push_back(item);
    }

    let approved = q.approve_all_up_to(RiskLevel::Critical);

    let ids: Vec<&str> = approved.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["id-2", "id-1", "id-0"]);
}

#[test]
fn approve_all_up_to_high_threshold_empties_the_queue() {
    let mut q = manual_queue();
    q.pending.push_back(pending_with_risk("read_file", low_risk_input()));
    q.pending
        .push_back(pending_with_risk("run_command", json!({"command": "rm -rf /tmp/x"})));

    let approved = q.approve_all_up_to(RiskLevel::Critical);

    assert_eq!(approved.len(), 2);
    assert!(!q.has_pending());
    assert!(q.focused().is_none(), "focus must reset after the drain");
}

#[test]
fn approve_all_up_to_on_an_empty_queue_is_a_no_op() {
    let mut q = manual_queue();
    assert!(q.approve_all_up_to(RiskLevel::Critical).is_empty());
    assert!(!q.has_pending());
}

#[test]
fn approve_all_up_to_counts_the_approvals() {
    let mut q = manual_queue();
    for _ in 0..3 {
        q.pending.push_back(pending_with_risk("read_file", low_risk_input()));
    }

    q.approve_all_up_to(RiskLevel::Low);

    assert_eq!(q.total_approved, 3, "stats must reflect the bulk decision");
    assert_eq!(q.total_rejected, 0);
}

// ── reject_all ─────────────────────────────────────────────────────────────

#[test]
fn reject_all_returns_every_pending_id_and_drains_the_queue() {
    let mut q = manual_queue();
    for i in 0..3 {
        let mut item = pending_with_risk("read_file", low_risk_input());
        item.tool_use_id = format!("id-{i}");
        q.pending.push_back(item);
    }

    let rejected = q.reject_all();

    assert_eq!(rejected, vec!["id-0", "id-1", "id-2"]);
    assert!(!q.has_pending(), "rejecting must drain the queue");
    assert!(q.focused().is_none());
}

#[test]
fn reject_all_counts_rejections_not_approvals() {
    let mut q = manual_queue();
    q.pending.push_back(pending_with_risk("read_file", low_risk_input()));
    q.pending
        .push_back(pending_with_risk("run_command", json!({"command": "rm -rf /tmp/x"})));

    q.reject_all();

    assert_eq!(q.total_rejected, 2);
    assert_eq!(q.total_approved, 0);
}

#[test]
fn reject_all_on_an_empty_queue_is_a_no_op() {
    let mut q = manual_queue();
    assert!(q.reject_all().is_empty());
    assert!(!q.has_pending());
}

#[test]
fn reject_all_rejects_low_risk_items_too() {
    // A low-risk item only reaches `pending` because the manual queue has no
    // auto-approve rules; "reject all" means all, and must not filter by risk.
    let mut q = manual_queue();
    q.pending.push_back(pending_with_risk("read_file", low_risk_input()));

    let rejected = q.reject_all();

    assert_eq!(rejected.len(), 1);
    assert_eq!(q.total_rejected, 1);
}

// ── focus movement ──────────────────────────────────────────────────────────

/// Push `n` items with distinct ids onto a manual queue.
fn queue_of(n: usize) -> (ApprovalQueue, Vec<String>) {
    let mut q = manual_queue();
    let mut ids = Vec::new();
    for i in 0..n {
        let mut item = pending_with_risk("read_file", low_risk_input());
        let id = format!("id-{i}");
        item.tool_use_id = id.clone();
        ids.push(id);
        q.pending.push_back(item);
    }
    (q, ids)
}

#[test]
fn focus_next_wraps_around() {
    let (mut q, ids) = queue_of(3);
    assert_eq!(q.focused().unwrap().tool_use_id, ids[0]);
    q.focus_next();
    assert_eq!(q.focused().unwrap().tool_use_id, ids[1]);
    q.focus_next();
    assert_eq!(q.focused().unwrap().tool_use_id, ids[2]);
    q.focus_next();
    assert_eq!(
        q.focused().unwrap().tool_use_id,
        ids[0],
        "focus must wrap, not run off the end"
    );
}

#[test]
fn focus_prev_wraps_backwards() {
    let (mut q, ids) = queue_of(3);
    q.focus_prev();
    assert_eq!(
        q.focused().unwrap().tool_use_id,
        ids[2],
        "focus_prev from the first item wraps to the last"
    );
    q.focus_prev();
    assert_eq!(q.focused().unwrap().tool_use_id, ids[1]);
}

#[test]
fn focus_movement_on_an_empty_queue_does_not_panic() {
    let mut q = manual_queue();
    q.focus_next();
    q.focus_prev();
    assert!(q.focused().is_none());
    assert!(!q.has_pending());
}

// ── ToolPreview::content_height ─────────────────────────────────────────────

#[test]
fn bash_preview_has_a_fixed_height() {
    // `bash_exec` is the tool name `from_input` matches on — `run_command`
    // falls through to the `Generic` arm, which would make this assertion
    // pass for the wrong reason.
    let p = ToolPreview::from_input("bash_exec", &json!({"command": "ls -la"}));
    assert!(matches!(p, ToolPreview::Bash { .. }), "got {p:?}");
    assert_eq!(p.content_height(), 5);
}

#[test]
fn a_non_bash_tool_name_does_not_produce_a_bash_preview() {
    // Pins the classification boundary: only `bash_exec` is Bash.
    let p = ToolPreview::from_input("run_command", &json!({"command": "ls -la"}));
    assert!(
        matches!(p, ToolPreview::Generic { .. }),
        "run_command must fall through to Generic, got {p:?}"
    );
}

#[test]
fn read_preview_is_shorter_than_bash() {
    let p = ToolPreview::from_input("read_file", &low_risk_input());
    assert!(matches!(p, ToolPreview::FileRead { .. }), "got {p:?}");
    assert_eq!(p.content_height(), 3);
}

#[test]
fn edit_preview_height_grows_with_the_diff_but_is_capped() {
    // `edit_file` reads `old_content`/`new_content`; using `old_string`
    // yields empty line vectors and a header-only preview.
    let small = ToolPreview::from_input(
        "edit_file",
        &json!({"path": "a.rs", "old_content": "x\ny", "new_content": "x\nz"}),
    );
    assert!(matches!(small, ToolPreview::FileEdit { .. }), "got {small:?}");
    let h_small = small.content_height();
    assert_eq!(h_small, 4 + 2, "header plus the two-line diff body");

    // A large diff must not make the dialog unbounded: the height is capped
    // at 10 diff lines.
    let big_body: String = (0..50).map(|i| format!("line {i}\n")).collect();
    let big = ToolPreview::from_input(
        "edit_file",
        &json!({"path": "a.rs", "old_content": big_body, "new_content": big_body}),
    );
    assert_eq!(big.content_height(), 4 + 10, "diff height must cap at 10");
    assert!(big.content_height() > h_small);
}

#[test]
fn write_preview_height_grows_with_content_but_is_capped() {
    let short = ToolPreview::from_input("write_file", &json!({"path": "a", "content": "hi"}));
    let h_short = short.content_height();
    assert_eq!(h_short, 4 + 1, "header plus one content line");

    // Height tracks *lines*, not bytes: a single very long line stays one row.
    let long_line = "x".repeat(5_000);
    let still_one =
        ToolPreview::from_input("write_file", &json!({"path": "a", "content": long_line}));
    assert_eq!(
        still_one.content_height(),
        h_short,
        "a long single line must not inflate the height"
    );

    let many_lines: String = (0..50).map(|i| format!("line {i}\n")).collect();
    let tall = ToolPreview::from_input("write_file", &json!({"path": "a", "content": many_lines}));
    assert!(
        tall.content_height() > h_short,
        "a multi-line preview must be taller"
    );
    assert_eq!(tall.content_height(), 4 + 8, "content height must cap at 8");
}

#[test]
fn generic_preview_height_tracks_the_line_count() {
    let p = ToolPreview::Generic { lines: vec![] };
    assert_eq!(p.content_height(), 2, "a header-only generic preview");

    let many = ToolPreview::Generic {
        lines: (0..100).map(|i| format!("l{i}")).collect(),
    };
    assert_eq!(many.content_height(), 2 + 8, "generic height must cap at 8");
}

#[test]
fn every_preview_variant_reports_a_positive_height() {
    // Guards against a new variant being added to the enum without a
    // `content_height` arm: this asserts over the variants we can reach.
    for p in [
        ToolPreview::from_input("bash_exec", &json!({"command": "ls"})),
        ToolPreview::from_input("read_file", &low_risk_input()),
        ToolPreview::Generic { lines: vec!["a".into()] },
    ] {
        assert!(p.content_height() > 0, "preview must occupy rows: {p:?}");
    }
}
