//! Parallel coding orchestration with isolated Git worktrees.
//!
//! Port of `agenthicc.worktrees` (PRD-203): the layer behind `xaft worktrees`
//! (parity matrix rows 50–53) and the `parallel_code_plan` workflow's
//! `parallel_workers` tool group, which writes the manifests `xaft worktrees`
//! reads. It lives in `xaft-tools` so the runtime can own an orchestration;
//! `xaft_cli::worktrees` re-exports it unchanged.
//!
//! | module | agenthicc | role |
//! | --- | --- | --- |
//! | [`model`] | `worktrees/model.py` | durable records, wire-identical JSON |
//! | [`store`] | `worktrees/store.py` | one atomically written manifest per orchestration |
//! | [`manager`] | `worktrees/manager.py` | Git worktree lifecycle via the system `git` |
//! | [`coordinator`] | `worktrees/coordinator.py` | the single writer of manifests |
//! | [`error`] | the exception family | one enum, agenthicc's messages |
//!
//! Manifests live in `$XAFT_HOME/orchestrations/` (default
//! `~/.xaft/orchestrations/`), worktrees in `$XAFT_HOME/worktrees/<repo>/`.

pub mod coordinator;
pub mod error;
pub mod manager;
pub mod model;
pub mod store;

pub use coordinator::ParallelCoordinator;
pub use error::WorktreeError;
pub use manager::{IntegrationResult, WorktreeInspection, WorktreeManager};
pub use model::{
    OrchestrationStatus, ParallelManifest, ParallelTask, TaskStatus, WorkerResult, WorktreeRecord,
    WorktreeStatus,
};
pub use store::ManifestStore;
