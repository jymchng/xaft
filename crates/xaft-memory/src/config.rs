//! Memory configuration for xaft.

use serde::{Deserialize, Serialize};

/// Configuration for the xaft memory system.
///
/// Part of [`XaftConfig`](xaft_config::XaftConfig) when enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Whether the memory system is enabled.
    pub enabled: bool,

    /// Storage backend: `"sqlite"` or `"in_memory"`.
    pub backend: MemoryBackend,

    /// Auto-remember facts extracted from agent turns.
    pub auto_remember: bool,

    /// Auto-summarize old memories when the store grows large.
    pub auto_summarize: bool,

    /// Default to project-scoped memory (workspace scope).
    pub project_scope_default: bool,

    /// Maximum number of memories before auto-summarization triggers.
    pub max_entries: Option<usize>,

    /// TTL in seconds for auto-remembered entries. `None` = no expiry.
    pub auto_remember_ttl_secs: Option<u64>,

    /// Maximum search results returned by recall.
    pub max_search_results: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: MemoryBackend::Sqlite,
            auto_remember: true,
            auto_summarize: true,
            project_scope_default: true,
            max_entries: Some(10_000),
            auto_remember_ttl_secs: None,
            max_search_results: 10,
        }
    }
}

/// Storage backend selection.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackend {
    /// SQLite-backed persistent storage.
    #[default]
    Sqlite,
    /// Ephemeral in-memory storage (for tests or ephemeral sessions).
    InMemory,
}
