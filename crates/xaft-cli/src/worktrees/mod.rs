//! Parallel coding orchestration with isolated Git worktrees.
//!
//! The orchestration store — durable records, the manifest store, the Git
//! worktree manager and the coordinator — is `xaft_tools::worktrees`, so the
//! runtime can own an orchestration as well as `xaft worktrees` (parity matrix
//! rows 50–53). This module re-exports it unchanged under its historical path
//! and adds the CLI's column formatting.
//!
//! Manifests live in `$XAFT_HOME/orchestrations/` (default
//! `~/.xaft/orchestrations/`), worktrees in `$XAFT_HOME/worktrees/<repo>/`.

pub use xaft_tools::worktrees::{
    IntegrationResult, ManifestStore, OrchestrationStatus, ParallelCoordinator, ParallelManifest,
    ParallelTask, TaskStatus, WorkerResult, WorktreeError, WorktreeInspection, WorktreeManager,
    WorktreeRecord, WorktreeStatus,
};
pub use xaft_tools::worktrees::{coordinator, manager, model, store};

/// The error type and the CLI's column helpers.
pub mod error {
    pub use xaft_tools::worktrees::error::*;

    /// Render an id the way `worktrees list` does: the first 16 characters.
    pub(crate) fn short_id(id: &str) -> String {
        id.chars().take(16).collect()
    }

    /// Left-align into a fixed-width field, matching Python's `f"{value:12}"`.
    ///
    /// agenthicc formats the status with `{status:12}`, which pads on the
    /// right and never truncates: the longest status is `"integrating"` at 11
    /// characters. An unknown status longer than the field is truncated,
    /// because silently overflowing the column would break every other line's
    /// alignment.
    pub(crate) fn pad_status(status: &str, width: usize) -> String {
        let count = status.chars().count();
        if count > width {
            return status.chars().take(width).collect();
        }
        let mut out = status.to_string();
        out.push_str(&" ".repeat(width - count));
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn short_id_takes_sixteen_characters() {
            assert_eq!(short_id("0123456789abcdef0123"), "0123456789abcdef");
            assert_eq!(short_id("short"), "short");
        }

        #[test]
        fn pad_status_pads_right_and_never_overflows() {
            assert_eq!(pad_status("running", 12), "running     ");
            assert_eq!(pad_status("integrating", 12), "integrating ");
            // 11 characters is the longest real status, so nothing truncates.
            assert_eq!(pad_status("integrating", 12).chars().count(), 12);
            assert_eq!(pad_status("a-very-long-status", 12).chars().count(), 12);
        }
    }
}
