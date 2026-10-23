//! Durable models for parallel worker orchestration.
//!
//! Port of `agenthicc.worktrees.model` (313 lines). These types are the
//! durable identity boundary between a coordinator, its worker sessions and
//! Git integration: every value has a JSON representation, so a process can be
//! restarted without reconstructing state from conversational text.
//!
//! # Wire format
//!
//! Every field name and every status string is identical to agenthicc's
//! `to_dict()`, so a manifest written by xaft and one written by agenthicc
//! describe the same thing. `worktrees list --json` is rendered through
//! `serde_json::Value`, whose object type is a `BTreeMap` (serde_json is built
//! without `preserve_order` — see `Cargo.lock`), which reproduces agenthicc's
//! `json.dumps(indent=2, sort_keys=True)` byte for byte.
//!
//! # Two deliberate divergences
//!
//! **Mutability.** agenthicc's models are frozen dataclasses updated by an
//! `evolve(**changes)` call that copies every field. That kwargs shape does not
//! translate to Rust; the equivalent here is an owned struct reached through
//! `ParallelManifest::{task_mut, worktree_mut}`. The observable JSON is the
//! same, and callers mutate in place instead of rebuilding each record.
//!
//! **Unknown status strings.** agenthicc deserialises `WorktreeStatus` and
//! `OrchestrationStatus` leniently (falling back to `orphaned` / `recovering`)
//! but `TaskStatus` strictly, so one unrecognised task status makes the whole
//! manifest raise and silently disappear from `worktrees list`. A durable audit
//! record going missing because of one unknown string is a data-loss-shaped
//! bug, so all three are lenient here, each with a documented fallback. The
//! fallback values are the same ones agenthicc already chose.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Define one status enum with its wire strings and a lenient fallback.
///
/// Generates `ALL`, [`as_str`](Self::as_str), [`parse`](Self::parse), plus
/// `Serialize` (to the bare wire string), a lenient `Deserialize` that falls
/// back to the named variant, and `Display`.
macro_rules! status_enum {
    (
        $(#[$meta:meta])*
        $name:ident, fallback $fallback:ident,
        { $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $(
                $(#[$vmeta])*
                $variant,
            )*
        }

        impl $name {
            /// Every status, in declaration order.
            pub const ALL: &'static [$name] = &[$( $name::$variant ),*];

            /// The wire string, e.g. `"integrating"`.
            pub const fn as_str(self) -> &'static str {
                match self {
 $( $name::$variant => $wire, )*
                }
            }

            /// Parse a wire string, or `None` if unknown.
            pub fn parse(value: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|s| s.as_str() == value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                // Unknown statuses fall back rather than failing the whole
                // manifest; see the module docs for why.
                Ok(Self::parse(&String::deserialize(d)?).unwrap_or($name::$fallback))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

status_enum! {
    /// Lifecycle of one isolated Git worktree owned by one orchestration.
    ///
    /// An unrecognised wire value loads as [`WorktreeStatus::Orphaned`]: a
    /// worktree whose state cannot be established is, by definition, one this
    /// process is not in charge of.
    WorktreeStatus, fallback Orphaned,
    {
        /// Git has been asked for the worktree but has not confirmed it.
        Creating => "creating",
        /// The worktree exists and is the current place to work.
        Active => "active",
        /// The worker finished and left a clean tree.
        Completed => "completed",
        /// A merge into the parent branch is in progress.
        Integrating => "integrating",
        /// The worker branch is merged into the parent branch.
        Integrated => "integrated",
        /// The merge conflicted; the worker worktree is preserved.
        Conflict => "conflict",
        /// The worktree path is missing or the record is otherwise unusable.
        Orphaned => "orphaned",
        /// The worktree has been removed and its branch deleted.
        Removed => "removed",
    }
}

status_enum! {
    /// Lifecycle of one task in an orchestration's dependency graph.
    ///
    /// An unrecognised wire value loads as [`TaskStatus::Blocked`]: the safest
    /// reading of "this task is in a state we do not recognise" is that it
    /// must not be dispatched, and `Blocked` is the one status that
    /// [`ParallelCoordinator::ready_tasks`](crate::worktrees::ParallelCoordinator::ready_tasks)
    /// will never hand out.
    TaskStatus, fallback Blocked,
    {
        /// Not started; waiting on its dependencies.
        Pending => "pending",
        /// A worktree has been allocated and the worker is being launched.
        Dispatching => "dispatching",
        /// Accepted by the worker launcher but waiting for capacity.
        Queued => "queued",
        /// A worker session is executing the task.
        Running => "running",
        /// Finished, with machine-derived completion evidence.
        Completed => "completed",
        /// Finished with an error.
        Failed => "failed",
        /// Will not progress without operator action.
        Blocked => "blocked",
        /// Being merged into the parent branch.
        Integrating => "integrating",
        /// Merged into the parent branch.
        Integrated => "integrated",
        /// The merge conflicted; the worker worktree is preserved.
        Conflict => "conflict",
        /// Abandoned by the operator.
        Cancelled => "cancelled",
        /// The worktree has been removed and the branch deleted.
        Cleaned => "cleaned",
    }
}

status_enum! {
    /// Lifecycle of a whole parent orchestration.
    ///
    /// An unrecognised wire value loads as [`OrchestrationStatus::Recovering`],
    /// the same fallback agenthicc already uses: the correct action for a
    /// coordinator whose state cannot be read is to reconcile with Git.
    OrchestrationStatus, fallback Recovering,
    {
        /// Tasks are being defined; no worktree exists yet.
        Planning => "planning",
        /// At least one worker is executing.
        Running => "running",
        /// A merge conflicted and needs an operator decision.
        Reviewing => "reviewing",
        /// A merge is in progress.
        Integrating => "integrating",
        /// Results are being checked after integration.
        Verifying => "verifying",
        /// Every task is integrated.
        Complete => "complete",
        /// The orchestration cannot continue.
        Failed => "failed",
        /// Being reconciled against real Git worktrees.
        Recovering => "recovering",
    }
}

impl Default for TaskStatus {
    /// agenthicc's `ParallelTask.status` default, and what a stored task with
    /// no `status` key loads as.
    fn default() -> Self {
        TaskStatus::Pending
    }
}

impl Default for OrchestrationStatus {
    /// agenthicc's `ParallelManifest.status` default, and what a stored
    /// manifest with no `status` key loads as.
    fn default() -> Self {
        OrchestrationStatus::Planning
    }
}

/// What a stored worktree record with no `status` key loads as.
///
/// agenthicc's `WorktreeRecord.from_mapping` uses `ORPHANED` for a missing or
/// unknown status (the constructor default `CREATING` only applies to records
/// built in memory).
fn orphaned() -> WorktreeStatus {
    WorktreeStatus::Orphaned
}

/// Seconds since the Unix epoch, as agenthicc's `time.time()` produces.
pub(crate) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One isolated Git worktree owned by one parent orchestration.
///
/// Every field but `worktree_id` defaults when absent, as agenthicc's
/// `WorktreeRecord.from_mapping` does, so a sparse record written by another
/// tool still loads instead of hiding its whole manifest from `worktrees list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    /// Short identifier, also the worktree directory name.
    pub worktree_id: String,
    /// Absolute path of the repository that owns this worktree.
    #[serde(default)]
    pub repository: String,
    /// Absolute path of the worktree itself.
    #[serde(default)]
    pub path: String,
    /// Branch checked out in the worktree.
    #[serde(default)]
    pub branch: String,
    /// Commit the worktree was branched from. Immutable for the worktree's life.
    #[serde(default)]
    pub base_commit: String,
    /// Session that owns the orchestration, not the worker.
    #[serde(default)]
    pub owner_session_id: String,
    /// Task this worktree exists for.
    #[serde(default)]
    pub task_id: String,
    /// Current lifecycle state.
    #[serde(default = "orphaned")]
    pub status: WorktreeStatus,
    /// Commit currently checked out in the worktree.
    #[serde(default)]
    pub head_commit: String,
    /// Whether the worktree has uncommitted changes.
    #[serde(default)]
    pub dirty: bool,
    /// Paths in conflict, when [`WorktreeStatus::Conflict`].
    #[serde(default)]
    pub conflict_paths: Vec<String>,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub created_at: f64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub updated_at: f64,
    /// Last error recorded against this worktree.
    #[serde(default)]
    pub last_error: String,
    /// Receipts of environment setup runs, newest last (at most 64).
    ///
    /// Carried for wire parity with agenthicc's manifest: xaft does not run
    /// worktree environment setup, so it never appends one, but a manifest
    /// written by agenthicc keeps its receipts through an xaft rewrite.
    #[serde(default)]
    pub environment_receipts: Vec<String>,
}

impl WorktreeRecord {
    /// Whether the worktree directory still exists on disk.
    pub fn path_exists(&self) -> bool {
        Path::new(&self.path).is_dir()
    }
}

/// Machine-derived completion evidence for a worker.
///
/// Every field is derived from Git, never from a worker's self-report, so a
/// worker cannot claim work it did not commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerResult {
    /// Task this evidence belongs to.
    #[serde(default)]
    pub task_id: String,
    /// Session that ran the worker, if one was launched.
    #[serde(default)]
    pub worker_session_id: String,
    /// Worktree the evidence was gathered from.
    #[serde(default)]
    pub worktree_id: String,
    /// Commit the worker started from.
    #[serde(default)]
    pub base_commit: String,
    /// Commit the worker finished at.
    #[serde(default)]
    pub head_commit: String,
    /// Commits the worker added.
    #[serde(default)]
    pub commits: Vec<String>,
    /// Files the worker changed.
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// Whether the worktree is clean.
    #[serde(default)]
    pub clean: bool,
    /// `"complete"`, `"incomplete"` or `"dirty"`.
    #[serde(default = "incomplete_status")]
    pub status: String,
    /// Worker-supplied summary. Never trusted for state transitions.
    #[serde(default)]
    pub summary: String,
    /// Tests the worker claims to have run.
    #[serde(default)]
    pub tests: Vec<String>,
    /// Error recorded while gathering evidence.
    #[serde(default)]
    pub error: String,
}

fn incomplete_status() -> String {
    "incomplete".to_string()
}

/// One task in an orchestration's dependency graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelTask {
    /// Identifier, unique within the orchestration.
    #[serde(default)]
    pub task_id: String,
    /// What the task is.
    #[serde(default)]
    pub description: String,
    /// Tasks that must be integrated before this one may run.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Current lifecycle state.
    #[serde(default)]
    pub status: TaskStatus,
    /// Session running the worker, once dispatched.
    #[serde(default)]
    pub worker_session_id: String,
    /// Worktree allocated to this task, once dispatched.
    #[serde(default)]
    pub worktree_id: String,
    /// Machine-derived completion evidence.
    #[serde(default)]
    pub result: Option<WorkerResult>,
    /// Last error recorded against this task.
    #[serde(default)]
    pub error: String,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub created_at: f64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub updated_at: f64,
}

/// Crash-recoverable parent orchestration manifest.
///
/// This is the record `worktrees list` and `worktrees show` read, and the one
/// `worktrees integrate` and `worktrees recover` reconcile against real Git.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelManifest {
    /// Identifier, also the manifest file name.
    pub orchestration_id: String,
    /// Session that owns this orchestration.
    #[serde(default)]
    pub parent_session_id: String,
    /// Goal run this orchestration belongs to, if any.
    #[serde(default)]
    pub run_id: String,
    /// Absolute path of the repository being orchestrated.
    #[serde(default)]
    pub repository: String,
    /// Branch integrations land on.
    #[serde(default)]
    pub parent_branch: String,
    /// Commit every worker branched from.
    #[serde(default)]
    pub base_commit: String,
    /// Current lifecycle state.
    #[serde(default)]
    pub status: OrchestrationStatus,
    /// The task dependency graph.
    #[serde(default)]
    pub tasks: Vec<ParallelTask>,
    /// Worktrees allocated to tasks.
    #[serde(default)]
    pub worktrees: Vec<WorktreeRecord>,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub created_at: f64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub updated_at: f64,
    /// Last error recorded against the orchestration.
    #[serde(default)]
    pub last_error: String,
    /// Verification steps recorded against the orchestration.
    #[serde(default)]
    pub verification: Vec<String>,
}

impl ParallelManifest {
    /// Look up a task by id.
    pub fn task(&self, task_id: &str) -> Option<&ParallelTask> {
        self.tasks.iter().find(|t| t.task_id == task_id)
    }

    /// Look up a task by id for mutation.
    pub fn task_mut(&mut self, task_id: &str) -> Option<&mut ParallelTask> {
        self.tasks.iter_mut().find(|t| t.task_id == task_id)
    }

    /// Look up a worktree record by id.
    pub fn worktree(&self, worktree_id: &str) -> Option<&WorktreeRecord> {
        self.worktrees.iter().find(|w| w.worktree_id == worktree_id)
    }

    /// Look up a worktree record by id for mutation.
    pub fn worktree_mut(&mut self, worktree_id: &str) -> Option<&mut WorktreeRecord> {
        self.worktrees
            .iter_mut()
            .find(|w| w.worktree_id == worktree_id)
    }

    /// Set the status of one task and stamp its `updated_at`.
    pub fn set_task_status(&mut self, task_id: &str, status: TaskStatus) -> bool {
        match self.task_mut(task_id) {
            Some(task) => {
                task.status = status;
                task.updated_at = now_secs();
                true
            }
            None => false,
        }
    }

    /// Set the status of one worktree and stamp its `updated_at`.
    pub fn set_worktree_status(&mut self, worktree_id: &str, status: WorktreeStatus) -> bool {
        match self.worktree_mut(worktree_id) {
            Some(record) => {
                record.status = status;
                record.updated_at = now_secs();
                true
            }
            None => false,
        }
    }

    /// Stamp the manifest's `updated_at`. Call before every [`ManifestStore::save`](crate::worktrees::ManifestStore::save).
    pub fn touch(&mut self) {
        self.updated_at = now_secs();
    }
}

/// True when the task dependency graph contains a cycle.
///
/// Port of `agenthicc.worktrees.coordinator._has_cycle`. Rejecting a cycle at
/// `add_task` time is what stops `ready_tasks` from deadlocking: a task can
/// never become ready if its own dependency waits on it.
pub(crate) fn has_cycle(tasks: &[ParallelTask]) -> bool {
    fn visit(
        graph: &std::collections::BTreeMap<String, Vec<String>>,
        node: &str,
        visiting: &mut std::collections::BTreeSet<String>,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }
        if visited.contains(node) {
            return false;
        }
        visiting.insert(node.to_string());
        for dep in graph.get(node).map(|v| v.as_slice()).unwrap_or_default() {
            if visit(graph, dep, visiting, visited) {
                return true;
            }
        }
        visiting.remove(node);
        visited.insert(node.to_string());
        false
    }

    let graph: std::collections::BTreeMap<String, Vec<String>> = tasks
        .iter()
        .map(|t| (t.task_id.clone(), t.dependencies.clone()))
        .collect();
    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    graph
        .keys()
        .any(|node| visit(&graph, node, &mut visiting, &mut visited))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_strings_match_agenthicc() {
        assert_eq!(WorktreeStatus::Creating.as_str(), "creating");
        assert_eq!(TaskStatus::Dispatching.as_str(), "dispatching");
        assert_eq!(TaskStatus::Cleaned.as_str(), "cleaned");
        assert_eq!(OrchestrationStatus::Recovering.as_str(), "recovering");
        assert_eq!(OrchestrationStatus::Verifying.as_str(), "verifying");
        assert_eq!(TaskStatus::Queued.as_str(), "queued");
        assert_eq!(WorktreeStatus::ALL.len(), 8);
        assert_eq!(TaskStatus::ALL.len(), 12);
        assert_eq!(OrchestrationStatus::ALL.len(), 8);
    }

    #[test]
    fn unknown_status_falls_back_instead_of_failing_the_manifest() {
        let json = r#"{
            "orchestration_id": "o1",
            "status": "from-the-future",
            "tasks": [{"task_id": "t1", "status": "also-unknown"}],
            "worktrees": [{"worktree_id": "w1", "status": "nope"}]
        }"#;
        let m: ParallelManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.status, OrchestrationStatus::Recovering);
        assert_eq!(m.tasks[0].status, TaskStatus::Blocked);
        assert_eq!(m.worktrees[0].status, WorktreeStatus::Orphaned);
    }

    #[test]
    fn a_manifest_missing_optional_fields_still_loads() {
        let m: ParallelManifest = serde_json::from_str(r#"{"orchestration_id": "o1"}"#).unwrap();
        assert_eq!(m.orchestration_id, "o1");
        assert!(m.tasks.is_empty());
        assert_eq!(m.status, OrchestrationStatus::Planning);
        assert_eq!(m.base_commit, "");
    }

    #[test]
    fn json_keys_are_sorted_like_agenthicc_sort_keys() {
        let m = ParallelManifest {
            orchestration_id: "o1".into(),
            parent_session_id: "s1".into(),
            run_id: String::new(),
            repository: "/tmp/repo".into(),
            parent_branch: "main".into(),
            base_commit: "abc".into(),
            status: OrchestrationStatus::Planning,
            tasks: vec![],
            worktrees: vec![],
            created_at: 1.0,
            updated_at: 2.0,
            last_error: String::new(),
            verification: vec![],
        };
        let value = serde_json::to_value(&m).unwrap();
        let rendered = serde_json::to_string_pretty(&value).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "base_commit",
                "created_at",
                "last_error",
                "orchestration_id",
                "parent_branch",
                "parent_session_id",
                "repository",
                "run_id",
                "status",
                "tasks",
                "updated_at",
                "verification",
                "worktrees",
            ]
        );
        assert!(rendered.starts_with("{\n  \"base_commit\": \"abc\","));
    }

    #[test]
    fn a_dependency_cycle_is_rejected() {
        let task = |id: &str, deps: &[&str]| ParallelTask {
            task_id: id.into(),
            description: String::new(),
            dependencies: deps.iter().map(|d| (*d).to_string()).collect(),
            status: TaskStatus::Pending,
            worker_session_id: String::new(),
            worktree_id: String::new(),
            result: None,
            error: String::new(),
            created_at: 0.0,
            updated_at: 0.0,
        };
        assert!(!has_cycle(&[task("a", &[]), task("b", &["a"])]));
        assert!(has_cycle(&[task("a", &["b"]), task("b", &["a"])]));
        assert!(has_cycle(&[task("a", &["a"])]));
        assert!(has_cycle(&[
            task("a", &["b"]),
            task("b", &["c"]),
            task("c", &["a"])
        ]));
    }

    #[test]
    fn task_and_worktree_lookups_report_absence() {
        let mut m = ParallelManifest {
            orchestration_id: "o1".into(),
            parent_session_id: String::new(),
            run_id: String::new(),
            repository: String::new(),
            parent_branch: String::new(),
            base_commit: String::new(),
            status: OrchestrationStatus::Planning,
            tasks: vec![ParallelTask {
                task_id: "t1".into(),
                description: String::new(),
                dependencies: vec![],
                status: TaskStatus::Pending,
                worker_session_id: String::new(),
                worktree_id: String::new(),
                result: None,
                error: String::new(),
                created_at: 0.0,
                updated_at: 0.0,
            }],
            worktrees: vec![],
            created_at: 0.0,
            updated_at: 0.0,
            last_error: String::new(),
            verification: vec![],
        };
        assert!(m.task("t1").is_some());
        assert!(m.task("nope").is_none());
        assert!(m.task_mut("nope").is_none());
        assert!(m.worktree("nope").is_none());
        assert!(m.set_task_status("nope", TaskStatus::Completed) == false);
        assert!(m.set_task_status("t1", TaskStatus::Completed));
        assert_eq!(m.task("t1").unwrap().status, TaskStatus::Completed);
        assert!(m.task("t1").unwrap().updated_at > 0.0);
        assert!(!m.set_worktree_status("w1", WorktreeStatus::Active));
    }
}
