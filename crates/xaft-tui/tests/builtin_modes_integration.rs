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

#[test]
fn test_safe_blocks_read_url() {
    let modes = builtin_modes();
    let safe = modes.iter().find(|m| m.name == "safe").unwrap();
    let f = safe.tool_filter.as_ref().unwrap();
    assert!(!f("read_url"));
}

#[test]
fn test_safe_allows_git_log() {
    let modes = builtin_modes();
    let safe = modes.iter().find(|m| m.name == "safe").unwrap();
    let f = safe.tool_filter.as_ref().unwrap();
    assert!(f("git_log"));
}

#[test]
fn test_debug_has_post_hook() {
    let modes = builtin_modes();
    let debug = modes.iter().find(|m| m.name == "debug").unwrap();
    assert!(debug.post_hook.is_some());
}

#[test]
fn test_debug_post_hook_appends_footer() {
    let modes = builtin_modes();
    let debug = modes.iter().find(|m| m.name == "debug").unwrap();
    let hook = debug.post_hook.as_ref().unwrap();
    let result = hook("Agent says hello.");
    assert!(result.starts_with("Agent says hello."));
    assert!(result.contains("[xaft:debug]"));
}
