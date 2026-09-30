//! Round-trip and tier-isolation tests for the persistent memory backend.
//!
//! WHY THIS FILE EXISTS
//!
//! `memory_e2e.rs` covers behaviour well but builds every manager with
//! `MemoryBackend::InMemory`, so it cannot answer the two questions that
//! matter most about persistent memory:
//!
//!   1. Does a memory actually survive the process that wrote it?
//!   2. Is one scope able to see or delete another's entries?
//!
//! Neither was testable before `XaftMemoryManager::new_in_dir` existed. The
//! SQLite path was hardcoded to `dirs::data_dir()/xaft/memory`, so any test
//! of the persistent backend would have written to the developer's real data
//! directory — which is why the round-trip was never simply added. That is
//! also why parity row 120 (`project_memory_path`) could not be closed:
//! there was no setting to point anywhere else.
//!
//! SCOPE CAVEAT, stated so the next pass does not over-read these tests: the
//! public `XaftMemoryManager` API exposes the **Workspace** tier only
//! (`remember`/`recall`/`list_project_memories`/`clear_project` are all
//! workspace-scoped). `Session`, `Run`, `User` and `Global` tiers exist in
//! `agtrs_memory::MemoryScope` and are addressable through `inner()`, but xaft
//! does not surface them to the agent. These tests pin the tiers' *isolation*
//! at the store layer; they do not claim xaft has a user-facing tier model.

use agtrs_memory::query::ListOptions;
use agtrs_memory::{MemoryEntry, MemoryFilter, MemoryKind, MemoryScope};
use tempfile::TempDir;
use xaft_memory::config::{MemoryBackend, MemoryConfig};
use xaft_memory::manager::XaftMemoryManager;

/// A SQLite-backed manager rooted at `dir` — a temp dir in every test here,
/// never the real data directory.
async fn sqlite_in(dir: &TempDir, workspace_id: &str) -> XaftMemoryManager {
    let config = MemoryConfig {
        enabled: true,
        backend: MemoryBackend::Sqlite,
        ..Default::default()
    };
    XaftMemoryManager::new_in_dir(
        MemoryBackend::Sqlite,
        config,
        workspace_id.to_string(),
        None,
        Some(dir.path()),
    )
    .await
    .expect("sqlite memory manager")
}

async fn entries_in_scope(
    mgr: &XaftMemoryManager,
    scope: &MemoryScope,
) -> Vec<agtrs_memory::MemoryEntry> {
    let options = ListOptions::new().with_filter(MemoryFilter {
        scopes: Some(vec![scope.clone()]),
        ..Default::default()
    });
    mgr.inner().list(&options).await.expect("list entries")
}

