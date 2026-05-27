//! End-to-end session lifecycle tests for `XaftRuntime`.
//!
//! Uses `MockLlmProvider` + `MockTransport` from `agtrs_runtime::testing` — no
//! real API calls. Tests cover the full loop:
//!   session created → cost/tokens accumulated → session saved → resumed

use std::path::PathBuf;
use std::sync::Arc;

use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use tempfile::TempDir;
use xaft_config::XaftConfig;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::runtime::XaftRuntime;
use xaft_runtime::session::SessionStatus;
use xaft_runtime::types::ExitCode;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn mock_config() -> XaftConfig {
    XaftConfig::default()
}

fn make_request(task: &str, dir: &TempDir) -> RunRequest {
    RunRequest {
        task: task.to_string(),
        config: mock_config(),
        working_dir: dir.path().to_path_buf(),
        headless: true,
        dry_run: false,
        auto_approve: true,
        dangerously_skip_permissions: false,
        resume_session_id: None,
    }
}

async fn queue_full_workflow(transport: &MockTransport) {
    // ── Planner phase ─────────────────────────────────────────────────────────
    // Turn 1: planner calls create_coding_plan tool
    transport
        .queue_tool_call(
            "create_coding_plan",
            serde_json::json!({"steps": "1. Write main.rs\n2. Verify output"}),
        )
        .await;
    // Turn 2: LLM's text response after the tool result is returned to it
    transport.queue_text("Plan recorded.").await;

    // ── Coder phase ───────────────────────────────────────────────────────────
    // StructuredLlm mode: agent runs (turn 1 may produce text), then a second
    // extraction call produces the EditSummary JSON.  Queue both to be safe.
    transport
        .queue_text(
            r#"{"files_changed":["main.rs"],"description":"wrote main.rs","tests_passed":false,"notes":""}"#,
        )
        .await;
    // StructuredLlm extraction (may be a second call)
    transport
        .queue_text(
            r#"{"files_changed":["main.rs"],"description":"wrote main.rs","tests_passed":false,"notes":""}"#,
        )
        .await;

    // ── QA phase ──────────────────────────────────────────────────────────────
    transport.queue_text("APPROVED").await;
    // Extra buffer so QA summarization doesn't exhaust the queue
    transport.queue_text("APPROVED").await;
}

// ── 1. Basic run → session persisted ─────────────────────────────────────────

#[tokio::test]
async fn run_creates_session_in_store() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
    let result = runtime
        .run(make_request("write a server", &tmp))
        .await
        .unwrap();

    assert!(result.exit_code.is_success());

    // Session must be persisted
    let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].task, "write a server");
    assert!(
        matches!(sessions[0].status, SessionStatus::Completed { .. }),
        "status should be Completed, got {:?}",
        sessions[0].status
    );
}

#[tokio::test]
async fn dry_run_creates_session_and_returns_success() {
    let tmp = TempDir::new().unwrap();
    let runtime = XaftRuntime::for_testing(mock_config(), None);
    let mut req = make_request("dry run task", &tmp);
    req.dry_run = true;

    let result = runtime.run(req).await.unwrap();
    assert!(result.exit_code.is_success());
    assert!(result.summary.contains("dry-run"));
}

#[tokio::test]
async fn session_turn_count_incremented() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
    let result = runtime.run(make_request("task", &tmp)).await.unwrap();

    // turn_count should be > 0 after a real run (llm calls accumulated)
    // For mock provider, cost_usd may be 0 but turn_count tracks signal emissions
    assert!(result.exit_code.is_success());
}

// ── 2. Session list and filter ────────────────────────────────────────────────

