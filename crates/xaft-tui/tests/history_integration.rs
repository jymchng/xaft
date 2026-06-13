//! Integration tests for `InputHistoryStore` (PRD-60 Feature B).
//!
//! Tests record / search / eviction semantics and verify interaction with
//! `HistoryTriggerHandler`.

use std::sync::{Arc, RwLock};

use xaft_tui::trigger::history::{
    HistoryKind, HistoryTriggerHandler, InputHistoryStore, time_ago_duration,
};
use xaft_tui::trigger::{LocalTriggerHandler, TriggerContext};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_store(max: usize) -> InputHistoryStore {
    InputHistoryStore::new(max)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn history_store_records_agent_tasks() {
    let mut store = make_store(50);
    store.push(
        "Fix the authentication bug".to_string(),
        HistoryKind::AgentTask,
    );
    store.push(
        "Add unit tests for payment".to_string(),
        HistoryKind::AgentTask,
    );

    assert_eq!(store.len(), 2);
    let all = store.search("");
    assert_eq!(all.len(), 2);
    // Newest first
    assert_eq!(all[0].text, "Add unit tests for payment");
    assert_eq!(all[0].kind, HistoryKind::AgentTask);
}

#[test]
fn history_store_records_slash_commands() {
    let mut store = make_store(50);
    store.push("/compact".to_string(), HistoryKind::SlashCommand);
    store.push("/cost".to_string(), HistoryKind::SlashCommand);

    assert_eq!(store.len(), 2);
    let cmds = store.search("/");
    assert_eq!(cmds.len(), 2);
}

#[test]
fn history_store_records_mixed_kinds() {
    let mut store = make_store(50);
    store.push(
        "refactor the login flow".to_string(),
        HistoryKind::AgentTask,
    );
    store.push("/compact".to_string(), HistoryKind::SlashCommand);
    store.push("add caching to the API".to_string(), HistoryKind::AgentTask);

    assert_eq!(store.len(), 3);
    // Newest is an AgentTask
    let all = store.search("");
    assert_eq!(all[0].kind, HistoryKind::AgentTask);
    assert_eq!(all[1].kind, HistoryKind::SlashCommand);
    assert_eq!(all[2].kind, HistoryKind::AgentTask);
}

#[test]
fn history_search_filters_by_content() {
    let mut store = make_store(50);
    store.push(
        "Fix the authentication bug in login.rs".to_string(),
        HistoryKind::AgentTask,
    );
    store.push("/compact".to_string(), HistoryKind::SlashCommand);
    store.push(
        "Add tests for the payment module".to_string(),
        HistoryKind::AgentTask,
    );
    store.push(
        "Fix the OAuth redirect bug".to_string(),
        HistoryKind::AgentTask,
    );

    // "fix" matches two entries (case-insensitive)
    let fixes = store.search("fix");
    assert_eq!(
        fixes.len(),
        2,
        "search('fix') must return 2 entries, got: {:?}",
        fixes.iter().map(|e| &e.text).collect::<Vec<_>>()
    );

    // "auth" matches two entries ("authentication" and "OAuth" both contain "auth")
    let auth_results = store.search("auth");
    assert!(
        auth_results.len() >= 1,
        "search('auth') must match at least one entry"
    );
    assert!(
        auth_results
            .iter()
            .any(|e| e.text.contains("authentication")),
        "search('auth') must include the authentication entry"
    );

    // "payment" matches one entry
    let payment = store.search("payment");
    assert_eq!(payment.len(), 1);
    assert!(payment[0].text.contains("payment"));

    // "xyz" matches no entries
    let nothing = store.search("xyz");
    assert!(nothing.is_empty());
}

#[test]
fn history_store_evicts_on_capacity() {
    let mut store = make_store(3);
    store.push("one".to_string(), HistoryKind::AgentTask);
    store.push("two".to_string(), HistoryKind::AgentTask);
    store.push("three".to_string(), HistoryKind::AgentTask);
    assert_eq!(store.len(), 3);

    store.push("four".to_string(), HistoryKind::AgentTask);
    assert_eq!(store.len(), 3);

    let texts: Vec<&str> = store.search("").iter().map(|e| e.text.as_str()).collect();
    assert!(!texts.contains(&"one"), "'one' must have been evicted");
    assert!(texts.contains(&"four"), "'four' must be present");
}

#[test]
fn history_store_no_adjacent_dedup() {
    let mut store = make_store(50);
    store.push("same command".to_string(), HistoryKind::AgentTask);
    store.push("same command".to_string(), HistoryKind::AgentTask);
    store.push("same command".to_string(), HistoryKind::AgentTask);
    // All three must be recorded (no adjacent dedup)
    assert_eq!(
        store.len(),
        3,
        "InputHistoryStore must NOT dedup adjacent identical entries"
    );
}

#[test]
fn history_trigger_returns_items_with_full_text_in_insert() {
    let store_inner = Arc::new(RwLock::new(make_store(50)));
    {
        let mut s = store_inner.write().unwrap();
        s.push(
            "Fix the type error in src/auth.rs".to_string(),
            HistoryKind::AgentTask,
        );
        s.push("/compact".to_string(), HistoryKind::SlashCommand);
    }
    let handler = HistoryTriggerHandler::new(Arc::clone(&store_inner));
    let ctx = TriggerContext::new_for_test("", '#');
    let items = handler.matches(&ctx);
    assert_eq!(items.len(), 2);
    // Newest first: /compact was pushed last
    assert_eq!(items[0].insert, "/compact");
    assert_eq!(items[1].insert, "Fix the type error in src/auth.rs");
}

#[test]
fn history_trigger_filters_with_prefix() {
    let store_inner = Arc::new(RwLock::new(make_store(50)));
    {
        let mut s = store_inner.write().unwrap();
        s.push("add authentication".to_string(), HistoryKind::AgentTask);
        s.push("/compact".to_string(), HistoryKind::SlashCommand);
        s.push("fix auth bug".to_string(), HistoryKind::AgentTask);
    }
    let handler = HistoryTriggerHandler::new(Arc::clone(&store_inner));
    let ctx = TriggerContext::new_for_test("auth", '#');
    let items = handler.matches(&ctx);
    // Should match "add authentication" and "fix auth bug" (both contain "auth")
    assert_eq!(
        items.len(),
        2,
        "prefix 'auth' must match 2 entries, got: {:?}",
        items.iter().map(|i| &i.display).collect::<Vec<_>>()
    );
    // Must not include /compact
    assert!(items.iter().all(|i| !i.insert.contains("compact")));
}

#[test]
fn time_ago_all_ranges() {
    use std::time::Duration;

    assert_eq!(time_ago_duration(Duration::from_secs(0)), "0s ago");
    assert_eq!(time_ago_duration(Duration::from_secs(30)), "30s ago");
    assert_eq!(time_ago_duration(Duration::from_secs(59)), "59s ago");
    assert_eq!(time_ago_duration(Duration::from_secs(60)), "1m ago");
    assert_eq!(time_ago_duration(Duration::from_secs(120)), "2m ago");
    assert_eq!(time_ago_duration(Duration::from_secs(3599)), "59m ago");
    assert_eq!(time_ago_duration(Duration::from_secs(3600)), "1h ago");
    assert_eq!(time_ago_duration(Duration::from_secs(7200)), "2h ago");
    assert_eq!(time_ago_duration(Duration::from_secs(86399)), "23h ago");
    assert_eq!(time_ago_duration(Duration::from_secs(86400)), "1d ago");
    assert_eq!(time_ago_duration(Duration::from_secs(172800)), "2d ago");
}
