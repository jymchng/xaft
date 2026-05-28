//! End-to-end integration tests for `xaft-agents`.
//!
//! Uses `MockLlmProvider` + `MockTransport` from `agtrs_runtime::testing` — no
//! real API calls. Tests cover the full agent lifecycle:
//!   registry → build_agent → handoff → signal emission

use std::sync::Arc;

use agtrs_runtime::agent::Agent;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::HandoffAgentStore;
use agtrs_runtime::tool::Tool;

use xaft_agents::coder::{CODER_NAME, EditSummary};
use xaft_agents::fixer::FIXER_NAME;
use xaft_agents::handoff::HandoffTool;
use xaft_agents::named::NamedAgent;
use xaft_agents::planner::{PLANNER_NAME, PlanResult, PlannerOutput, parse_plan_result};
use xaft_agents::qa::{QA_NAME, RequestFixTool};
use xaft_agents::registry::{AgentDefinition, AgentRegistry, AgentToolSet, WorkflowConfig};

// ── Helper ────────────────────────────────────────────────────────────────────

fn dummy_store() -> Arc<HandoffAgentStore> {
    Arc::new(HandoffAgentStore::new())
}

fn dummy_signals() -> Arc<SignalBus> {
    Arc::new(SignalBus::new())
}

// ── NamedAgent ────────────────────────────────────────────────────────────────

#[test]
fn named_agent_new_has_correct_name_and_turns() {
    let a = NamedAgent::new("coder", "you are a coder", 20);
    assert_eq!(a.name(), "coder");
    assert_eq!(a.config().max_turns, 20);
    assert!(a.tools().is_empty());
}

#[test]
fn named_agent_system_prompt() {
    let a = NamedAgent::new("qa", "you are a reviewer", 10);
    assert_eq!(a.system_prompt(), "you are a reviewer");
}

#[tokio::test]
async fn named_agent_before_llm_call_ok_even_when_flag_set() {
    use std::sync::atomic::AtomicBool;
    // HandoffOrchestrator needs one more LLM call after tool result,
    // so before_llm_call must not error — the orchestrator handles termination.
    let flag = Arc::new(AtomicBool::new(true));
    let agent = NamedAgent::new("coder", "code", 10).with_handoff_flag(Arc::clone(&flag));
    let mut msgs = vec![];
    let mut opts = agtrs_runtime::llm::LlmOptions::default();
    let result = agent.before_llm_call(&mut msgs, &mut opts).await;
    assert!(result.is_ok());
}

// ── AgentRegistry ─────────────────────────────────────────────────────────────

#[test]
fn registry_starts_empty() {
    let r = AgentRegistry::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
}

#[test]
fn register_and_lookup() {
    let r = AgentRegistry::new().register(AgentDefinition {
        name: "my_agent".into(),
        system_prompt_fn: Box::new(|_, _| "You are my agent.".into()),
        tool_set: AgentToolSet::ReadOnly,
        max_turns: 5,
        can_handoff_to: vec![],
    });
    assert_eq!(r.len(), 1);
    assert!(r.get("my_agent").is_some());
    assert!(r.get("unknown").is_none());
}

#[test]
fn default_xaft_has_four_agents() {
    let r = AgentRegistry::default_xaft();
    assert_eq!(r.len(), 4);
    assert!(r.get(PLANNER_NAME).is_some());
    assert!(r.get(CODER_NAME).is_some());
    assert!(r.get(QA_NAME).is_some());
    assert!(r.get(FIXER_NAME).is_some());
}

