//! End-to-end tests for dynamic command registration (PRD-60 Feature C).
//!
//! Tests the full flow:
//!   `TuiEvent::CommandRegistered` → `AppState::handle_event` → `RenderMutation::CommitLine`
//!
//! and verifies that `InputHistoryStore` behaves correctly when used through
//! the trigger handler from an async context.

use std::sync::{Arc, RwLock};

use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::{AppState, commit_line_texts};
use xaft_tui::trigger::history::{HistoryKind, HistoryTriggerHandler, InputHistoryStore};
use xaft_tui::trigger::{LocalTriggerHandler, TriggerContext};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_state() -> AppState {
    AppState::new("test task")
}

// ── E2E tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn command_registered_event_updates_tui() {
    let mut state = make_state();

    // Fire TuiEvent::CommandRegistered
    state.handle_event(TuiEvent::CommandRegistered {
        name: "test_cmd".to_string(),
        source: "skill:test-skill".to_string(),
        description: "A test command from a skill".to_string(),
        args_hint: Some("[arg]".to_string()),
    });

    // Assert a system line was committed with the command name.
    let texts = commit_line_texts(&state.mutations);
    assert!(
        texts.iter().any(|t| t.contains("test_cmd")),
        "must commit a line containing the command name 'test_cmd'; got: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("skill:test-skill")),
        "must commit a line containing the source 'skill:test-skill'; got: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("[command]")),
        "line must contain '[command]' marker; got: {texts:?}"
    );
}

#[tokio::test]
async fn command_registered_dynamic_source() {
    let mut state = make_state();

    state.handle_event(TuiEvent::CommandRegistered {
        name: "my_tool".to_string(),
        source: "dynamic".to_string(),
        description: "A dynamically created tool".to_string(),
        args_hint: None,
    });

    let texts = commit_line_texts(&state.mutations);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("my_tool") && t.contains("dynamic")),
        "system line must contain 'my_tool' and 'dynamic'; got: {texts:?}"
    );
}

#[tokio::test]
async fn command_registered_mcp_source() {
    let mut state = make_state();

    state.handle_event(TuiEvent::CommandRegistered {
        name: "search".to_string(),
        source: "mcp:brave-search".to_string(),
        description: "Search the web using Brave".to_string(),
        args_hint: Some("<query>".to_string()),
    });

    let texts = commit_line_texts(&state.mutations);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("search") && t.contains("mcp:brave-search")),
        "system line must mention the command and source; got: {texts:?}"
    );
}

#[tokio::test]
async fn history_records_submitted_messages() {
    let store = Arc::new(RwLock::new(InputHistoryStore::new(50)));

    // Simulate recording entries as the user submits messages.
    {
        let mut s = store.write().unwrap();
        s.push(
            "Fix the auth bug in login.rs".to_string(),
            HistoryKind::AgentTask,
        );
        s.push("/compact".to_string(), HistoryKind::SlashCommand);
        s.push(
            "Add error handling to the payment flow".to_string(),
            HistoryKind::AgentTask,
        );
    }

    // Verify search works through the trigger handler.
    let handler = HistoryTriggerHandler::new(Arc::clone(&store));
    let ctx = TriggerContext::new_for_test("auth", '#');
    let items = handler.matches(&ctx);

    assert_eq!(
        items.len(),
        1,
        "search for 'auth' must return 1 entry; got {} items: {:?}",
        items.len(),
        items.iter().map(|i| &i.display).collect::<Vec<_>>()
    );
    assert_eq!(items[0].insert, "Fix the auth bug in login.rs");
    assert_eq!(
        items[0].kind,
        xaft_tui::trigger::MatchKind::Custom("history".to_string())
    );
}

#[tokio::test]
async fn history_stores_slash_and_task_separately() {
    let store = Arc::new(RwLock::new(InputHistoryStore::new(50)));

    {
        let mut s = store.write().unwrap();
        s.push("/diff".to_string(), HistoryKind::SlashCommand);
        s.push(
            "explain the codebase architecture".to_string(),
            HistoryKind::AgentTask,
        );
    }

    let handler = HistoryTriggerHandler::new(Arc::clone(&store));

    // All entries
    let all_ctx = TriggerContext::new_for_test("", '#');
    let all_items = handler.matches(&all_ctx);
    assert_eq!(all_items.len(), 2);

    // The hint must contain the kind label
    assert!(
        all_items[0].hint.as_deref().unwrap_or("").contains("task"),
        "most recent (task) hint must contain 'task': {:?}",
        all_items[0].hint
    );
    assert!(
        all_items[1].hint.as_deref().unwrap_or("").contains("cmd"),
        "older (cmd) hint must contain 'cmd': {:?}",
        all_items[1].hint
    );
}

#[tokio::test]
async fn on_select_restores_full_text() {
    let store = Arc::new(RwLock::new(InputHistoryStore::new(50)));

    {
        let mut s = store.write().unwrap();
        s.push("A".repeat(80), HistoryKind::AgentTask);
    }

    let handler = HistoryTriggerHandler::new(Arc::clone(&store));
    let ctx = TriggerContext::new_for_test("", '#');
    let items = handler.matches(&ctx);

    assert_eq!(items.len(), 1);
    // Display is truncated to ≤ 60 chars
    assert!(items[0].display.chars().count() <= 60);
    // on_select must return the FULL text, not the truncated display
    let restored = handler.on_select(&items[0], &ctx);
    assert_eq!(
        restored.len(),
        80,
        "on_select must restore the full 80-char text, not the truncated display"
    );
}