// ── Round-trip ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn memory_survives_a_manager_restart() {
    let dir = TempDir::new().unwrap();

    {
        let mgr = sqlite_in(&dir, "proj-a").await;
        mgr.remember("the auth service uses JWT", &["arch"])
            .await
            .unwrap();
        mgr.remember("tests use tempfile", &["testing"])
            .await
            .unwrap();
        assert_eq!(mgr.count_project_memories().await.unwrap(), 2);
        // The manager is dropped here, closing its SQLite connection. This is
        // the whole point: a memory must outlive the handle that wrote it.
    }

    let reopened = sqlite_in(&dir, "proj-a").await;
    assert_eq!(
        reopened.count_project_memories().await.unwrap(),
        2,
        "both entries must survive the reopen"
    );
    let hits = reopened.recall("auth JWT").await.unwrap();
    assert!(!hits.is_empty(), "the JWT fact must be recallable");
    assert!(hits[0].entry.content.contains("JWT"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forgotten_memory_stays_forgotten_after_a_restart() {
    let dir = TempDir::new().unwrap();
    let id = {
        let mgr = sqlite_in(&dir, "proj-a").await;
        let keep = mgr.remember("keep me", &[]).await.unwrap();
        let drop_it = mgr.remember("forget me", &[]).await.unwrap();
        assert!(mgr.forget(&drop_it).await.unwrap());
        keep
    };

    let reopened = sqlite_in(&dir, "proj-a").await;
    assert_eq!(reopened.count_project_memories().await.unwrap(), 1);
    let hits = reopened.recall("forget me").await.unwrap();
    assert!(
        hits.is_empty(),
        "a deletion must persist too, not just insertions"
    );
    let kept = reopened.recall("keep me").await.unwrap();
    assert!(!kept.is_empty() && kept[0].entry.id == id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_directory_starts_empty() {
    let dir = TempDir::new().unwrap();
    let mgr = sqlite_in(&dir, "proj-a").await;
    assert_eq!(mgr.count_project_memories().await.unwrap(), 0);
    assert!(mgr.recall("anything").await.unwrap().is_empty());
}

// ── Workspace tier ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn two_workspaces_sharing_one_database_stay_isolated_across_restarts() {
    let dir = TempDir::new().unwrap();
    // Distinctive contents: `recall` is a keyword search, so a query sharing a
    // word with the OTHER workspace's memory would match on that word and tell
    // us nothing about isolation. (The first draft of this test used
    // "Project A only secret" / "Project B only secret" and "recalled" B from A
    // purely because both contain the word "Project".)
    const A_ONLY: &str = "alphauniquemarker";
    const B_ONLY: &str = "betauniquemarker";

    {
        let a = sqlite_in(&dir, "project-a").await;
        let b = sqlite_in(&dir, "project-b").await;
        a.remember(A_ONLY, &[]).await.unwrap();
        b.remember(B_ONLY, &[]).await.unwrap();

        assert!(
            a.recall(A_ONLY).await.unwrap()[0]
                .entry
                .content
                .contains(A_ONLY)
        );
        assert!(
            b.recall(B_ONLY).await.unwrap()[0]
                .entry
                .content
                .contains(B_ONLY)
        );
    }

    // Isolation must survive reopening, not just hold within one process.
    let a = sqlite_in(&dir, "project-a").await;
    let b = sqlite_in(&dir, "project-b").await;
    assert_eq!(a.count_project_memories().await.unwrap(), 1);
    assert_eq!(b.count_project_memories().await.unwrap(), 1);

    let a_hits = a.recall(B_ONLY).await.unwrap();
    assert!(
        a_hits.is_empty(),
        "project A must not recall project B's memory, got {:?}",
        a_hits.iter().map(|h| &h.entry.content).collect::<Vec<_>>()
    );
    let b_hits = b.recall(A_ONLY).await.unwrap();
    assert!(
        b_hits.is_empty(),
        "project B must not recall project A's memory, got {:?}",
        b_hits.iter().map(|h| &h.entry.content).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_project_leaves_the_other_workspace_alone() {
    let dir = TempDir::new().unwrap();
    let a = sqlite_in(&dir, "project-a").await;
    let b = sqlite_in(&dir, "project-b").await;
    a.remember("a1", &[]).await.unwrap();
    a.remember("a2", &[]).await.unwrap();
    b.remember("b1", &[]).await.unwrap();

    assert_eq!(a.clear_project().await.unwrap(), 2);
    assert_eq!(a.count_project_memories().await.unwrap(), 0);
    assert_eq!(
        b.count_project_memories().await.unwrap(),
        1,
        "clearing one workspace must not reach into another"
    );
}

// ── Session / Global tiers ──────────────────────────────────────────────────
//
// These tiers exist in `agtrs_memory::MemoryScope` and are addressable via
// `inner()`. xaft's public API does not expose them (see the scope caveat at
// the top of this file), so these tests pin store-level isolation only.

#[tokio::test(flavor = "multi_thread")]
async fn a_session_tier_entry_is_not_visible_to_the_workspace_tier() {
    let dir = TempDir::new().unwrap();
    let mgr = sqlite_in(&dir, "proj-a").await;

    mgr.inner()
        .store(MemoryEntry::new(
            MemoryScope::Session {
                session_id: "sess-1".into(),
            },
            MemoryKind::Fact,
            "session-scoped knowledge",
        ))
        .await
        .unwrap();
    mgr.remember("workspace-scoped knowledge", &[])
        .await
        .unwrap();

    let hits = mgr.recall("session-scoped knowledge").await.unwrap();
    assert!(
        hits.is_empty(),
        "the workspace-tier recall must not cross into the session tier"
    );
    assert_eq!(mgr.count_project_memories().await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_sessions_do_not_see_each_others_memories() {
    let dir = TempDir::new().unwrap();
    let mgr = sqlite_in(&dir, "proj-a").await;

    for sid in ["sess-1", "sess-2"] {
        mgr.inner()
            .store(MemoryEntry::new(
                MemoryScope::Session {
                    session_id: sid.into(),
                },
                MemoryKind::Fact,
                format!("knowledge for {sid}"),
            ))
            .await
            .unwrap();
    }

    let one = entries_in_scope(
        &mgr,
        &MemoryScope::Session {
            session_id: "sess-1".into(),
        },
    )
    .await;
    let two = entries_in_scope(
        &mgr,
        &MemoryScope::Session {
            session_id: "sess-2".into(),
        },
    )
    .await;
    assert_eq!(one.len(), 1);
    assert_eq!(two.len(), 1);
    assert!(one[0].content.contains("sess-1"));
    assert!(two[0].content.contains("sess-2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_tier_is_reachable_from_every_workspace() {
    let dir = TempDir::new().unwrap();
    {
        let a = sqlite_in(&dir, "project-a").await;
        a.inner()
            .store(MemoryEntry::new(
                MemoryScope::Global,
                MemoryKind::Fact,
                "organisation-wide convention",
            ))
            .await
            .unwrap();
    }

    for ws in ["project-a", "project-b"] {
        let mgr = sqlite_in(&dir, ws).await;
        let entries = entries_in_scope(&mgr, &MemoryScope::Global).await;
        assert_eq!(
            entries.len(),
            1,
            "the global tier must be readable from {ws} — that is what makes it global"
        );
        assert!(entries[0].content.contains("organisation-wide"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clearing_the_workspace_tier_leaves_session_and_global_entries() {
    let dir = TempDir::new().unwrap();
    let mgr = sqlite_in(&dir, "proj-a").await;

    mgr.remember("workspace fact", &[]).await.unwrap();
    mgr.inner()
        .store(MemoryEntry::new(
            MemoryScope::Session {
                session_id: "sess-1".into(),
            },
            MemoryKind::Fact,
            "session fact",
        ))
        .await
        .unwrap();
    mgr.inner()
        .store(MemoryEntry::new(
            MemoryScope::Global,
            MemoryKind::Fact,
            "global fact",
        ))
        .await
        .unwrap();

    assert_eq!(mgr.clear_project().await.unwrap(), 1);

    assert_eq!(
        entries_in_scope(
            &mgr,
            &MemoryScope::Session {
                session_id: "sess-1".into()
            }
        )
        .await
        .len(),
        1,
        "clear_project must not delete session-tier entries"
    );
    assert_eq!(
        entries_in_scope(&mgr, &MemoryScope::Global).await.len(),
        1,
        "clear_project must not delete global-tier entries"
    );
}

// ── The store path is real, not decorative ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn new_in_dir_actually_places_the_database_there() {
    let dir = TempDir::new().unwrap();
    let mgr = sqlite_in(&dir, "proj-a").await;
    mgr.remember("written to the chosen dir", &[])
        .await
        .unwrap();

    let db = dir.path().join("memory.db");
    assert!(
        db.exists(),
        "memory.db must be created in the supplied directory, got {:?}",
        std::fs::read_dir(dir.path())
            .map(|d| d
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
}