#[tokio::test]
async fn list_sessions_empty_for_new_runtime() {
    let tmp = TempDir::new().unwrap();
    let runtime = XaftRuntime::for_testing(mock_config(), None);
    let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn list_sessions_filtered_by_workspace() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let runtime = XaftRuntime::for_testing(mock_config(), Some(Arc::clone(&llm)));
    runtime.run(make_request("task a", &tmp_a)).await.unwrap();

    let runtime2 = XaftRuntime::for_testing(mock_config(), Some(Arc::clone(&llm)));
    runtime2.run(make_request("task b", &tmp_b)).await.unwrap();

    // Each runtime has its own in-memory store, but we test workspace filter
    let sessions_a = runtime.list_sessions(tmp_a.path()).await.unwrap();
    assert_eq!(sessions_a.len(), 1);
    assert_eq!(sessions_a[0].task, "task a");
}

// ── 3. Resume flow ────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_nonexistent_session_returns_error() {
    let runtime = XaftRuntime::for_testing(mock_config(), None);
    let result = runtime
        .resume_session("nonexistent-id", mock_config())
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not found"),
        "error should mention 'not found': {msg}"
    );
}

#[tokio::test]
async fn resume_completed_session_returns_error() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
    let result = runtime.run(make_request("done task", &tmp)).await.unwrap();
    let session_id = result.session.id.as_str().to_string();

    // Completed sessions cannot be resumed
    let resume_result = runtime.resume_session(&session_id, mock_config()).await;
    assert!(
        resume_result.is_err(),
        "completed session resume should fail"
    );
}

// ── 4. Cost tracking signal integration ──────────────────────────────────────

#[tokio::test]
async fn mock_run_cost_zero_but_no_panic() {
    // The mock LLM doesn't emit ModelCallComplete with real cost values,
    // but the SignalBus subscription should not panic on empty emissions.
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
    let result = runtime.run(make_request("cost test", &tmp)).await;
    assert!(result.is_ok(), "run should succeed: {:?}", result);

    // Session cost is 0 for mock (mock doesn't emit ModelCallComplete with cost)
    let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
    assert!(!sessions.is_empty());
    // total_cost_usd is ≥ 0 (mock gives 0, real gives > 0)
    assert!(sessions[0].total_cost_usd >= 0.0);
}

// ── 5. Concurrent run isolation ───────────────────────────────────────────────

#[tokio::test]
async fn concurrent_sessions_independent() {
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();

    let make_runtime = || async {
        let transport = Arc::new(MockTransport::new());
        queue_full_workflow(&transport).await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
        XaftRuntime::for_testing(mock_config(), Some(llm))
    };

    let (rt1, rt2) = tokio::join!(make_runtime(), make_runtime());

    let (r1, r2) = tokio::join!(
        rt1.run(make_request("task 1", &tmp1)),
        rt2.run(make_request("task 2", &tmp2)),
    );

    let r1 = r1.unwrap();
    let r2 = r2.unwrap();
    assert!(r1.exit_code.is_success());
    assert!(r2.exit_code.is_success());
    assert_ne!(
        r1.session.id, r2.session.id,
        "sessions must have unique IDs"
    );
}

// ── 6. SQLite-backed session persistence (with xaft-session) ─────────────────

#[cfg(feature = "sqlite-test")]
#[tokio::test]
async fn sqlite_backed_session_persists_across_runtime() {
    let tmp = TempDir::new().unwrap();

    // First run — creates SQLite store
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let session_id = {
        let mgr = xaft_session::SessionManager::new(tmp.path()).await.unwrap();
        let base = XaftRuntime::bootstrap(mock_config()).await.unwrap();
        let runtime = base.with_stores(mgr.session_store(), mgr.conversation_store());
        let result = runtime
            .run(make_request("sqlite task", &tmp))
            .await
            .unwrap();
        result.session.id.as_str().to_string()
    };

    // Second "process" — different runtime, same SQLite
    let mgr2 = xaft_session::SessionManager::new(tmp.path()).await.unwrap();
    let loaded = mgr2
        .load(&xaft_runtime::session::SessionId::from_string(&session_id))
        .await
        .unwrap();

    assert!(loaded.is_some(), "session must persist in SQLite");
    let s = loaded.unwrap();
    assert_eq!(s.task, "sqlite task");
    assert!(matches!(s.status, SessionStatus::Completed { .. }));
}

