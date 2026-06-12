//! Git tools: read-only inspection and write operations.
//!
//! Read-only tools are backed by [`agtrs_git::GitRepo`] directly or use
//! `tokio::process::Command::new("git")` for operations not in the GitRepo API.
//!
//! Write tools (staging, committing, pushing) use `git` directly and those
//! that are destructive set `requires_confirmation() = true`.

// Read-only tools
pub mod blame;
pub mod branch;
pub mod diff;
pub mod grep;
pub mod log;
pub mod remote;
pub mod show;
pub mod stash_list;
pub mod status;
pub mod tag;

// Write tools
pub mod add;
pub mod checkout_files;
pub mod commit_tool;
pub mod create_branch;
pub mod merge;
pub mod push;
pub mod stash;
pub mod stash_pop;
pub mod unstage;

// Re-exports
pub use add::GitAddTool;
pub use blame::GitBlameTool;
pub use branch::GitBranchTool;
pub use checkout_files::GitCheckoutFilesTool;
pub use commit_tool::GitCommitStagedTool;
pub use create_branch::GitCreateBranchTool;
pub use diff::GitDiffTool;
pub use grep::GitGrepTool;
pub use log::GitLogTool;
pub use merge::GitMergeTool;
pub use push::GitPushTool;
pub use remote::GitRemoteTool;
pub use show::GitShowTool;
pub use stash::GitStashTool;
pub use stash_list::GitStashListTool;
pub use stash_pop::GitStashPopTool;
pub use status::GitStatusTool;
pub use tag::GitTagTool;
pub use unstage::GitUnstageTool;

use std::sync::Arc;

use agtrs_git::GitRepo;
use agtrs_runtime::tool::ErasedTool;

/// Convenience builder — creates all git tools from a shared repo.
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

    /// All three original read-only git tools: status, diff, log.
    pub fn all(&self) -> Vec<Arc<ErasedTool>> {
        vec![
            Arc::new(GitStatusTool::new(Arc::clone(&self.repo))) as Arc<ErasedTool>,
            Arc::new(GitDiffTool::new(Arc::clone(&self.repo), self.repo_root())) as Arc<ErasedTool>,
            Arc::new(GitLogTool::new(Arc::clone(&self.repo), self.repo_root())) as Arc<ErasedTool>,
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
            Arc::new(GitDiffTool::new(Arc::clone(&self.repo), self.repo_root())) as Arc<ErasedTool>,
        ]
    }

    fn repo_root(&self) -> std::path::PathBuf {
        self.repo.root().to_path_buf()
    }
}
