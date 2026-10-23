//! The coordinator that owns a parent orchestration manifest.
//!
//! Port of `agenthicc.worktrees.coordinator.ParallelCoordinator`. It is the
//! single writer of [`ParallelManifest`]s: it creates an orchestration from a
//! clean coordinator tree, records the task graph (rejecting cycles and
//! unknown dependencies), allocates one Git worktree per dispatched task,
//! derives completion evidence from Git, integrates finished work into the
//! parent branch, and reconciles a manifest with Git's view after a crash.
//!
//! `worktrees integrate` and `worktrees recover` are thin wrappers over
//! [`ParallelCoordinator::integrate_task`] and [`ParallelCoordinator::recover`].
//!
//! # What is not ported, and why it does not change these commands
//!
//! agenthicc's `dispatch` also runs the repository's worktree *environment*
//! setup (`.agenthicc/environment.toml`, its own trust store) and hands the
//! task to the background supervisor, which launches a worker session.
//! Neither layer exists in xaft, so [`ParallelCoordinator::dispatch`] does
//! what agenthicc does when it has no supervisor: allocate the worktree and
//! record the task as `running` under a fresh worker-session id. Integration
//! and recovery read only the manifest and Git, so they behave identically
//! whoever did the work in the worktree.

use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::error::WorktreeError;
use super::manager::{IntegrationResult, WorktreeManager};
use super::model::{
    OrchestrationStatus, ParallelManifest, ParallelTask, TaskStatus, WorkerResult, WorktreeRecord,
    WorktreeStatus, has_cycle, now_secs,
};
use super::store::ManifestStore;

/// Owns one parent session's orchestrations.
#[derive(Debug, Clone)]
pub struct ParallelCoordinator {
    manager: WorktreeManager,
    store: ManifestStore,
    parent_session_id: String,
    run_id: String,
}

impl ParallelCoordinator {
    /// A coordinator over `manager`'s repository, persisting to `store`.
    pub fn new(
        manager: WorktreeManager,
        store: ManifestStore,
        parent_session_id: impl Into<String>,
    ) -> Self {
        Self {
            manager,
            store,
            parent_session_id: parent_session_id.into(),
            run_id: String::new(),
        }
    }