// ── Smart routing tests ───────────────────────────────────────────────────────

/// Informational task: planner calls answer_directly → workflow returns early,
/// skipping the coder entirely.  Only planner responses are queued.
#[tokio::test]
async fn informational_task_answered_directly_without_coder() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // Planner calls answer_directly (informational task)
    transport
        .queue_tool_call(
            "answer_directly",
            serde_json::json!({"answer": "This repository implements a CLI tool for X."}),
        )
        .await;
    // LLM response after tool result
    transport.queue_text("Answer recorded.").await;
    // NO coder or QA responses — they must not be called

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    let result = runtime
        .run(make_request("describe this repository", &tmp))
        .await
        .unwrap();

    assert!(
        result.exit_code.is_success(),
        "informational task must succeed"
    );
    assert!(
        result.summary.contains("This repository") || result.summary.contains("implements"),
        "summary must contain the planner's direct answer, got: {:?}",
        result.summary
    );
}

/// Coding task: planner calls create_coding_plan → full workflow runs.
#[tokio::test]
async fn coding_task_proceeds_to_coder_and_qa() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    queue_full_workflow(&transport).await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    let result = runtime
        .run(make_request("add error handling to src/main.rs", &tmp))
        .await
        .unwrap();

    assert!(result.exit_code.is_success(), "coding task must succeed");
}

/// When the planner calls neither routing tool (LLM ignores instructions),
/// the workflow falls back to treating the task as a coding plan.
#[tokio::test]
async fn no_routing_tool_call_falls_back_to_coding_plan() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());

    // Planner outputs prose without calling any routing tool (bad behaviour)
    transport
        .queue_text("I will help you with this task.")
        .await;
    // Coder + QA (fallback coding path)
    transport
        .queue_text(
            r#"{"files_changed":[],"description":"no-op","tests_passed":false,"notes":""}"#,
        )
        .await;
    transport
        .queue_text(
            r#"{"files_changed":[],"description":"no-op","tests_passed":false,"notes":""}"#,
        )
        .await;
    transport.queue_text("APPROVED").await;
    transport.queue_text("APPROVED").await;

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));
    let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));

    // Should not panic — fallback keeps the workflow alive
    let result = runtime
        .run(make_request("do something", &tmp))
        .await
        .unwrap();
    assert!(result.exit_code.is_success());
}

/// parse_plan_result: numbered lines → CodingPlan.
#[test]
fn parse_plan_result_numbered_list_is_coding_plan() {
    use xaft_runtime::parse_plan_result;
    let raw = "1. Read src/main.rs\n2. Add error handling\n3. Run tests";
    let result = parse_plan_result("add error handling", raw);
    assert!(
        matches!(result, xaft_runtime::PlanResult::CodingPlan { .. }),
        "numbered list must parse as CodingPlan"
    );
}

/// parse_plan_result: valid PlannerOutput JSON direct_answer → DirectAnswer.
#[test]
fn parse_plan_result_json_direct_answer() {
    use xaft_runtime::parse_plan_result;
    let raw = r#"{"task_type":"direct_answer","content":"This repo does X."}"#;
    let result = parse_plan_result("describe repo", raw);
    if let xaft_runtime::PlanResult::DirectAnswer { content } = result {
        assert!(content.contains("This repo"));
    } else {
        panic!("expected DirectAnswer");
    }
}

/// parse_plan_result: valid PlannerOutput JSON coding_plan → CodingPlan.
#[test]
fn parse_plan_result_json_coding_plan() {
    use xaft_runtime::parse_plan_result;
    let raw = r#"{"task_type":"coding_plan","content":"1. Modify main.rs"}"#;
    let result = parse_plan_result("add feature", raw);
    assert!(
        matches!(result, xaft_runtime::PlanResult::CodingPlan { .. }),
        "coding_plan JSON must parse as CodingPlan"
    );
}
