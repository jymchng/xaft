//! Integration tests for the mode system data layer (PRD 64/65).

use xaft_tui::mode::{AgentMode, AgentModeBuilder, ModeColour};
use xaft_tui::{ModeManager, ModeRegistry};

fn make_mode(name: &str, source: &str) -> AgentMode {
    AgentModeBuilder::new(name, name.to_uppercase())
        .source_id(source)
        .build()
}

fn make_req() -> xaft_runtime::RunRequest {
    use std::path::PathBuf;
    xaft_runtime::RunRequest {
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
    }
}

#[test]
fn test_mode_registry_register_and_get() {
    let mut reg = ModeRegistry::new();
    reg.register(make_mode("alpha", "builtin"));
    assert!(reg.get("alpha").is_some());
    assert_eq!(reg.get("alpha").unwrap().name, "alpha");
    assert!(reg.get("beta").is_none());
}

#[test]
fn test_mode_registry_cycle_wraps() {
    let mut reg = ModeRegistry::new();
    reg.register(make_mode("a", "s"));
    reg.register(make_mode("b", "s"));
    reg.register(make_mode("c", "s"));
    let next = reg.next_after("c");
    assert_eq!(next.name, "a");
}

#[test]
fn test_mode_registry_unregister_source() {
    let mut reg = ModeRegistry::new();
    reg.register(make_mode("a", "builtin"));
    reg.register(make_mode("b", "mcp"));
    reg.register(make_mode("c", "builtin"));
    reg.unregister_source("mcp");
    assert_eq!(reg.len(), 2);
    assert!(reg.get("b").is_none());
    assert!(reg.get("a").is_some());
}

#[test]
fn test_mode_registry_replace_preserves_order() {
    let mut reg = ModeRegistry::new();
    reg.register(make_mode("a", "s1"));
    reg.register(make_mode("b", "s1"));
    reg.register(make_mode("c", "s1"));
    // Replace "b" — should stay at index 1
    reg.register(make_mode("b", "s2"));
    assert_eq!(reg.len(), 3);
    assert_eq!(reg.all_modes()[1].name, "b");
    assert_eq!(reg.get("b").unwrap().source_id, "s2");
}

#[test]
fn test_mode_manager_default_is_auto() {
    let mgr = ModeManager::default_builtin();
    assert_eq!(mgr.active_name(), "auto");
}

#[test]
fn test_mode_manager_cycle() {
    let mut mgr = ModeManager::default_builtin();
    let name = mgr.cycle().name.clone();
    assert_ne!(name, "auto");
    assert_eq!(name, "plan");
}

#[test]
fn test_mode_manager_set_by_name() {
    let mut mgr = ModeManager::default_builtin();
    let result = mgr.set("debug");
    assert!(result.is_ok());
    assert_eq!(mgr.active_name(), "debug");
}

#[test]
fn test_mode_manager_set_unknown_returns_err() {
    let mut mgr = ModeManager::default_builtin();
    let result = mgr.set("nonexistent_mode_xyz");
    assert!(result.is_err());
}

#[test]
fn test_mode_manager_apply_system_patch() {
    let mut mgr = ModeManager::default_builtin();
    mgr.set("plan").unwrap();
    let mut req = make_req();
    mgr.apply_to_run_request(&mut req);
    assert!(req.mode_system_patch.is_some());
    let patch = req.mode_system_patch.unwrap();
    assert!(patch.contains("[MODE: PLAN]"));
}

#[test]
fn test_mode_manager_apply_tool_filter() {
    let mut mgr = ModeManager::default_builtin();
    mgr.set("safe").unwrap();
    let mut req = make_req();
    mgr.apply_to_run_request(&mut req);
    let filter = req
        .mode_tool_filter
        .as_ref()
        .expect("safe must have a tool filter");
    assert!(filter("read_file"));
    assert!(!filter("write_file"));
    assert!(!filter("bash_exec"));
}
