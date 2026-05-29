//! End-to-end tests for xaft-memory runtime integration.
//!
//! Tests that memory tools are properly wired into the runtime and
//! agents can use them during execution.

use std::sync::Arc;

use agtrs_runtime::signals::SignalBus;
use tempfile::TempDir;
use xaft_memory::config::{MemoryBackend, MemoryConfig};
use xaft_memory::manager::XaftMemoryManager;

async fn make_memory_manager() -> Arc<XaftMemoryManager> {
    let config = MemoryConfig {
        enabled: true,
        backend: MemoryBackend::InMemory,
        ..Default::default()
    };
    Arc::new(XaftMemoryManager::in_memory(config).await.unwrap())
}

// ── Memory persists across runtime operations ─────────────────────────────────

#[tokio::test]
async fn memory_manager_persists_data() {
    let mem_mgr = make_memory_manager().await;

    // Store a fact
    mem_mgr
        .remember("The project uses Tokio for async", &["tech"])
        .await
        .unwrap();

    // Recall it
    let results = mem_mgr.recall("Tokio async").await.unwrap();
    assert!(!results.is_empty());
    assert!(results[0].entry.content.contains("Tokio"));
}

#[tokio::test]
async fn memory_manager_project_scoping() {
    let config = MemoryConfig {
        enabled: true,
        backend: MemoryBackend::InMemory,
        ..Default::default()
    };
    let mgr1 = XaftMemoryManager::new(
        MemoryBackend::InMemory,
        config.clone(),
        "project-a".into(),
        None,
    )
    .await
    .unwrap();

    let mgr2 = XaftMemoryManager::new(MemoryBackend::InMemory, config, "project-b".into(), None)
        .await
        .unwrap();

    // Store in project A
    mgr1.remember("Project A secret", &[]).await.unwrap();

    // Should not be visible in project B
    let results = mgr2.recall("secret").await.unwrap();
    assert!(results.is_empty());
}

// ── Signal integration ────────────────────────────────────────────────────────

#[tokio::test]
async fn memory_signals_propagate() {
    use xaft_memory::signals::XaftMemoryStored;

    let signals = Arc::new(SignalBus::new());
    let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let recv_clone = received.clone();

    signals
        .on::<XaftMemoryStored>(move |ev| {
            let recv = recv_clone.clone();
            let summary = ev.content_summary.clone();
            tokio::spawn(async move {
                recv.lock().await.push(summary);
            });
        })
        .await;

    let config = MemoryConfig {
        enabled: true,
        backend: MemoryBackend::InMemory,
        ..Default::default()
    };
    let mgr = XaftMemoryManager::in_memory_with_signals(config, signals)
        .await
        .unwrap();

    mgr.remember("test fact", &["tag1"]).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let summaries = received.lock().await;
    assert!(!summaries.is_empty());
    assert!(summaries[0].contains("test fact"));
}

// ── Git-aware memory ──────────────────────────────────────────────────────────

#[tokio::test]
async fn git_context_attached_to_memories() {
    use xaft_memory::manager::GitContext;

    let mgr = make_memory_manager().await;
    let git = GitContext {
        branch: Some("feature/auth".into()),
        commit_sha: Some("abc1234".into()),
        worktree_id: None,
    };

    mgr.remember_with_agent(
        "Refactored auth middleware",
        &["refactor"],
        Some("coder"),
        Some(&git),
    )
    .await
    .unwrap();

    let results = mgr.recall("auth middleware").await.unwrap();
    assert!(!results.is_empty());
    assert!(
        results[0]
            .entry
            .metadata
            .tags
            .contains(&"branch:feature/auth".to_string())
    );
}

// ── Planner context ───────────────────────────────────────────────────────────

#[tokio::test]
async fn planner_context_includes_memories() {
    let mgr = make_memory_manager().await;

    mgr.remember("The project uses SQLite for persistence", &["db"])
        .await
        .unwrap();
    mgr.remember("Tests use tempfile for isolation", &["testing"])
        .await
        .unwrap();

    let ctx = mgr.planner_context("database setup").await.unwrap();
    assert!(ctx.contains("Relevant Memory"));
    assert!(ctx.contains("SQLite"));
}

#[tokio::test]
async fn planner_context_empty_when_no_memories() {
    let mgr = make_memory_manager().await;
    let ctx = mgr.planner_context("anything").await.unwrap();
    assert!(ctx.is_empty());
}

