//! Integration tests for conversation persistence across tasks.
//!
//! Tests that messages are properly persisted and loaded when resuming sessions.

use std::path::PathBuf;
use std::sync::Arc;

use agtrs_runtime::transport::{Message, Role};
use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};
use xaft_session::SessionManager;

fn make_session(task: &str) -> AgentSession {
    AgentSession::new(
        task,
        PathBuf::from("/work"),
        "default".into(),
        "claude".into(),
    )
}

fn user_msg(text: &str) -> Message {
    Message::user(text)
}

fn assistant_msg(text: &str) -> Message {
    Message::assistant(text)
}

// ── Basic persistence ─────────────────────────────────────────────────────────

#[tokio::test]
async fn messages_persisted_across_tasks() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("task one");

    // Save session with initial messages
    let messages = vec![user_msg("hello"), assistant_msg("hi there")];
    mgr.save_with_history(&session, &messages).await.unwrap();

    // Load back
    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 2);
    assert_eq!(swh.messages[0].role, Role::User);
    assert_eq!(swh.messages[0].content.text(), "hello");
    assert_eq!(swh.messages[1].role, Role::Assistant);
    assert_eq!(swh.messages[1].content.text(), "hi there");
}

#[tokio::test]
async fn resume_loads_prior_messages_in_order() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("fix the bug");

    // Simulate a 3-turn conversation
    let messages = vec![
        user_msg("Fix the type error in src/lib.rs"),
        assistant_msg("I'll look at the file and fix the error."),
        user_msg("Great, also add a test for it"),
        assistant_msg("Done, I've added a test in tests/test_lib.rs"),
        user_msg("Run the tests to make sure they pass"),
        assistant_msg("All tests pass. The fix is complete."),
    ];
    mgr.save_with_history(&session, &messages).await.unwrap();

    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 6);
    assert_eq!(
        swh.messages[0].content.text(),
        "Fix the type error in src/lib.rs"
    );
    assert_eq!(
        swh.messages[5].content.text(),
        "All tests pass. The fix is complete."
    );
}

#[tokio::test]
async fn multi_task_tui_session_accumulates_messages() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("first task");

    // First task
    let msgs1 = vec![user_msg("task 1"), assistant_msg("done 1")];
    mgr.save_with_history(&session, &msgs1).await.unwrap();

    // Append more messages (simulating second task in same session)
    mgr.append_message(&session.id, &user_msg("task 2"))
        .await
        .unwrap();
    mgr.append_message(&session.id, &assistant_msg("done 2"))
        .await
        .unwrap();

    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 4);
    assert_eq!(swh.messages[2].content.text(), "task 2");
    assert_eq!(swh.messages[3].content.text(), "done 2");
}

// ── Conversation store integration ────────────────────────────────────────────

#[tokio::test]
async fn conversation_store_persists_messages() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("test task");
    mgr.save(&session).await.unwrap();

    // Write via conversation store directly
    let conv_store = mgr.conversation_store();
    let key = xaft_session::conversation_key_for(&session.id);
    conv_store
        .save(&key, &[user_msg("via store"), assistant_msg("stored")])
        .await
        .unwrap();

    // Load via session manager
    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 2);
    assert_eq!(swh.messages[0].content.text(), "via store");
}

#[tokio::test]
async fn conversation_key_is_session_id() {
    let id = SessionId::new();
    let key = xaft_session::conversation_key_for(&id);
    assert_eq!(key, id.as_str());
}

// ── Validate resumable ────────────────────────────────────────────────────────

#[tokio::test]
async fn validate_resumable_active_session_with_history() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("active task");
    let messages = vec![user_msg("hello"), assistant_msg("hi")];
    mgr.save_with_history(&session, &messages).await.unwrap();

    let swh = mgr.validate_resumable(&session.id).await.unwrap();
    assert_eq!(swh.session.id, session.id);
    assert_eq!(swh.messages.len(), 2);
}

#[tokio::test]
async fn validate_resumable_completed_session_succeeds() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let mut session = make_session("done task");
    session.status = SessionStatus::Completed {
        summary: "done".into(),
    };
    mgr.save(&session).await.unwrap();

    let swh = mgr.validate_resumable(&session.id).await.unwrap();
    assert_eq!(swh.session.id, session.id);
}

#[tokio::test]
async fn validate_resumable_missing_session_fails() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let id = SessionId::new();
    let err = mgr.validate_resumable(&id).await.unwrap_err();
    assert!(matches!(err, xaft_session::SessionError::NotFound(_)));
}

// ── Message count ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn message_count_matches_saved() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("count task");
    let messages = vec![
        user_msg("a"),
        assistant_msg("b"),
        user_msg("c"),
        assistant_msg("d"),
        user_msg("e"),
    ];
    mgr.save_with_history(&session, &messages).await.unwrap();
    assert_eq!(mgr.message_count(&session.id).await.unwrap(), 5);
}

// ── Delete cascades ───────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_session_removes_conversation() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("delete me");
    let messages = vec![user_msg("hi"), assistant_msg("hello")];
    mgr.save_with_history(&session, &messages).await.unwrap();

    mgr.delete(&session.id).await.unwrap();

    assert!(mgr.load(&session.id).await.unwrap().is_none());
    assert!(mgr.load_with_history(&session.id).await.unwrap().is_none());
}

// ── Concurrent access ─────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_session_operations() {
    let mgr = Arc::new(SessionManager::in_memory().await.unwrap());
    let mut handles = Vec::new();

    for i in 0..10 {
        let m = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            let s = make_session(&format!("task {i}"));
            let msgs = vec![
                user_msg(&format!("user msg {i}")),
                assistant_msg(&format!("assistant msg {i}")),
            ];
            m.save_with_history(&s, &msgs).await.unwrap();
            let swh = m.load_with_history(&s.id).await.unwrap().unwrap();
            assert_eq!(swh.messages.len(), 2);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(mgr.count().await.unwrap(), 10);
}

// ── Replace messages semantics ────────────────────────────────────────────────

#[tokio::test]
async fn save_with_history_replaces_messages() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("replace task");

    // First save
    mgr.save_with_history(&session, &[user_msg("old")])
        .await
        .unwrap();

    // Replace with new messages
    mgr.save_with_history(&session, &[user_msg("new 1"), assistant_msg("new 2")])
        .await
        .unwrap();

    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 2);
    assert_eq!(swh.messages[0].content.text(), "new 1");
}

// ── Empty session ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn new_session_has_empty_history() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("empty task");
    mgr.save(&session).await.unwrap();

    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert!(swh.messages.is_empty());
}

// ── Multiple message types ────────────────────────────────────────────────────

#[tokio::test]
async fn preserves_message_roles() {
    let mgr = SessionManager::in_memory().await.unwrap();
    let session = make_session("role test");

    let messages = vec![
        Message::system("You are a helpful assistant"),
        user_msg("hello"),
        assistant_msg("hi"),
        Message::tool_result("tool-1", "tool result"),
    ];
    mgr.save_with_history(&session, &messages).await.unwrap();

    let swh = mgr.load_with_history(&session.id).await.unwrap().unwrap();
    assert_eq!(swh.messages.len(), 4);
    assert_eq!(swh.messages[0].role, Role::System);
    assert_eq!(swh.messages[1].role, Role::User);
    assert_eq!(swh.messages[2].role, Role::Assistant);
    assert_eq!(swh.messages[3].role, Role::Tool);
}
