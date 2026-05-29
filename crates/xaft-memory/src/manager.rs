//! `XaftMemoryManager` — the coding-agent-facing memory layer.
//!
//! Wraps [`agtrs_memory::MemoryManager`] with xaft-specific behavior:
//!
//! - git-aware scoping (branch, commit, worktree)
//! - project-scoped memory by default
//! - convenience methods for coding workflows
//! - SignalBus integration for TUI events

use std::sync::Arc;

use agtrs_memory::{
    ErasedMemoryStore, GcPolicy, InMemoryMemoryStore, ListOptions, MemoryEntry, MemoryFilter,
    MemoryId, MemoryKind, MemoryManager, MemoryManagerBuilder, MemoryManagerConfig, MemoryQuery,
    MemoryScope, SearchResult, SqliteMemoryStore, into_erased,
};
use agtrs_runtime::signals::SignalBus;
use tracing::{debug, info};

use crate::config::{MemoryBackend, MemoryConfig};
use crate::error::{MemoryError, MemoryResult};
use crate::signals::{XaftMemoryRecalled, XaftMemoryStored};

/// Git context attached to memory entries for branch/commit awareness.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    /// Current branch name.
    pub branch: Option<String>,
    /// Current commit SHA (short).
    pub commit_sha: Option<String>,
    /// Worktree/session identifier.
    pub worktree_id: Option<String>,
}

/// The xaft memory manager — coding-agent-facing memory layer.
///
/// Wraps [`MemoryManager`] with project scoping, git awareness,
/// and convenience methods for common coding-agent memory operations.
pub struct XaftMemoryManager {
    inner: MemoryManager,
    config: MemoryConfig,
    workspace_id: String,
    signals: Option<Arc<SignalBus>>,
}

impl XaftMemoryManager {
    /// Create a new manager with the given backend and config.
    pub async fn new(
        backend: MemoryBackend,
        config: MemoryConfig,
        workspace_id: String,
        signals: Option<Arc<SignalBus>>,
    ) -> MemoryResult<Self> {
        let store: ErasedMemoryStore = match backend {
            MemoryBackend::Sqlite => {
                let data_dir = dirs::data_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("xaft")
                    .join("memory");
                std::fs::create_dir_all(&data_dir)
                    .map_err(|e| MemoryError::Config(format!("create data dir: {e}")))?;
                let db_path = data_dir.join("memory.db");
                let url = format!("sqlite:{}?mode=rwc", db_path.display());
                let store = SqliteMemoryStore::open(&url).await?;
                into_erased(store)
            }
            MemoryBackend::InMemory => {
                let store = InMemoryMemoryStore::new();
                into_erased(store)
            }
        };

        let mgr_config = MemoryManagerConfig {
            max_search_results: config.max_search_results,
            gc_policy: GcPolicy {
                max_entries: config.max_entries,
                ..Default::default()
            },
            ..Default::default()
        };

        let inner = MemoryManagerBuilder::new(store)
            .with_config(mgr_config)
            .with_maybe_signals(signals.clone())
            .build();

        info!(
            backend = ?config.backend,
            workspace = %workspace_id,
            "xaft: memory manager initialized"
        );

        Ok(Self {
            inner,
            config,
            workspace_id,
            signals,
        })
    }

    /// Create an in-memory manager (for tests).
    pub async fn in_memory(config: MemoryConfig) -> MemoryResult<Self> {
        Self::new(
            MemoryBackend::InMemory,
            config,
            "test-workspace".into(),
            None,
        )
        .await
    }

    /// Create an in-memory manager with signals (for tests).
    pub async fn in_memory_with_signals(
        config: MemoryConfig,
        signals: Arc<SignalBus>,
    ) -> MemoryResult<Self> {
        Self::new(
            MemoryBackend::InMemory,
            config,
            "test-workspace".into(),
            Some(signals),
        )
        .await
    }

    /// Access the underlying [`MemoryManager`].
    pub fn inner(&self) -> &MemoryManager {
        &self.inner
    }

    /// The workspace ID this manager is scoped to.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Whether the memory system is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    // ── Convenience API ────────────────────────────────────────────────────

    /// Store a fact in project memory.
    ///
    /// Creates a [`MemoryKind::Fact`] entry scoped to the current workspace.
    pub async fn remember(&self, content: &str, tags: &[&str]) -> MemoryResult<MemoryId> {
        self.remember_with_agent(content, tags, None, None).await
    }