    /// Attach the goal run this coordinator works for.
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = run_id.into();
        self
    }

    /// The Git manager.
    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    /// The manifest store this coordinator writes.
    pub fn store(&self) -> &ManifestStore {
        &self.store
    }

    /// Create a manifest from a clean coordinator worktree.
    pub fn create(
        &self,
        orchestration_id: Option<&str>,
    ) -> Result<ParallelManifest, WorktreeError> {
        self.manager.ensure_clean(None)?;
        let id = match orchestration_id {
            Some(id) => id.to_string(),
            None => uuid::Uuid::new_v4().simple().to_string(),
        };
        let now = now_secs();
        let manifest = ParallelManifest {
            orchestration_id: id,
            parent_session_id: self.parent_session_id.clone(),
            run_id: self.run_id.clone(),
            repository: self.manager.repository().display().to_string(),
            parent_branch: self.manager.branch(None)?,
            base_commit: self.manager.head(None)?,
            status: OrchestrationStatus::Planning,
            tasks: Vec::new(),
            worktrees: Vec::new(),
            created_at: now,
            updated_at: now,
            last_error: String::new(),
            verification: Vec::new(),
        };
        self.store.create(manifest)
    }

    /// Read one manifest.
    pub fn manifest(&self, orchestration_id: &str) -> Result<ParallelManifest, WorktreeError> {
        self.store.get(orchestration_id)
    }

    /// Add a task to the graph.
    pub fn add_task(
        &self,
        orchestration_id: &str,
        task_id: &str,
        description: &str,
        dependencies: &[String],
    ) -> Result<ParallelManifest, WorktreeError> {
        let mut manifest = self.manifest(orchestration_id)?;
        let task_id = task_id.trim();
        if task_id.is_empty() || description.trim().is_empty() {
            return Err(WorktreeError::Coordinator(
                "task_id and description must not be empty".into(),
            ));
        }
        if manifest.task(task_id).is_some() {
            return Err(WorktreeError::Coordinator(format!(
                "task already exists: {task_id}"
            )));
        }
        let known: BTreeSet<&str> = manifest.tasks.iter().map(|t| t.task_id.as_str()).collect();
        let unknown: Vec<&str> = dependencies
            .iter()
            .map(String::as_str)
            .filter(|d| !known.contains(d))
            .collect();
        if dependencies.iter().any(|d| d == task_id) {
            return Err(WorktreeError::Coordinator(
                "a task cannot depend on itself".into(),
            ));
        }
        if !unknown.is_empty() {
            return Err(WorktreeError::Coordinator(format!(
                "unknown task dependencies: {}",
                unknown.join(", ")
            )));
        }
        let now = now_secs();
        manifest.tasks.push(ParallelTask {
            task_id: task_id.to_string(),
            description: description.trim().to_string(),
            dependencies: dependencies.to_vec(),
            status: TaskStatus::Pending,
            worker_session_id: String::new(),
            worktree_id: String::new(),
            result: None,
            error: String::new(),
            created_at: now,
            updated_at: now,
        });
        manifest.status = OrchestrationStatus::Planning;
        if has_cycle(&manifest.tasks) {
            return Err(WorktreeError::Coordinator(
                "task dependency graph contains a cycle".into(),
            ));
        }
        manifest.touch();
        self.store.save(manifest)
    }

    /// Pending tasks whose dependencies are all completed or integrated.
    pub fn ready_tasks(&self, orchestration_id: &str) -> Result<Vec<ParallelTask>, WorktreeError> {
        let manifest = self.manifest(orchestration_id)?;
        let done: BTreeSet<&str> = manifest
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Integrated))
            .map(|t| t.task_id.as_str())
            .collect();
        Ok(manifest
            .tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && t.dependencies.iter().all(|d| done.contains(d.as_str()))
            })
            .cloned()
            .collect())
    }

    /// Allocate a worktree for a ready task and mark it running.
    pub fn dispatch(
        &self,
        orchestration_id: &str,
        task_id: &str,
    ) -> Result<ParallelTask, WorktreeError> {
        let mut manifest = self.manifest(orchestration_id)?;
        let task = manifest
            .task(task_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task_id.to_string()))?;
        if task.status != TaskStatus::Pending {
            return Err(WorktreeError::Coordinator(format!(
                "task {task_id} is not pending"
            )));
        }
        if !self
            .ready_tasks(orchestration_id)?
            .iter()
            .any(|t| t.task_id == task_id)
        {
            return Err(WorktreeError::Coordinator(format!(
                "task dependencies are not complete: {task_id}"
            )));
        }
        if manifest.base_commit != self.manager.head(None)? {
            return Err(WorktreeError::Coordinator(
                "coordinator HEAD changed after the orchestration was created".into(),
            ));
        }
        let record = if task.worktree_id.is_empty() {
            let planned = planned_worktree_id(orchestration_id, task_id);
            self.manager.create(
                &self.parent_session_id,
                task_id,
                Some(&manifest.base_commit),
                Some(&planned),
                None,
            )?
        } else {
            manifest
                .worktree(&task.worktree_id)
                .cloned()
                .ok_or_else(|| WorktreeError::NotFound(task.worktree_id.clone()))?
        };
        if !Path::new(&record.path).is_dir() {
            return Err(WorktreeError::Coordinator(format!(
                "worker worktree is missing: {}",
                record.path
            )));
        }
        let worker_session_id = uuid::Uuid::new_v4().simple().to_string();
        if manifest.worktree(&record.worktree_id).is_none() {
            manifest.worktrees.insert(0, record.clone());
        }
        manifest.status = OrchestrationStatus::Running;
        let running = {
            let slot = manifest.task_mut(task_id).expect("task was found above");
            slot.status = TaskStatus::Running;
            slot.worker_session_id = worker_session_id;
            slot.worktree_id = record.worktree_id.clone();
            slot.updated_at = now_secs();
            slot.clone()
        };
        manifest.touch();
        self.store.save(manifest)?;
        Ok(running)
    }

    /// Derive completion evidence for a task from Git and record it.
    pub fn inspect_task(
        &self,
        orchestration_id: &str,
        task_id: &str,
    ) -> Result<WorkerResult, WorktreeError> {
        let mut manifest = self.manifest(orchestration_id)?;
        let task = manifest
            .task(task_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task_id.to_string()))?;
        if task.worktree_id.is_empty() {
            return Err(WorktreeError::Coordinator(format!(
                "task has no worktree: {task_id}"
            )));
        }
        let record = manifest
            .worktree(&task.worktree_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task.worktree_id.clone()))?;
        let result = self.manager.complete(&record, &task.worker_session_id)?;
        let status = if result.status == "complete" {
            TaskStatus::Completed
        } else {
            task.status
        };
        if let Some(slot) = manifest.task_mut(task_id) {
            slot.status = status;
            slot.result = Some(result.clone());
            slot.updated_at = now_secs();
        }
        if let Some(slot) = manifest.worktree_mut(&record.worktree_id) {
            slot.status = if status == TaskStatus::Completed {
                WorktreeStatus::Completed
            } else {
                WorktreeStatus::Active
            };
            slot.head_commit = result.head_commit.clone();
            slot.dirty = !result.clean;
            slot.updated_at = now_secs();
        }
        manifest.touch();
        self.store.save(manifest)?;
        Ok(result)
    }

    /// Merge a completed task's branch into the parent branch.
    ///
    /// Port of `integrate_task`: refuses a task without completion evidence and
    /// a coordinator that has left the manifest's parent branch, marks the
    /// orchestration `integrating` before touching Git (so a crash mid-merge is
    /// visible), then records `integrated`, or `conflict`/`reviewing` with the
    /// conflicting paths.
    pub fn integrate_task(
        &self,
        orchestration_id: &str,
        task_id: &str,
    ) -> Result<IntegrationResult, WorktreeError> {
        let mut manifest = self.manifest(orchestration_id)?;
        let task = manifest
            .task(task_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task_id.to_string()))?;
        if !matches!(task.status, TaskStatus::Completed | TaskStatus::Conflict) {
            return Err(WorktreeError::Coordinator(format!(
                "task must have completion evidence before integration: {task_id}"
            )));
        }
        let record = manifest
            .worktree(&task.worktree_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task.worktree_id.clone()))?;
        if self.manager.branch(None)? != manifest.parent_branch {
            return Err(WorktreeError::Coordinator(
                "coordinator is not on the manifest parent branch; refusing integration".into(),
            ));
        }

        let parent_branch = manifest.parent_branch.clone();
        manifest.status = OrchestrationStatus::Integrating;
        manifest.set_task_status(task_id, TaskStatus::Integrating);
        manifest.set_worktree_status(&record.worktree_id, WorktreeStatus::Integrating);
        manifest.touch();
        self.store.save(manifest)?;

        let result = self.manager.integrate(&record, Some(&parent_branch))?;

        let head = result.head_commit.clone();
        let message = result.message.clone();
        let conflicts = result.conflict_paths.clone();
        let ok = result.ok;
        self.store.update(orchestration_id, |refreshed| {
            if ok {
                refreshed.status = OrchestrationStatus::Running;
                refreshed.set_task_status(task_id, TaskStatus::Integrated);
                if let Some(slot) = refreshed.worktree_mut(&record.worktree_id) {
                    slot.status = WorktreeStatus::Integrated;
                    slot.head_commit = head.clone();
                    slot.updated_at = now_secs();
                }
            } else {
                refreshed.status = OrchestrationStatus::Reviewing;
                refreshed.last_error = message.clone();
                if let Some(slot) = refreshed.task_mut(task_id) {
                    slot.status = TaskStatus::Conflict;
                    slot.error = message.clone();
                    slot.updated_at = now_secs();
                }
                if let Some(slot) = refreshed.worktree_mut(&record.worktree_id) {
                    slot.status = WorktreeStatus::Conflict;
                    slot.conflict_paths = conflicts.clone();
                    slot.last_error = message.clone();
                    slot.updated_at = now_secs();
                }
            }
        })?;
        Ok(result)
    }

    /// Rebase a task's branch onto the parent branch.
    pub fn rebase_task(
        &self,
        orchestration_id: &str,
        task_id: &str,
    ) -> Result<IntegrationResult, WorktreeError> {
        let manifest = self.manifest(orchestration_id)?;
        let task = manifest
            .task(task_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task_id.to_string()))?;
        let record = manifest
            .worktree(&task.worktree_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task.worktree_id.clone()))?;
        let result = self
            .manager
            .rebase(&record, Some(&manifest.parent_branch))?;
        if result.ok {
            let head = result.head_commit.clone();
            self.store.update(orchestration_id, |m| {
                if let Some(slot) = m.task_mut(task_id) {
                    slot.status = TaskStatus::Completed;
                    slot.error.clear();
                    slot.updated_at = now_secs();
                }
                if let Some(slot) = m.worktree_mut(&record.worktree_id) {
                    slot.status = WorktreeStatus::Completed;
                    slot.head_commit = head.clone();
                    slot.conflict_paths.clear();
                    slot.updated_at = now_secs();
                }
            })?;
        }
        Ok(result)
    }

    /// Remove an integrated task's worktree and branch.
    pub fn cleanup_task(
        &self,
        orchestration_id: &str,
        task_id: &str,
    ) -> Result<ParallelTask, WorktreeError> {
        let manifest = self.manifest(orchestration_id)?;
        let task = manifest
            .task(task_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task_id.to_string()))?;
        if task.status != TaskStatus::Integrated {
            return Err(WorktreeError::Coordinator(
                "only integrated workers may be cleaned up".into(),
            ));
        }
        let record = manifest
            .worktree(&task.worktree_id)
            .cloned()
            .ok_or_else(|| WorktreeError::NotFound(task.worktree_id.clone()))?;
        let removed = self.manager.remove(&record, false)?;
        let saved = self.store.update(orchestration_id, |m| {
            m.set_task_status(task_id, TaskStatus::Cleaned);
            if let Some(slot) = m.worktree_mut(&record.worktree_id) {
                *slot = removed.clone();
            }
        })?;
        Ok(saved.task(task_id).cloned().expect("task was found above"))
    }

    /// Reconcile a manifest with Git's actual worktrees after a crash.
    ///
    /// Port of `recover`: every recorded worktree whose path is gone becomes
    /// `orphaned` with `worktree path is missing`; every Git worktree the
    /// manifest does not know about is appended as an orphan; the
    /// orchestration is marked `recovering`.
    pub fn recover(&self, orchestration_id: &str) -> Result<ParallelManifest, WorktreeError> {
        let mut manifest = self.manifest(orchestration_id)?;
        let actual = self.manager.recover()?;
        let mut worktrees: Vec<WorktreeRecord> = Vec::new();
        for mut record in manifest.worktrees.drain(..) {
            if !Path::new(&record.path).is_dir() {
                record.status = WorktreeStatus::Orphaned;
                record.last_error = "worktree path is missing".into();
                record.updated_at = now_secs();
            }
            worktrees.push(record);
        }
        for record in actual {
            if !worktrees.iter().any(|w| same_path(&w.path, &record.path)) {
                worktrees.push(record);
            }
        }
        manifest.worktrees = worktrees;
        manifest.status = OrchestrationStatus::Recovering;
        manifest.touch();
        self.store.save(manifest)
    }
}

