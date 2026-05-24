//! Git inspection tools: status, diff, log.
//!
//! All three tools are read-only and backed by [`agtrs_git::GitRepo`] directly —
//! no worktree required. For write operations (stage, commit, restore) use
//! [`agtrs_git::GitToolSet`] with a [`agtrs_git::WorktreeGuard`].

pub mod diff;
pub mod log;
pub mod status;

pub use diff::GitDiffTool;
pub use log::GitLogTool;
pub use status::GitStatusTool;

use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::ErasedTool;

/// Convenience builder — creates all git inspection tools from a shared repo.
pub struct GitTools {
    repo: Arc<GitRepo>,
}

impl GitTools {
    /// Create a `GitTools` from a shared `GitRepo`.
    pub fn new(repo: Arc<GitRepo>) -> Self {
        Self { repo }
    }

    /// Open the repo at `path` and create a `GitTools`.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, agtrs_git::GitError> {
        Ok(Self::new(Arc::new(GitRepo::open(path)?)))
    }

    /// All three read-only git tools: status, diff, log.
    pub fn all(&self) -> Vec<Arc<ErasedTool>> {
        vec![
            Arc::new(GitStatusTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
            Arc::new(GitDiffTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
            Arc::new(GitLogTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
        ]
    }

    /// Just status.
    pub fn status(&self) -> Arc<ErasedTool> {
        Arc::new(GitStatusTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>
    }

    /// Status + diff (two most-used tools for read-only agent inspection).
    pub fn status_and_diff(&self) -> Vec<Arc<ErasedTool>> {
        vec![
            Arc::new(GitStatusTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
            Arc::new(GitDiffTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
        ]
    }
}
