//! Integration tests for the 6 built-in modes (PRD 64/65).

use xaft_tui::builtin_modes;

#[test]
fn test_auto_has_no_filter() {
    let modes = builtin_modes();
    let auto = modes.iter().find(|m| m.name == "auto").unwrap();
    assert!(auto.tool_filter.is_none());
}

#[test]
fn test_plan_blocks_write_file() {
    let modes = builtin_modes();
    let plan = modes.iter().find(|m| m.name == "plan").unwrap();
    let f = plan.tool_filter.as_ref().unwrap();
    assert!(!f("write_file"));
}

#[test]
fn test_plan_allows_read_file() {
    let modes = builtin_modes();
    let plan = modes.iter().find(|m| m.name == "plan").unwrap();
    let f = plan.tool_filter.as_ref().unwrap();
    assert!(f("read_file"));
}

#[test]
fn test_plan_allows_git_status() {
    let modes = builtin_modes();
    let plan = modes.iter().find(|m| m.name == "plan").unwrap();
    let f = plan.tool_filter.as_ref().unwrap();
    assert!(f("git_status"));
}

#[test]
fn test_plan_system_patch_contains_must_not() {
    let modes = builtin_modes();
    let plan = modes.iter().find(|m| m.name == "plan").unwrap();
    assert!(plan.system_patch.contains("MUST NOT"));
}

#[test]
fn test_ask_has_no_filter() {
    let modes = builtin_modes();
    let ask = modes.iter().find(|m| m.name == "ask").unwrap();
    assert!(ask.tool_filter.is_none());
}

#[test]
fn test_review_blocks_bash_exec() {
    let modes = builtin_modes();
    let review = modes.iter().find(|m| m.name == "review").unwrap();
    let f = review.tool_filter.as_ref().unwrap();
    assert!(!f("bash_exec"));
}

#[test]
fn test_review_allows_git_diff() {
    let modes = builtin_modes();
    let review = modes.iter().find(|m| m.name == "review").unwrap();
    let f = review.tool_filter.as_ref().unwrap();
    assert!(f("git_diff"));
}

/// Safe is agenthicc's gate-and-ask Safe: nothing is filtered out, and
/// network / execution need explicit approval.
#[test]
fn test_safe_requires_approval_for_read_url() {
    let modes = builtin_modes();
    let safe = modes.iter().find(|m| m.name == "safe").unwrap();
    assert!(safe.tool_filter.is_none(), "Safe hard-blocks nothing");
    let needs = safe.approval_filter.as_ref().unwrap();
    assert!(needs("read_url"));
    assert!(needs("bash_exec"));
}

#[test]
fn test_safe_runs_git_log_directly() {
    let modes = builtin_modes();
    let safe = modes.iter().find(|m| m.name == "safe").unwrap();
    let needs = safe.approval_filter.as_ref().unwrap();
    assert!(!needs("git_log"));
}

/// agenthicc registers no Debug identity; xaft no longer ships one.
#[test]
fn test_there_is_no_debug_mode() {
    let modes = builtin_modes();
    assert!(!modes.iter().any(|m| m.name == "debug"));
}

/// The internal Replay mode blocks every tool and is not selectable.
#[test]
fn test_replay_is_internal_and_blocks_everything() {
    let modes = builtin_modes();
    let replay = modes.iter().find(|m| m.name == "replay").unwrap();
    assert!(!replay.selectable);
    let allowed = replay.tool_filter.as_ref().unwrap();
    assert!(!allowed("read_file"));
}
