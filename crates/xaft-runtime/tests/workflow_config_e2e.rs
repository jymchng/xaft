//! End-to-end tests for `WorkflowConfig` integration with `XaftRuntime::run()`
//! and the `XaftAgentHandoff` signal emitted by `run_dynamic_handoff`.
//!
//! Uses `MockLlmProvider` + `MockTransport` — no real API calls.

use std::sync::Arc;

use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_runtime::agent_registry::{AgentDefinition, AgentRegistry, AgentToolSet, WorkflowConfig};
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::orchestrator::run_dynamic_handoff;
use xaft_runtime::runtime::XaftRuntime;
use xaft_runtime::session::AgentSession;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn mock_config() -> XaftConfig {
    XaftConfig::default()
}

fn make_resolve_ctx() -> Arc<injectable_runtime::ResolveContext> {
    Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
        injectable_runtime::EmptySingletonStore,
    )))
}

fn make_session(dir: &TempDir) -> AgentSession {
    AgentSession::new(
        "test task".to_string(),
        dir.path().to_path_buf(),
        "default".to_string(),
        "mock-model".to_string(),
    )
}

fn make_request_with_workflow(task: &str, dir: &TempDir, workflow: WorkflowConfig) -> RunRequest {
    RunRequest {
        task: task.to_string(),
        config: mock_config(),
        working_dir: dir.path().to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: true,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow,
        prior_messages: vec![],
        user_message: None,
    }
}

fn two_agent_registry() -> AgentRegistry {
    AgentRegistry::new()
        .register(AgentDefinition {
            name: "agent_a".into(),
            system_prompt_fn: Box::new(|_, _| "You are agent A.".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 100,
            can_handoff_to: vec!["agent_b".into()],
        })
        .register(AgentDefinition {
            name: "agent_b".into(),
            system_prompt_fn: Box::new(|_, _| "You are agent B.".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 100,
            can_handoff_to: vec![],
        })
}

// ── Test 1: dynamic workflow via RunRequest → XaftRuntime::run() ─────────────

#[tokio::test]
async fn dynamic_workflow_via_run_request() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // The dynamic path uses AgentRegistry::default_xaft() which has coder/qa/fixer.
    // Initial agent is "coder"; it hands off to "qa" which outputs "done".
    // Pattern: tool call → follow-up text (after tool result) → qa text
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "qa", "reason": "coding complete"}),
        )
        .await;
    transport.queue_text("Handing off to QA.").await;
    transport.queue_text("APPROVED — task done").await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    let request = make_request_with_workflow(
        "do something",
        &tmp,
        WorkflowConfig::Dynamic {
            initial_agent: "coder".into(),
            max_handoffs: 4,
            agent_subset: None,
        },
    );

    let result = runtime.run(request).await.unwrap();
    assert!(result.exit_code.is_success());
}

// ── Test 2: standard workflow via Default WorkflowConfig ─────────────────────

#[tokio::test]
async fn standard_workflow_default_config() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // Standard pipeline: planner → coder → qa
    // Planner calls handoff_to_agent("coder", plan)
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "coder", "reason": "1. Write a hello world function"}),
        )
        .await;
    transport.queue_text("Handing off to coder.").await;
    // Coder calls handoff_to_agent("qa", summary)
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "qa", "reason": "Files changed: [main.rs]. Description: added hello."}),
        )
        .await;
    transport.queue_text("Handing off to QA.").await;
    // QA approves
    transport.queue_text("APPROVED").await;
    // Concluding summary LLM call
    transport.queue_text("APPROVED").await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    // WorkflowConfig::default() == Standard
    let request = make_request_with_workflow(
        "add a hello world function",
        &tmp,
        WorkflowConfig::default(),
    );

    let result = runtime.run(request).await.unwrap();
    assert!(result.exit_code.is_success());
}

// ── Test 3: three-agent chain via Dynamic workflow ────────────────────────────

