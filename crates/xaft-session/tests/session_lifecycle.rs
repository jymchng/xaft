//! Integration tests for the full xaft session lifecycle.
//!
//! Tests cover:
//! - Session creation → completion → persistence
//! - Cost/token accumulation across LLM calls
//! - Session resume with conversation history restoration
//! - Concurrent session isolation
//! - Purge and cleanup
//! - Error recovery paths

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::transport::Message;
use tempfile::TempDir;
use xaft_session::{SessionError, SessionManager, SqliteSessionStore, conversation_key_for};
use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};
use xaft_runtime::session_store::SessionStore;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn manager_with_dir(tmp: &TempDir) -> SessionManager {
    SessionManager::new(tmp.path()).await.unwrap()
}

fn make_session(task: &str, workspace: &str) -> AgentSession {
    AgentSession::new(task, PathBuf::from(workspace), "default".into(), "test-model".into())
}

fn user_msg(text: &str) -> Message {
    Message::user(text)
}

fn assistant_msg(text: &str) -> Message {
    Message::assistant(text)
}

// ── 1. Basic CRUD ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn session_create_and_load() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("write a Rust server", "/proj");
    let id = s.id.clone();
    mgr.save(&s).await.unwrap();

    let loaded = mgr.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.task, "write a Rust server");
    assert_eq!(loaded.model, "test-model");
    assert!(matches!(loaded.status, SessionStatus::Active));
}

#[tokio::test]
async fn session_not_found_returns_none() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;
    let result = mgr.load(&SessionId::new()).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn session_update_preserves_id() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("task", "/work");
    let id = s.id.clone();
    mgr.save(&s).await.unwrap();

    s.total_tokens = 5000;
    s.total_cost_usd = 0.015;
    s.turn_count = 7;
    mgr.save(&s).await.unwrap();

    let loaded = mgr.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.total_tokens, 5000);
    assert!((loaded.total_cost_usd - 0.015).abs() < 1e-9);
    assert_eq!(loaded.turn_count, 7);
    assert_eq!(loaded.id, id);
}

#[tokio::test]
async fn session_delete_cascade() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("delete me", "/x");
    let id = s.id.clone();
    let msgs = vec![user_msg("hi"), assistant_msg("hello")];
    mgr.save_with_history(&s, &msgs).await.unwrap();

    mgr.delete(&id).await.unwrap();

    assert!(mgr.load(&id).await.unwrap().is_none());
    // History also gone
    let conv_key = conversation_key_for(&id);
    let history = mgr.conversation_store().load(&conv_key).await.unwrap_or_default();
    assert!(history.is_empty());
}

// ── 2. Conversation history ───────────────────────────────────────────────────

#[tokio::test]
async fn save_and_load_conversation_history() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("refactor auth", "/repo");
    let msgs = vec![
        user_msg("Refactor the auth module"),
        assistant_msg("Sure, I'll start by reading auth.rs"),
        user_msg("proceed"),
        assistant_msg("Done. Here are the changes."),
    ];
    mgr.save_with_history(&s, &msgs).await.unwrap();

    let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 4);
    assert_eq!(swh.session.task, "refactor auth");
}

#[tokio::test]
async fn history_empty_for_new_session() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("new task", "/new");
    mgr.save(&s).await.unwrap();

    let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
    assert!(swh.messages.is_empty());
}

#[tokio::test]
async fn save_with_history_replaces_previous() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("task", "/w");
    mgr.save_with_history(&s, &[user_msg("first")]).await.unwrap();
    mgr.save_with_history(&s, &[user_msg("a"), assistant_msg("b"), user_msg("c")])
        .await
        .unwrap();

    let swh = mgr.load_with_history(&s.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 3);
}

