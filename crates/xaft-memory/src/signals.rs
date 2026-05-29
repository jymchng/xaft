//! Signal types for xaft memory operations.
//!
//! Emitted via `agtrs_runtime::signals::SignalBus` during memory operations.
//! The TUI bridge subscribes to these events for display.

/// Emitted when a memory entry is stored.
#[derive(Debug, Clone)]
pub struct XaftMemoryStored {
    /// The content that was stored.
    pub content_summary: String,
    /// Tags attached to the entry.
    pub tags: Vec<String>,
    /// Scope display string.
    pub scope: String,
    /// Which agent stored this memory.
    pub agent_name: String,
}

/// Emitted when a memory search (recall) completes.
#[derive(Debug, Clone)]
pub struct XaftMemoryRecalled {
    /// The query text.
    pub query: String,
    /// Number of results found.
    pub results_count: usize,
    /// Top result content preview (truncated).
    pub top_result_preview: Option<String>,
}