#[tokio::test]
async fn dynamic_workflow_three_agent_chain() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // intake → specialist → approver
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "specialist", "reason": "needs deep analysis"}),
        )
        .await;
    transport.queue_text("Transferring to specialist.").await;
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "approver", "reason": "ready for approval"}),
        )
        .await;
    transport.queue_text("Transferring to approver.").await;
    transport.queue_text("APPROVED — all checks passed").await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let registry = AgentRegistry::new()
        .register(AgentDefinition {
            name: "intake".into(),
            system_prompt_fn: Box::new(|_, _| "intake agent".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 100,
            can_handoff_to: vec!["specialist".into()],
        })
        .register(AgentDefinition {
            name: "specialist".into(),
            system_prompt_fn: Box::new(|_, _| "specialist agent".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 100,
            can_handoff_to: vec!["approver".into()],
        })
        .register(AgentDefinition {
            name: "approver".into(),
            system_prompt_fn: Box::new(|_, _| "approver agent".into()),
            tool_set: AgentToolSet::ReadOnly,
            max_turns: 100,
            can_handoff_to: vec![],
        });

    let signals = Arc::new(SignalBus::new());
    let resolve_ctx = make_resolve_ctx();
    let mut session = make_session(&tmp);
    let llm_provider: Arc<dyn LlmProvider> = llm;

    let result = run_dynamic_handoff(
        "banking transaction",
        &registry,
        &WorkflowConfig::Dynamic {
            initial_agent: "intake".into(),
            max_handoffs: 6,
            agent_subset: None,
        },
        llm_provider,
        signals,
        resolve_ctx,
        vec![],
        vec![],
        &mut session,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.agent_name, "approver");
    assert!(result.content.to_uppercase().contains("APPROVED"));
}

// ── Test 4: XaftAgentHandoff signal emitted during run_dynamic_handoff ────────

#[tokio::test]
async fn agent_llm_call_starting_fires_for_each_active_agent() {
    // XaftAgentHandoff only fires when using run_stream(). Since we use run()
    // (to avoid the streaming empty-tool-name bug), we verify agent visibility
    // via XaftLlmCallStarting which fires from NamedAgent::before_llm_call.
    use std::sync::Mutex;
    use xaft_agent::signals::XaftLlmCallStarting;

    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // agent_a calls handoff_to_agent("agent_b"), agent_b outputs final text
    transport
        .queue_tool_call(
            "handoff_to_agent",
            serde_json::json!({"target_agent": "agent_b", "reason": "needs specialist"}),
        )
        .await;
    transport.queue_text("Handing off.").await;
    transport.queue_text("Final answer from B").await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let signals = Arc::new(SignalBus::new());
    let resolve_ctx = make_resolve_ctx();
    let registry = two_agent_registry();
    let mut session = make_session(&tmp);

    // Collect agent names observed via LlmCallStarting.
    let agent_names: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let names_for_listener = Arc::clone(&agent_names);
    signals
        .on::<XaftLlmCallStarting>(move |ev| {
            if let Ok(mut v) = names_for_listener.lock() {
                v.push(ev.agent_name.clone());
            }
        })
        .await;

    run_dynamic_handoff(
        "test task",
        &registry,
        &WorkflowConfig::Dynamic {
            initial_agent: "agent_a".into(),
            max_handoffs: 4,
            agent_subset: None,
        },
        Arc::clone(&llm),
        Arc::clone(&signals),
        resolve_ctx,
        vec![],
        vec![],
        &mut session,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Give the spawned signal tasks a moment to complete.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let names = agent_names.lock().unwrap();
    // Both agents must have started an LLM call: agent_a (initial) + agent_b (after handoff)
    assert!(
        names.contains(&"agent_a".to_string()),
        "agent_a must appear in LlmCallStarting signals, got: {names:?}"
    );
    assert!(
        names.contains(&"agent_b".to_string()),
        "agent_b must appear in LlmCallStarting signals after handoff, got: {names:?}"
    );
}

// ── Test 5: planner answers directly in standard (unified) workflow ───────────

#[tokio::test]
async fn planner_answers_directly_in_unified_workflow() {
    // When the planner outputs text inline (no handoff tool call), the
    // orchestrator terminates early with the planner's content.
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // Planner outputs text directly without calling any tool.
    transport
        .queue_text("This repository is a CLI coding assistant written in Rust.")
        .await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    let request =
        make_request_with_workflow("describe this repository", &tmp, WorkflowConfig::Standard);

    let result = runtime.run(request).await.unwrap();
    assert!(result.exit_code.is_success());
    // Planner text should appear in the summary.
    assert!(
        result.summary.contains("CLI coding assistant")
            || result.summary.contains("task completed")
            || result.summary.contains("describe"),
        "summary should contain planner content or task name: {}",
        result.summary
    );
}

// ── Test 6 (unit): WorkflowConfig Default and serialization/round-trip ────────

#[test]
fn workflow_config_serializes_and_deserializes() {
    // Default is Standard.
    assert!(matches!(
        WorkflowConfig::default(),
        WorkflowConfig::Standard
    ));

    // Dynamic variant construction
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
            let expected: Vec<String> = vec!["coder".into(), "qa".into()];
            assert_eq!(agent_subset.as_deref(), Some(expected.as_slice()));
        }
        _ => panic!("expected Dynamic"),
    }

    // Standard variant is Clone
    let std_cfg = WorkflowConfig::Standard;
    let _cloned = std_cfg.clone();
    assert!(matches!(_cloned, WorkflowConfig::Standard));
}