    /// Store a fact with agent attribution and optional git context.
    pub async fn remember_with_agent(
        &self,
        content: &str,
        tags: &[&str],
        agent_name: Option<&str>,
        git: Option<&GitContext>,
    ) -> MemoryResult<MemoryId> {
        if !self.config.enabled {
            return Err(MemoryError::Disabled);
        }

        let scope = self.project_scope();
        let mut metadata = agtrs_memory::MemoryMetadata::new();
        for tag in tags {
            metadata = metadata.with_tag(*tag);
        }
        if let Some(agent) = agent_name {
            metadata = metadata.with_agent(agent);
        }
        if let Some(g) = git {
            if let Some(ref branch) = g.branch {
                metadata = metadata.with_tag(format!("branch:{branch}"));
            }
            if let Some(ref sha) = g.commit_sha {
                metadata = metadata.with_attribute(
                    "commit_sha".to_string(),
                    serde_json::Value::String(sha.clone()),
                );
            }
        }

        let entry = MemoryEntry::new(scope, MemoryKind::Fact, content).with_metadata(metadata);

        let id = self.inner.store(entry).await?;

        debug!(id = %id, content_len = content.len(), "xaft: remembered");

        if let Some(bus) = &self.signals {
            bus.emit(XaftMemoryStored {
                content_summary: truncate(content, 120).to_string(),
                tags: tags.iter().map(|s| s.to_string()).collect(),
                scope: self.project_scope_display(),
                agent_name: agent_name.unwrap_or("unknown").to_string(),
            })
            .await;
        }

        Ok(id)
    }

    /// Search project memory for relevant entries.
    pub async fn recall(&self, query: &str) -> MemoryResult<Vec<SearchResult>> {
        if !self.config.enabled {
            return Err(MemoryError::Disabled);
        }

        let filter = MemoryFilter::new().with_scope(self.project_scope());
        let mq = MemoryQuery::new(query)
            .with_filter(filter)
            .with_limit(self.config.max_search_results);

        let results = self.inner.search(&mq).await?;

        debug!(query = %query, results = results.len(), "xaft: recalled");

        if let Some(bus) = &self.signals {
            bus.emit(XaftMemoryRecalled {
                query: query.to_string(),
                results_count: results.len(),
                top_result_preview: results
                    .first()
                    .map(|r| truncate(&r.entry.content, 120).to_string()),
            })
            .await;
        }

        Ok(results)
    }

    /// Delete a memory entry by ID.
    pub async fn forget(&self, id: &MemoryId) -> MemoryResult<bool> {
        if !self.config.enabled {
            return Err(MemoryError::Disabled);
        }
        Ok(self.inner.delete(id).await?)
    }

    /// List all project memories with optional tag filter.
    pub async fn list_project_memories(
        &self,
        tags: Option<Vec<String>>,
    ) -> MemoryResult<Vec<MemoryEntry>> {
        if !self.config.enabled {
            return Err(MemoryError::Disabled);
        }

        let mut filter = MemoryFilter::new().with_scope(self.project_scope());
        if let Some(tags) = tags {
            for tag in tags {
                filter = filter.with_tag(tag);
            }
        }

        let opts = ListOptions::new().with_filter(filter);
        Ok(self.inner.list(&opts).await?)
    }

    /// Count project memories.
    pub async fn count_project_memories(&self) -> MemoryResult<usize> {
        if !self.config.enabled {
            return Err(MemoryError::Disabled);
        }
        let filter = MemoryFilter::new().with_scope(self.project_scope());
        Ok(self.inner.count(&filter).await?)
    }

    /// Run garbage collection on the memory store.
    pub async fn gc(&self) -> MemoryResult<agtrs_memory::GcStats> {
        Ok(self.inner.gc().await?)
    }

    /// Clear all project memories.
    pub async fn clear_project(&self) -> MemoryResult<usize> {
        Ok(self.inner.delete_scope(&self.project_scope()).await?)
    }

    /// Build a recall query for the planner — returns top N memories as context.
    pub async fn planner_context(&self, task: &str) -> MemoryResult<String> {
        if !self.config.enabled {
            return Ok(String::new());
        }

        let results = self.recall(task).await?;
        if results.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("## Relevant Memory\n\n");
        for (i, result) in results.iter().take(5).enumerate() {
            let tags = if result.entry.metadata.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", result.entry.metadata.tags.join(", "))
            };
            context.push_str(&format!(
                "{}. {}{}\n",
                i + 1,
                truncate(&result.entry.content, 300),
                tags
            ));
        }

        Ok(context)
    }

    // ── Internal helpers ───────────────────────────────────────────────────

    fn project_scope(&self) -> MemoryScope {
        MemoryScope::Workspace {
            workspace_id: self.workspace_id.clone(),
        }
    }

    fn project_scope_display(&self) -> String {
        format!("workspace:{}", self.workspace_id)
    }
}

/// Helper extension trait for `MemoryManagerBuilder` to accept optional signals.
trait BuilderSignalsExt {
    fn with_maybe_signals(self, signals: Option<Arc<SignalBus>>) -> Self;
}

impl BuilderSignalsExt for MemoryManagerBuilder {
    fn with_maybe_signals(self, signals: Option<Arc<SignalBus>>) -> Self {
        match signals {
            Some(bus) => self.with_signals(bus),
            None => self,
        }
    }
}