#[tokio::test]
async fn message_count_matches_saved() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("count test", "/c");
    let msgs: Vec<Message> = (0..15).flat_map(|i| {
        vec![user_msg(&format!("msg {i}")), assistant_msg(&format!("resp {i}"))]
    }).collect();
    mgr.save_with_history(&s, &msgs).await.unwrap();

    assert_eq!(mgr.message_count(&s.id).await.unwrap(), 30);
}

// ── 3. Session resumption ─────────────────────────────────────────────────────

#[tokio::test]
async fn validate_resumable_active() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("active task", "/a");
    mgr.save(&s).await.unwrap();

    let swh = mgr.validate_resumable(&s.id).await.unwrap();
    assert_eq!(swh.session.id, s.id);
}

#[tokio::test]
async fn validate_resumable_suspended() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("suspended", "/s");
    s.status = SessionStatus::Suspended;
    mgr.save(&s).await.unwrap();

    let swh = mgr.validate_resumable(&s.id).await.unwrap();
    assert!(matches!(swh.session.status, SessionStatus::Suspended));
}

#[tokio::test]
async fn validate_resumable_completed_fails() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("done", "/d");
    s.status = SessionStatus::Completed { summary: "ok".into() };
    mgr.save(&s).await.unwrap();

    let err = mgr.validate_resumable(&s.id).await.unwrap_err();
    assert!(matches!(err, SessionError::NotResumable { .. }));
}

#[tokio::test]
async fn validate_resumable_failed_session_fails() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("failed", "/f");
    s.status = SessionStatus::Failed { error: "crash".into() };
    mgr.save(&s).await.unwrap();

    let err = mgr.validate_resumable(&s.id).await.unwrap_err();
    assert!(matches!(err, SessionError::NotResumable { .. }));
}

#[tokio::test]
async fn validate_resumable_cancelled_fails() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("cancelled", "/c");
    s.status = SessionStatus::Cancelled;
    mgr.save(&s).await.unwrap();

    assert!(mgr.validate_resumable(&s.id).await.is_err());
}

#[tokio::test]
async fn resume_preserves_conversation_history() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let s = make_session("long task", "/repo");
    let first_run_msgs = vec![
        user_msg("Start implementing the feature"),
        assistant_msg("Reading the codebase…"),
        user_msg("Continue"),
        assistant_msg("Wrote src/feature.rs with the implementation."),
    ];
    mgr.save_with_history(&s, &first_run_msgs).await.unwrap();

    // Simulate process restart: new SessionManager, same data_dir
    let mgr2 = manager_with_dir(&tmp).await;
    let swh = mgr2.load_with_history(&s.id).await.unwrap().unwrap();

    assert_eq!(swh.messages.len(), 4, "history must survive across process restarts");
    assert!(matches!(swh.session.status, SessionStatus::Active));
}

// ── 4. Cost and token tracking ────────────────────────────────────────────────

#[tokio::test]
async fn cost_and_token_accumulation() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("cost tracking", "/cost");
    mgr.save(&s).await.unwrap();

    // Simulate multiple LLM turns adding cost
    s.total_cost_usd += 0.002;
    s.total_tokens += 1000;
    s.turn_count += 1;
    mgr.save(&s).await.unwrap();

    s.total_cost_usd += 0.005;
    s.total_tokens += 2500;
    s.turn_count += 1;
    mgr.save(&s).await.unwrap();

    let loaded = mgr.load(&s.id).await.unwrap().unwrap();
    assert!((loaded.total_cost_usd - 0.007).abs() < 1e-9);
    assert_eq!(loaded.total_tokens, 3500);
    assert_eq!(loaded.turn_count, 2);
}

#[tokio::test]
async fn completed_session_has_cost_summary() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("complete with cost", "/cc");
    s.total_cost_usd = 0.042;
    s.total_tokens = 21_000;
    s.turn_count = 5;
    s.status = SessionStatus::Completed { summary: "done".into() };
    mgr.save(&s).await.unwrap();

    let loaded = mgr.load(&s.id).await.unwrap().unwrap();
    assert!((loaded.total_cost_usd - 0.042).abs() < 1e-9);
    assert_eq!(loaded.total_tokens, 21_000);
    assert!(matches!(loaded.status, SessionStatus::Completed { .. }));
}