/// Compare two recorded paths, resolving symlinks where they still exist.
fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (Path::new(a).canonicalize(), Path::new(b).canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A deterministic worktree id for `(orchestration, task)`.
///
/// agenthicc derives it with `uuid5(NAMESPACE_URL, "agenthicc:{o}:{t}")`, so a
/// dispatch retried after a crash finds the worktree it already created. A
/// SHA-256 prefix gives the same property without a UUIDv5 dependency.
fn planned_worktree_id(orchestration_id: &str, task_id: &str) -> String {
    let digest = Sha256::digest(format!("xaft:{orchestration_id}:{task_id}").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::super::manager::test_support::Repo;
    use super::*;

    fn coordinator(repo: &Repo, store: &Path) -> ParallelCoordinator {
        ParallelCoordinator::new(
            WorktreeManager::with_root(&repo.path, &repo.worktrees).unwrap(),
            ManifestStore::new(store),
            "parent-1",
        )
    }

    #[test]
    fn create_records_the_clean_coordinator_state() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        let manifest = c.create(Some("orch-1")).unwrap();
        assert_eq!(manifest.parent_branch, "main");
        assert_eq!(manifest.base_commit, c.manager().head(None).unwrap());
        assert_eq!(manifest.status, OrchestrationStatus::Planning);
        assert_eq!(manifest.parent_session_id, "parent-1");
    }

    #[test]
    fn the_task_graph_rejects_bad_edges() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        c.create(Some("o")).unwrap();
        c.add_task("o", "a", "first", &[]).unwrap();
        let err = c.add_task("o", "a", "again", &[]).unwrap_err();
        assert_eq!(err.to_string(), "task already exists: a");
        let err = c.add_task("o", "b", "x", &["zzz".into()]).unwrap_err();
        assert_eq!(err.to_string(), "unknown task dependencies: zzz");
        let err = c.add_task("o", "c", "x", &["c".into()]).unwrap_err();
        assert_eq!(err.to_string(), "a task cannot depend on itself");
        c.add_task("o", "b", "second", &["a".into()]).unwrap();
        let ready: Vec<String> = c
            .ready_tasks("o")
            .unwrap()
            .into_iter()
            .map(|t| t.task_id)
            .collect();
        assert_eq!(ready, vec!["a"]);
    }

    #[test]
    fn dispatch_inspect_integrate_round_trip() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        c.create(Some("o")).unwrap();
        c.add_task("o", "a", "write a feature", &[]).unwrap();

        let task = c.dispatch("o", "a").unwrap();
        assert_eq!(task.status, TaskStatus::Running);
        assert!(!task.worker_session_id.is_empty());
        let manifest = c.manifest("o").unwrap();
        let record = manifest.worktree(&task.worktree_id).unwrap().clone();
        assert_eq!(manifest.status, OrchestrationStatus::Running);

        // Integration needs completion evidence first.
        let err = c.integrate_task("o", "a").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("task must have completion evidence")
        );

        repo.commit_in(Path::new(&record.path), "feature.txt", "done\n");
        let result = c.inspect_task("o", "a").unwrap();
        assert_eq!(result.status, "complete");
        assert_eq!(
            c.manifest("o").unwrap().task("a").unwrap().status,
            TaskStatus::Completed
        );

        let integration = c.integrate_task("o", "a").unwrap();
        assert!(integration.ok);
        let after = c.manifest("o").unwrap();
        assert_eq!(after.task("a").unwrap().status, TaskStatus::Integrated);
        assert_eq!(
            after.worktree(&record.worktree_id).unwrap().status,
            WorktreeStatus::Integrated
        );
        assert_eq!(after.status, OrchestrationStatus::Running);
        assert!(repo.path.join("feature.txt").is_file());

        let cleaned = c.cleanup_task("o", "a").unwrap();
        assert_eq!(cleaned.status, TaskStatus::Cleaned);
        assert!(!Path::new(&record.path).exists());
    }

    #[test]
    fn a_conflict_moves_the_orchestration_to_reviewing() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        c.create(Some("o")).unwrap();
        c.add_task("o", "a", "edit readme", &[]).unwrap();
        let task = c.dispatch("o", "a").unwrap();
        let path = c
            .manifest("o")
            .unwrap()
            .worktree(&task.worktree_id)
            .unwrap()
            .path
            .clone();
        repo.commit_in(Path::new(&path), "README.md", "worker\n");
        c.inspect_task("o", "a").unwrap();
        repo.commit_in(&repo.path, "README.md", "coordinator\n");

        let result = c.integrate_task("o", "a").unwrap();
        assert!(!result.ok);
        let after = c.manifest("o").unwrap();
        assert_eq!(after.status, OrchestrationStatus::Reviewing);
        assert_eq!(after.task("a").unwrap().status, TaskStatus::Conflict);
        let record = after.worktree(&task.worktree_id).unwrap();
        assert_eq!(record.status, WorktreeStatus::Conflict);
        assert_eq!(record.conflict_paths, vec!["README.md"]);
    }

    #[test]
    fn dispatch_refuses_a_moved_coordinator_head() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        c.create(Some("o")).unwrap();
        c.add_task("o", "a", "x", &[]).unwrap();
        repo.commit_in(&repo.path, "other.txt", "moved\n");
        let err = c.dispatch("o", "a").unwrap_err();
        assert_eq!(
            err.to_string(),
            "coordinator HEAD changed after the orchestration was created"
        );
    }

    #[test]
    fn recover_marks_missing_paths_and_adopts_unknown_worktrees() {
        let repo = Repo::new();
        let store = tempfile::tempdir().unwrap();
        let c = coordinator(&repo, store.path());
        c.create(Some("o")).unwrap();
        c.add_task("o", "a", "x", &[]).unwrap();
        let task = c.dispatch("o", "a").unwrap();
        let manifest = c.manifest("o").unwrap();
        let recorded = manifest.worktree(&task.worktree_id).unwrap().clone();

        // A worktree Git knows about but the manifest never recorded.
        let stray = c
            .manager()
            .create("someone", "else", None, Some("stray"), None)
            .unwrap();
        // And the recorded worktree's directory disappearing.
        std::fs::remove_dir_all(&recorded.path).unwrap();

        let recovered = c.recover("o").unwrap();
        assert_eq!(recovered.status, OrchestrationStatus::Recovering);
        let missing = recovered.worktree(&recorded.worktree_id).unwrap();
        assert_eq!(missing.status, WorktreeStatus::Orphaned);
        assert_eq!(missing.last_error, "worktree path is missing");
        let adopted = recovered.worktree("stray").unwrap();
        assert_eq!(adopted.status, WorktreeStatus::Orphaned);
        assert!(same_path(&adopted.path, &stray.path));
    }

    #[test]
    fn planned_ids_are_deterministic_and_distinct() {
        assert_eq!(planned_worktree_id("o", "a"), planned_worktree_id("o", "a"));
        assert_ne!(planned_worktree_id("o", "a"), planned_worktree_id("o", "b"));
        assert_eq!(planned_worktree_id("o", "a").len(), 16);
    }
}