fn truncate(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        &s[..max_chars]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> MemoryConfig {
        MemoryConfig {
            enabled: true,
            backend: MemoryBackend::InMemory,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn remember_and_recall() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        mgr.remember("The auth service uses JWT tokens", &["architecture"])
            .await
            .unwrap();
        let results = mgr.recall("auth service").await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].entry.content.contains("JWT"));
    }

    #[tokio::test]
    async fn remember_with_agent_and_git() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
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

    #[tokio::test]
    async fn forget_deletes_entry() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        let id = mgr.remember("temporary fact", &[]).await.unwrap();
        assert!(mgr.forget(&id).await.unwrap());
        assert!(mgr.inner().retrieve(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn disabled_returns_error() {
        let config = MemoryConfig {
            enabled: false,
            ..default_config()
        };
        let mgr = XaftMemoryManager::in_memory(config).await.unwrap();
        assert!(mgr.remember("test", &[]).await.is_err());
        assert!(mgr.recall("test").await.is_err());
    }

    #[tokio::test]
    async fn count_project_memories() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        mgr.remember("fact 1", &[]).await.unwrap();
        mgr.remember("fact 2", &[]).await.unwrap();
        assert_eq!(mgr.count_project_memories().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn planner_context_returns_formatted() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        mgr.remember("The project uses Tokio for async", &["tech"])
            .await
            .unwrap();
        mgr.remember("Tests use tempfile for isolation", &["testing"])
            .await
            .unwrap();

        let ctx = mgr.planner_context("project setup").await.unwrap();
        assert!(ctx.contains("Relevant Memory"));
        assert!(ctx.contains("Tokio"));
    }

    #[tokio::test]
    async fn planner_context_empty_when_no_memories() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        let ctx = mgr.planner_context("anything").await.unwrap();
        assert!(ctx.is_empty());
    }

    #[tokio::test]
    async fn signals_emitted_on_store() {
        use agtrs_memory::MemoryStored;

        let signals = Arc::new(SignalBus::new());
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recv_clone = received.clone();

        signals
            .on::<MemoryStored>(move |ev| {
                let recv = recv_clone.clone();
                let id = ev.entry_id.clone();
                tokio::spawn(async move {
                    recv.lock().await.push(id);
                });
            })
            .await;

        let mgr = XaftMemoryManager::in_memory_with_signals(default_config(), signals)
            .await
            .unwrap();
        mgr.remember("test fact", &["tag1"]).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let ids = received.lock().await;
        assert!(!ids.is_empty(), "MemoryStored signal should be emitted");
    }

    #[tokio::test]
    async fn xaft_memory_stored_signal_emitted() {
        use crate::signals::XaftMemoryStored;

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

        let mgr = XaftMemoryManager::in_memory_with_signals(default_config(), signals)
            .await
            .unwrap();
        mgr.remember("JWT auth pattern", &["auth"]).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let summaries = received.lock().await;
        assert!(
            !summaries.is_empty(),
            "XaftMemoryStored signal should be emitted"
        );
    }

    #[tokio::test]
    async fn xaft_memory_recalled_signal_emitted() {
        use crate::signals::XaftMemoryRecalled;

        let signals = Arc::new(SignalBus::new());
        let received = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recv_clone = received.clone();

        signals
            .on::<XaftMemoryRecalled>(move |ev| {
                let recv = recv_clone.clone();
                let query = ev.query.clone();
                tokio::spawn(async move {
                    recv.lock().await.push(query);
                });
            })
            .await;

        let mgr = XaftMemoryManager::in_memory_with_signals(default_config(), signals)
            .await
            .unwrap();
        mgr.remember("JWT auth pattern", &[]).await.unwrap();
        mgr.recall("auth").await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let queries = received.lock().await;
        assert!(
            !queries.is_empty(),
            "XaftMemoryRecalled signal should be emitted"
        );
    }

    #[tokio::test]
    async fn list_project_memories_with_tags() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        mgr.remember("fact A", &["arch"]).await.unwrap();
        mgr.remember("fact B", &["testing"]).await.unwrap();
        mgr.remember("fact C", &["arch", "important"])
            .await
            .unwrap();

        let arch_only = mgr
            .list_project_memories(Some(vec!["arch".into()]))
            .await
            .unwrap();
        assert_eq!(arch_only.len(), 2);
    }

    #[tokio::test]
    async fn clear_project_removes_all() {
        let mgr = XaftMemoryManager::in_memory(default_config())
            .await
            .unwrap();
        mgr.remember("a", &[]).await.unwrap();
        mgr.remember("b", &[]).await.unwrap();
        mgr.remember("c", &[]).await.unwrap();

        let deleted = mgr.clear_project().await.unwrap();
        assert_eq!(deleted, 3);
        assert_eq!(mgr.count_project_memories().await.unwrap(), 0);
    }
}