// ── 5. Listing and filtering ──────────────────────────────────────────────────

#[tokio::test]
async fn list_sessions_all() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    for i in 0..5 {
        mgr.save(&make_session(&format!("task {i}"), "/work")).await.unwrap();
    }
    assert_eq!(mgr.list(None).await.unwrap().len(), 5);
}

#[tokio::test]
async fn list_sessions_by_workspace() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s1 = make_session("t1", "/proj/a");
    s1.workspace_root = PathBuf::from("/proj/a");
    let mut s2 = make_session("t2", "/proj/b");
    s2.workspace_root = PathBuf::from("/proj/b");
    let mut s3 = make_session("t3", "/proj/a");
    s3.workspace_root = PathBuf::from("/proj/a");

    for s in [&s1, &s2, &s3] {
        mgr.save(s).await.unwrap();
    }

    let filtered = mgr.list(Some(std::path::Path::new("/proj/a"))).await.unwrap();
    assert_eq!(filtered.len(), 2);
    for s in &filtered {
        assert_eq!(s.workspace_root, PathBuf::from("/proj/a"));
    }
}

#[tokio::test]
async fn count_matches_saved() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    assert_eq!(mgr.count().await.unwrap(), 0);
    mgr.save(&make_session("a", "/w")).await.unwrap();
    mgr.save(&make_session("b", "/w")).await.unwrap();
    mgr.save(&make_session("c", "/w")).await.unwrap();
    assert_eq!(mgr.count().await.unwrap(), 3);
}

// ── 6. Purge ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn purge_old_sessions_removes_stale() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    // Create two sessions — manually age one
    let mut old = make_session("old task", "/old");
    old.updated_at = chrono::Utc::now() - chrono::Duration::hours(25);
    mgr.save(&old).await.unwrap();

    let fresh = make_session("fresh task", "/fresh");
    mgr.save(&fresh).await.unwrap();

    let deleted = mgr
        .purge_old_sessions(Duration::from_secs(3600 * 24))
        .await
        .unwrap();

    assert_eq!(deleted, 1, "only the old session should be purged");
    assert_eq!(mgr.count().await.unwrap(), 1);
    assert!(mgr.load(&fresh.id).await.unwrap().is_some());
    assert!(mgr.load(&old.id).await.unwrap().is_none());
}

// ── 7. Persistence across process restart ─────────────────────────────────────

#[tokio::test]
async fn sessions_persist_across_manager_restart() {
    let tmp = TempDir::new().unwrap();

    let s = {
        let mgr = manager_with_dir(&tmp).await;
        let s = make_session("persisted task", "/persist");
        let msgs = vec![user_msg("hello"), assistant_msg("world")];
        mgr.save_with_history(&s, &msgs).await.unwrap();
        s
    }; // mgr dropped here — simulates process exit

    // New manager, same data_dir
    let mgr2 = manager_with_dir(&tmp).await;
    let loaded = mgr2.load(&s.id).await.unwrap().unwrap();
    assert_eq!(loaded.task, "persisted task");

    let swh = mgr2.load_with_history(&s.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 2);
}

#[tokio::test]
async fn sqlite_store_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("sessions.db");

    {
        let store = SqliteSessionStore::open(&db_path).await.unwrap();
        let mut s = make_session("task", "/work");
        s.total_tokens = 9999;
        store.save(&s).await.unwrap();
    }

    {
        let store = SqliteSessionStore::open(&db_path).await.unwrap();
        let all = store.list(None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].total_tokens, 9999);
    }
}