#[test]
fn registry_preserves_insertion_order() {
    let r = AgentRegistry::new()
        .register(AgentDefinition {
            name: "a".into(),
            system_prompt_fn: Box::new(|_, _| String::new()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 1,
            can_handoff_to: vec![],
        })
        .register(AgentDefinition {
            name: "b".into(),
            system_prompt_fn: Box::new(|_, _| String::new()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 1,
            can_handoff_to: vec![],
        });
    assert_eq!(r.agent_names(), &["a", "b"]);
}

#[test]
fn build_unknown_agent_returns_error() {
    let r = AgentRegistry::new();
    let result = r.build_agent(
        "ghost",
        "task",
        "/tmp",
        &[],
        &[],
        dummy_store(),
        dummy_signals(),
    );
    assert!(result.is_err());
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("ghost"));
}

#[test]
fn build_agent_injects_handoff_tool() {
    let r = AgentRegistry::new().register(AgentDefinition {
        name: "reviewer".into(),
        system_prompt_fn: Box::new(|_, _| "review code".into()),
        tool_set: AgentToolSet::ReadOnly,
        max_turns: 5,
        can_handoff_to: vec!["fixer".into()],
    });
    let agent = r
        .build_agent(
            "reviewer",
            "task",
            "/tmp",
            &[],
            &[],
            dummy_store(),
            dummy_signals(),
        )
        .unwrap();
    assert!(agent.tools().iter().any(|t| t.name() == "handoff_to_agent"));
}

// ── HandoffTool ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn handoff_tool_writes_to_store() {
    let store = dummy_store();
    let tool = HandoffTool::new(Arc::clone(&store), vec!["fixer".into()]);
    let mut ctx = agtrs_runtime::tool::ToolContext::new("tid-1");
    ctx.state
        .insert("conversation_id".into(), serde_json::json!("conv-1"));

    let result = tool
        .call(
            serde_json::json!({"target_agent": "fixer", "reason": "found bugs"}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(
        store.get_active_agent("conv-1").await,
        Some("fixer".to_string())
    );
}

#[tokio::test]
async fn handoff_tool_rejects_disallowed_target() {
    let store = dummy_store();
    let tool = HandoffTool::new(Arc::clone(&store), vec!["fixer".into()]);
    let mut ctx = agtrs_runtime::tool::ToolContext::new("tid-2");
    ctx.state
        .insert("conversation_id".into(), serde_json::json!("conv-2"));

    let result = tool
        .call(
            serde_json::json!({"target_agent": "evil_agent", "reason": "escape"}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(!result.is_error, "returns ok with rejection message");
    assert!(result.content.contains("not permitted"));
    assert_eq!(store.get_active_agent("conv-2").await, None);
}

#[tokio::test]
async fn handoff_tool_empty_targets_permits_any() {
    let store = dummy_store();
    let tool = HandoffTool::new(Arc::clone(&store), vec![]);
    let mut ctx = agtrs_runtime::tool::ToolContext::new("tid-3");
    ctx.state
        .insert("conversation_id".into(), serde_json::json!("conv-3"));

    let result = tool
        .call(
            serde_json::json!({"target_agent": "any_agent", "reason": "reason"}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(
        store.get_active_agent("conv-3").await,
        Some("any_agent".to_string())
    );
}

#[tokio::test]
async fn handoff_tool_sets_flag() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = dummy_store();
    let flag = Arc::new(AtomicBool::new(false));
    let tool =
        HandoffTool::new_with_flag(Arc::clone(&store), vec!["coder".into()], Arc::clone(&flag));
    let mut ctx = agtrs_runtime::tool::ToolContext::new("tid-flag");
    ctx.state
        .insert("conversation_id".into(), serde_json::json!("conv-flag"));

    assert!(!flag.load(Ordering::Acquire));
    tool.call(
        serde_json::json!({"target_agent": "coder", "reason": "plan ready"}),
        &ctx,
    )
    .await
    .unwrap();
    assert!(flag.load(Ordering::Acquire));
}

// ── RequestFixTool ────────────────────────────────────────────────────────────

#[tokio::test]
async fn request_fix_writes_to_store() {
    let store = dummy_store();
    let tool = RequestFixTool::new(Arc::clone(&store));
    let mut ctx = agtrs_runtime::tool::ToolContext::new("tid-fix");
    ctx.state
        .insert("conversation_id".into(), serde_json::json!("conv-fix"));

    let result = tool
        .call(
            serde_json::json!({"summary": "missing error handling"}),
            &ctx,
        )
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(
        store.get_active_agent("conv-fix").await,
        Some(FIXER_NAME.to_string())
    );
    assert_eq!(
        store.get_and_clear_summary("conv-fix").await,
        Some("missing error handling".to_string())
    );
}

#[tokio::test]
async fn request_fix_empty_conv_id_no_panic() {
    let store = dummy_store();
    let tool = RequestFixTool::new(Arc::clone(&store));
    let ctx = agtrs_runtime::tool::ToolContext::new("tid-noc");

    let result = tool
        .call(serde_json::json!({"summary": "issues"}), &ctx)
        .await
        .unwrap();
    assert!(!result.is_error);
}

// ── EditSummary ───────────────────────────────────────────────────────────────

#[test]
fn edit_summary_serialises_roundtrip() {
    let summary = EditSummary {
        files_changed: vec!["src/main.rs".into()],
        description: "Added error handling".into(),
        tests_passed: true,
        notes: "all green".into(),
    };
    let json = serde_json::to_string(&summary).unwrap();
    let parsed: EditSummary = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.files_changed.len(), 1);
    assert!(parsed.tests_passed);
}

#[test]
fn edit_summary_defaults_on_missing_fields() {
    let json = r#"{"files_changed":[],"description":"done"}"#;
    let parsed: EditSummary = serde_json::from_str(json).unwrap();
    assert!(!parsed.tests_passed);
    assert!(parsed.notes.is_empty());
}

// ── PlanResult / parse_plan_result ────────────────────────────────────────────

#[test]
fn parse_numbered_list_is_coding_plan() {
    let raw = "1. Read src/main.rs\n2. Add error handling\n3. Run tests";
    let result = parse_plan_result("add error handling", raw);
    assert!(matches!(result, PlanResult::CodingPlan { .. }));
}

#[test]
fn parse_json_direct_answer() {
    let raw = r#"{"task_type":"direct_answer","content":"This repo does X."}"#;
    let result = parse_plan_result("describe repo", raw);
    if let PlanResult::DirectAnswer { content } = result {
        assert!(content.contains("This repo"));
    } else {
        panic!("expected DirectAnswer");
    }
}

#[test]
fn parse_json_coding_plan() {
    let raw = r#"{"task_type":"coding_plan","content":"1. Modify main.rs"}"#;
    let result = parse_plan_result("add feature", raw);
    assert!(matches!(result, PlanResult::CodingPlan { .. }));
}

#[test]
fn parse_prose_is_direct_answer() {
    let raw = "This repository implements a CLI coding assistant in Rust.";
    let result = parse_plan_result("describe repo", raw);
    assert!(matches!(result, PlanResult::DirectAnswer { .. }));
}

#[test]
fn parse_empty_falls_to_coding_with_task() {
    let result = parse_plan_result("fix the bug", "");
    match result {
        PlanResult::CodingPlan { plan_text } => assert_eq!(plan_text, "fix the bug"),
        _ => panic!("expected CodingPlan"),
    }
}

// ── PlannerOutput ─────────────────────────────────────────────────────────────

#[test]
fn planner_output_serialises_roundtrip() {
    let output = PlannerOutput {
        task_type: "coding_plan".into(),
        content: "1. Fix auth\n2. Add tests".into(),
    };
    let json = serde_json::to_string(&output).unwrap();
    let parsed: PlannerOutput = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.task_type, "coding_plan");
}

// ── WorkflowConfig ────────────────────────────────────────────────────────────

#[test]
fn workflow_config_default_is_standard() {
    assert!(matches!(
        WorkflowConfig::default(),
        WorkflowConfig::Standard
    ));
}

#[test]
fn workflow_config_dynamic_construction() {
    let cfg = WorkflowConfig::Dynamic {
        initial_agent: "coder".into(),
        max_handoffs: 5,
        agent_subset: Some(vec!["coder".into(), "qa".into()]),
    };
    match cfg {
        WorkflowConfig::Dynamic {
            ref initial_agent,
            max_handoffs,
            ref agent_subset,
        } => {
            assert_eq!(initial_agent, "coder");
            assert_eq!(max_handoffs, 5);
            assert_eq!(
                agent_subset.as_deref(),
                Some(["coder".into(), "qa".into()].as_slice())
            );
        }
        _ => panic!("expected Dynamic"),
    }
}
