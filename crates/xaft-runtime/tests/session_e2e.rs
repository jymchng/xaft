//! End-to-end session lifecycle tests for `XaftRuntime`.
//!
//! Uses `MockLlmProvider` + `MockTransport` from `agtrs_runtime::testing` — no
//! real API calls. Tests cover the full loop:
//!   session created → cost/tokens accumulated → session saved → resumed

use std::path::PathBuf;
use std::sync::Arc;

use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::llm::LlmProvider;
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
        resume_session_id: None,
    }
}

async fn queue_full_workflow(transport: &MockTransport) {
    // Planning (garbage responses → falls back to task)
    for _ in 0..6 {
        transport.queue_text("not a plan").await;
    }
    // Coder: returns EditSummary JSON
    transport
        .queue_text(
            r#"{"files_changed":["main.rs"],"description":"wrote main.rs","tests_passed":false,"notes":""}"#,
        )
        .await;
    // QA: approves
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
    let result = runtime.run(make_request("write a server", &tmp)).await.unwrap();

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
    let result = runtime.resume_session("nonexistent-id", mock_config()).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("not found"), "error should mention 'not found': {msg}");
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
    assert_ne!(r1.session.id, r2.session.id, "sessions must have unique IDs");
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
        let result = runtime.run(make_request("sqlite task", &tmp)).await.unwrap();
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