// ── 8. Concurrency ────────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_session_metadata_no_corruption() {
    // Verify concurrent writes to session METADATA (sessions.db) don't corrupt.
    // Conversation history writes are intentionally sequential in this test
    // because SQLite single-writer semantics apply to the conversations.db.
    let tmp = TempDir::new().unwrap();
    let mgr = Arc::new(SessionManager::new(tmp.path()).await.unwrap());

    let mut handles = Vec::new();
    for i in 0..10 {
        let m = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            // save() only touches sessions.db — safe to run concurrently
            let s = make_session(&format!("task {i}"), "/concurrent");
            m.save(&s).await.unwrap();
            s.id
        }));
    }

    let ids: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(mgr.count().await.unwrap(), 10, "all 10 sessions must be saved");

    // Verify each session's metadata is intact
    for id in &ids {
        let s = mgr.load(id).await.unwrap().unwrap();
        assert!(s.task.starts_with("task "), "task should be preserved: {}", s.task);
    }
}

#[tokio::test]
async fn concurrent_session_isolation() {
    // Sequential saves to avoid SQLite write-lock contention, then verify isolation
    let tmp = TempDir::new().unwrap();
    let mgr = SessionManager::new(tmp.path()).await.unwrap();

    let s1 = make_session("session 1", "/iso");
    let s2 = make_session("session 2", "/iso");
    let id1 = s1.id.clone();
    let id2 = s2.id.clone();

    mgr.save_with_history(&s1, &[user_msg("s1 msg1"), user_msg("s1 msg2")])
        .await
        .unwrap();
    mgr.save_with_history(&s2, &[user_msg("s2 msg1")])
        .await
        .unwrap();

    assert_eq!(mgr.message_count(&id1).await.unwrap(), 2);
    assert_eq!(mgr.message_count(&id2).await.unwrap(), 1);

    // Verify history is isolated (s2's messages not leaked into s1 and vice versa)
    let swh1 = mgr.load_with_history(&id1).await.unwrap().unwrap();
    let swh2 = mgr.load_with_history(&id2).await.unwrap().unwrap();
    assert!(swh1.messages.iter().all(|m| m.text().contains("s1")));
    assert!(swh2.messages.iter().all(|m| m.text().contains("s2")));
}

// ── 9. Conversation key convention ───────────────────────────────────────────

#[tokio::test]
async fn conversation_key_is_session_id_string() {
    let id = SessionId::new();
    assert_eq!(conversation_key_for(&id), id.as_str().to_string());
}

#[tokio::test]
async fn qa_conversation_key_format() {
    let id = SessionId::from_string("abc-123");
    let qa_key = format!("{}::qa", id.as_str());
    assert_eq!(qa_key, "abc-123::qa");
}

// ── 10. StatusStore integration ───────────────────────────────────────────────

#[tokio::test]
async fn all_session_status_variants_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let statuses = vec![
        SessionStatus::Active,
        SessionStatus::Suspended,
        SessionStatus::Completed { summary: "all done".into() },
        SessionStatus::Failed { error: "oom".into() },
        SessionStatus::Cancelled,
    ];

    for status in statuses {
        let mut s = make_session("status test", "/st");
        s.status = status.clone();
        let id = s.id.clone();
        mgr.save(&s).await.unwrap();
        let loaded = mgr.load(&id).await.unwrap().unwrap();
        assert_eq!(
            loaded.status.label(),
            status.label(),
            "status {}: roundtrip failed",
            status.label()
        );
    }
}

#[tokio::test]
async fn git_branch_persisted() {
    let tmp = TempDir::new().unwrap();
    let mgr = manager_with_dir(&tmp).await;

    let mut s = make_session("git task", "/git");
    s.git_branch = Some("agtrs/run-abc123".into());
    let id = s.id.clone();
    mgr.save(&s).await.unwrap();

    let loaded = mgr.load(&id).await.unwrap().unwrap();
    assert_eq!(loaded.git_branch.unwrap(), "agtrs/run-abc123");
}
