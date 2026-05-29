//! Memory tools for xaft agents.
//!
//! Provides four tools that agents can use to interact with the memory system:
//!
//! - [`RememberTool`] — store facts, insights, and discoveries
//! - [`RecallTool`] — search project memory for relevant entries
//! - [`ForgetTool`] — delete stale or incorrect memories
//! - [`SummarizeMemoryTool`] — compress old memories into summaries

pub mod forget;
pub mod recall;
pub mod remember;
pub mod summarize;

pub use forget::ForgetTool;
pub use recall::RecallTool;
pub use remember::RememberTool;
pub use summarize::SummarizeMemoryTool;

use std::sync::Arc;

use agtrs_runtime::tool::ErasedTool;

use crate::manager::XaftMemoryManager;

/// Collect all memory tools as erased trait objects.
///
/// Registers `remember`, `recall`, `forget`, and `summarize_memory`.
pub fn memory_toolset(manager: Arc<XaftMemoryManager>) -> MemoryToolset {
    MemoryToolset {
        remember: Arc::new(RememberTool::new(Arc::clone(&manager))) as Arc<ErasedTool>,
        recall: Arc::new(RecallTool::new(Arc::clone(&manager))) as Arc<ErasedTool>,
        forget: Arc::new(ForgetTool::new(Arc::clone(&manager))) as Arc<ErasedTool>,
        summarize: Arc::new(SummarizeMemoryTool::new(Arc::clone(&manager))) as Arc<ErasedTool>,
    }
}

/// A set of memory tools ready for agent registration.
pub struct MemoryToolset {
    pub remember: Arc<ErasedTool>,
    pub recall: Arc<ErasedTool>,
    pub forget: Arc<ErasedTool>,
    pub summarize: Arc<ErasedTool>,
}

impl MemoryToolset {
    /// All four memory tools as a vector.
    pub fn all(&self) -> Vec<Arc<ErasedTool>> {
        vec![
            Arc::clone(&self.remember),
            Arc::clone(&self.recall),
            Arc::clone(&self.forget),
            Arc::clone(&self.summarize),
        ]
    }

    /// Only the read-only tools (recall).
    pub fn read_only(&self) -> Vec<Arc<ErasedTool>> {
        vec![Arc::clone(&self.recall)]
    }
}