// ── GC and cleanup ────────────────────────────────────────────────────────────

#[tokio::test]
async fn gc_removes_expired_entries() {
    use chrono::{Duration, Utc};

    let mgr = make_memory_manager().await;
    let past = Utc::now() - Duration::hours(1);

    mgr.inner()
        .store(
            agtrs_memory::MemoryEntry::new(
                agtrs_memory::MemoryScope::Global,
                agtrs_memory::MemoryKind::Fact,
                "expired fact",
            )
            .with_expiry(past),
        )
        .await
        .unwrap();

    mgr.remember("alive fact", &[]).await.unwrap();

    let stats = mgr.gc().await.unwrap();
    assert_eq!(stats.deleted, 1);
    assert_eq!(mgr.count_project_memories().await.unwrap(), 1);
}

#[tokio::test]
async fn clear_project_removes_all() {
    let mgr = make_memory_manager().await;
    mgr.remember("a", &[]).await.unwrap();
    mgr.remember("b", &[]).await.unwrap();
    mgr.remember("c", &[]).await.unwrap();

    let deleted = mgr.clear_project().await.unwrap();
    assert_eq!(deleted, 3);
    assert_eq!(mgr.count_project_memories().await.unwrap(), 0);
}

// ── Concurrent access ─────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_remember_and_recall() {
    let mgr = Arc::new(make_memory_manager().await);

    let mut handles = Vec::new();

    // Spawn multiple remember tasks
    for i in 0..10 {
        let mgr = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            mgr.remember(&format!("fact {i}"), &["concurrent"])
                .await
                .unwrap();
        }));
    }

    // Spawn multiple recall tasks
    for _ in 0..5 {
        let mgr = Arc::clone(&mgr);
        handles.push(tokio::spawn(async move {
            let _ = mgr.recall("fact").await;
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let count = mgr.count_project_memories().await.unwrap();
    assert_eq!(count, 10);
}

// ── Multiple tags ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn multiple_tags_filter_correctly() {
    let mgr = make_memory_manager().await;
    mgr.remember("fact A", &["arch"]).await.unwrap();
    mgr.remember("fact B", &["testing"]).await.unwrap();
    mgr.remember("fact C", &["arch", "important"])
        .await
        .unwrap();

    let arch = mgr
        .list_project_memories(Some(vec!["arch".into()]))
        .await
        .unwrap();
    assert_eq!(arch.len(), 2);

    let important = mgr
        .list_project_memories(Some(vec!["important".into()]))
        .await
        .unwrap();
    assert_eq!(important.len(), 1);
    assert!(important[0].content.contains("fact C"));
}

// ── Recall with different queries ─────────────────────────────────────────────

#[tokio::test]
async fn recall_keyword_matching() {
    let mgr = make_memory_manager().await;
    mgr.remember("The auth service uses JWT tokens", &[])
        .await
        .unwrap();
    mgr.remember("Database connection pooling with 10 connections", &[])
        .await
        .unwrap();

    let auth_results = mgr.recall("auth JWT").await.unwrap();
    assert!(!auth_results.is_empty());
    assert!(auth_results[0].entry.content.contains("JWT"));

    let db_results = mgr.recall("database connection").await.unwrap();
    assert!(!db_results.is_empty());
    assert!(db_results[0].entry.content.contains("pooling"));
}

// ── Forget by ID ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn forget_specific_memory() {
    let mgr = make_memory_manager().await;
    let id1 = mgr.remember("keep this", &[]).await.unwrap();
    let id2 = mgr.remember("delete this", &[]).await.unwrap();

    assert!(mgr.forget(&id2).await.unwrap());
    assert_eq!(mgr.count_project_memories().await.unwrap(), 1);

    // The remaining one should be "keep this"
    let results = mgr.recall("keep").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.id, id1);
}

// ── Recall returns no results gracefully ──────────────────────────────────────

#[tokio::test]
async fn recall_empty_store_returns_empty() {
    let mgr = make_memory_manager().await;
    let results = mgr.recall("anything").await.unwrap();
    assert!(results.is_empty());
}

// ── List with pagination ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_project_memories_all() {
    let mgr = make_memory_manager().await;
    for i in 0..5 {
        mgr.remember(&format!("fact {i}"), &[]).await.unwrap();
    }

    let all = mgr.list_project_memories(None).await.unwrap();
    assert_eq!(all.len(), 5);
}
