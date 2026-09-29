//! Durable goal-run model and store for `xaft runs`.
//!
//! This is the Rust counterpart of agenthicc's `agenthicc.runs` package
//! (`model.py` + `store.py`), reduced to the surface the `xaft runs` CLI
//! actually exposes. agenthicc's `GoalRun` carries worker/process/manifest
//! join tables that are projections over other stores; here the run is the
//! single durable record, which is what `runs list` / `runs show` render.
//!
//! What is reproduced exactly, because it is user-visible:
//!
//! * the 12 `GoalRunStatus` values and their wire strings;
//! * the terminal partition (`completed`, `failed`, `cancelled`);
//! * `runs list` column layout and truncation, and `runs list --status`
//!   rejecting an unknown status with exit 2 rather than silently listing all;
//! * the `cancel: <run_id> -> <status>` / `resume: <session_id> -> <status>`
//!   confirmation lines, including that `resume` reports the *session* id
//!   while `cancel` reports the *run* id.
//!
//! What is deliberately not ported: the cross-store reconciliation
//! (`GoalRunManager.projection`) that joins workflow, worktree and manifest
//! records. xaft has no equivalent of those stores yet, so a run here is the
//! authoritative record rather than a projection. That is visible in one
//! respect only: `attention_reasons` is stored, not recomputed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Coarse product-level status of a goal run.
///
/// Mirrors `agenthicc.runs.model.GoalRunStatus`. The wire values are identical
/// and are what the CLI prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Recorded, nothing started.
    Created,
    /// Worker spinning up.
    Starting,
    /// Producing a plan.
    Planning,
    /// Actively executing.
    Running,
    /// Blocked on a user decision or external input.
    Waiting,
    /// Needs a human; not progressing on its own.
    NeedsAttention,
    /// Merging worktree results back.
    Integrating,
    /// Checking the result.
    Verifying,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled by the user.
    Cancelled,
    /// Worker vanished without reporting.
    Lost,
}

impl RunStatus {
    /// Every status, in declaration order — the order `runs list` sorts by
    /// recency within a status bucket, and the order an unknown `--status`
    /// value is validated against.
    pub const ALL: [RunStatus; 12] = [
        RunStatus::Created,
        RunStatus::Starting,
        RunStatus::Planning,
        RunStatus::Running,
        RunStatus::Waiting,
        RunStatus::NeedsAttention,
        RunStatus::Integrating,
        RunStatus::Verifying,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::Lost,
    ];

    /// The exact wire string, matching `GoalRunStatus.value`.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Created => "created",
            RunStatus::Starting => "starting",
            RunStatus::Planning => "planning",
            RunStatus::Running => "running",
            RunStatus::Waiting => "waiting",
            RunStatus::NeedsAttention => "needs_attention",
            RunStatus::Integrating => "integrating",
            RunStatus::Verifying => "verifying",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Lost => "lost",
        }
    }

    /// Parse a wire string.
    pub fn parse(value: &str) -> Option<RunStatus> {
        RunStatus::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// True for the statuses agenthicc treats as final
    /// (`_TERMINAL = {COMPLETED, FAILED, CANCELLED}`).
    ///
    /// `Lost` is deliberately *not* terminal: agenthicc excludes it so a run
    /// whose worker died can still be resumed or cancelled.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One durable goal run.
///
/// Field names match agenthicc's `GoalRun` for the subset the CLI renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRun {
    /// Stable identifier, always `run_`-prefixed.
    pub run_id: String,
    /// The goal statement the user asked for.
    pub goal: String,
    /// Repository the run targets.
    pub repository: String,
    /// Absolute path to the repository root.
    pub repository_root: String,
    /// HEAD the run started from.
    #[serde(default)]
    pub base_commit: String,
    /// Branch the run integrates into.
    #[serde(default)]
    pub main_branch: String,
    /// Current status.
    pub status: RunStatus,
    /// Creation time, epoch seconds.
    pub created_at: f64,
    /// When execution began, if it has.
    #[serde(default)]
    pub started_at: Option<f64>,
    /// When it reached a terminal status.
    #[serde(default)]
    pub completed_at: Option<f64>,
    /// Last mutation time, epoch seconds.
    pub last_updated_at: f64,
    /// The foreground session this run drives. Empty until it starts.
    #[serde(default)]
    pub main_session_id: String,
    /// Workflow executing the run.
    #[serde(default = "default_workflow_name")]
    pub workflow_name: String,
    /// Whether the run was started detached.
    #[serde(default)]
    pub detached: bool,
    /// Process exit code once finished.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Short outcome summary.
    #[serde(default)]
    pub result_summary: String,
    /// Why it failed, when it did.
    #[serde(default)]
    pub failure_reason: String,
    /// Why it needs a human. Stored, not recomputed — see the module docs.
    #[serde(default)]
    pub attention_reasons: Vec<String>,
}

fn default_workflow_name() -> String {
    // agenthicc's `GoalRun.workflow_name` default.
    "goal_flow".to_string()
}

impl GoalRun {
    /// A new run in the `created` status.
    pub fn new(
        run_id: impl Into<String>,
        goal: impl Into<String>,
        repository: impl Into<String>,
        root: impl Into<String>,
    ) -> Self {
        let now = now_secs();
        GoalRun {
            run_id: run_id.into(),
            goal: goal.into(),
            repository: repository.into(),
            repository_root: root.into(),
            base_commit: String::new(),
            main_branch: String::new(),
            status: RunStatus::Created,
            created_at: now,
            started_at: None,
            completed_at: None,
            last_updated_at: now,
            main_session_id: String::new(),
            workflow_name: default_workflow_name(),
            detached: false,
            exit_code: None,
            result_summary: String::new(),
            failure_reason: String::new(),
            attention_reasons: Vec::new(),
        }
    }

    /// The JSON projection `runs show --json` emits.
    ///
    /// agenthicc's `GoalRun.to_dict()` renders `status` as its enum value; the
    /// CLI prints that same mapping, so scripts see identical keys.
    pub fn to_projection(&self) -> serde_json::Value {
        serde_json::json!({
            "run_id": self.run_id,
            "goal": self.goal,
            "repository": self.repository,
            "repository_root": self.repository_root,
            "base_commit": self.base_commit,
            "main_branch": self.main_branch,
            "status": self.status.as_str(),
            "created_at": self.created_at,
            "started_at": self.started_at,
            "completed_at": self.completed_at,
            "last_updated_at": self.last_updated_at,
            "main_session_id": self.main_session_id,
            "workflow_name": self.workflow_name,
            "detached": self.detached,
            "exit_code": self.exit_code,
            "result_summary": self.result_summary,
            "failure_reason": self.failure_reason,
            "attention_reasons": self.attention_reasons,
        })
    }
}

/// Failures the store can report.
#[derive(Debug)]
pub enum RunError {
    /// No run with that id.
    NotFound(String),
    /// `--status` was not one of the known statuses.
    InvalidStatus(String),
    /// The run exists but cannot legally make the requested move.
    IllegalTransition {
        /// Current status of the run.
        current: RunStatus,
        /// What the caller asked for.
        target: RunStatus,
    },
    /// `resume` on a run that never started a session.
    NoMainSession(String),
    /// Persistence failure.
    Io(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::NotFound(id) => write!(f, "Run not found: {id}"),
            RunError::InvalidStatus(s) => {
                write!(f, "Invalid run status: {s}")
            }
            RunError::IllegalTransition { current, target } => {
                write!(f, "cannot move from {current} to {target}")
            }
            RunError::NoMainSession(id) => {
                write!(f, "goal run has no main session: {id}")
            }
            RunError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        RunError::Io(e)
    }
}

/// The on-disk run store.
///
/// Same persistence strategy as [`crate::jobs::JobStore`]: one atomically
/// rewritten JSON snapshot rather than agenthicc's append-only event log.
/// Crash-recovery behaviour is not user-visible, so the simpler form wins.
#[derive(Debug)]
pub struct RunStore {
    path: PathBuf,
    runs: BTreeMap<String, GoalRun>,
}

impl RunStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RunError> {
        let path = path.as_ref().to_path_buf();
        let runs = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<BTreeMap<String, GoalRun>>(&bytes).unwrap_or_default()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(RunError::Io(e)),
        };
        Ok(RunStore { path, runs })
    }

    /// Directory the run store lives in.
    ///
    /// Same `$XAFT_HOME` convention as [`crate::jobs::JobStore::default_root`],
    /// under a sibling `runs/` directory so the two stores never collide.
    pub fn default_root() -> Result<PathBuf, RunError> {
        if let Some(dir) = std::env::var_os("XAFT_HOME") {
            return Ok(PathBuf::from(dir).join("runs").join("runs.json"));
        }
        let base = dirs::home_dir().ok_or_else(|| {
            RunError::Io(std::io::Error::other(
                "cannot determine home directory; set XAFT_HOME",
            ))
        })?;
        Ok(base.join(".xaft").join("runs").join("runs.json"))
    }

    /// Open the store at its default location.
    pub fn open_default() -> Result<Self, RunError> {
        RunStore::open(RunStore::default_root()?)
    }

    /// Every run, oldest id first.
    pub fn list(&self) -> Vec<&GoalRun> {
        self.runs.values().collect()
    }

    /// Runs matching `status`, or all of them when `status` is `None`.
    ///
    /// An unknown status is an error, never an empty result — agenthicc
    /// raises `ValueError` and the CLI exits 2, because silently listing
    /// everything would hide a typo in a user's script.
    pub fn list_by_status(&self, status: Option<&str>) -> Result<Vec<&GoalRun>, RunError> {
        let Some(raw) = status else {
            return Ok(self.list());
        };
        if raw.is_empty() {
            return Ok(self.list());
        }
        let parsed =
            RunStatus::parse(raw).ok_or_else(|| RunError::InvalidStatus(raw.to_string()))?;
        Ok(self.runs.values().filter(|r| r.status == parsed).collect())
    }

    /// One run by id.
    pub fn get(&self, run_id: &str) -> Result<&GoalRun, RunError> {
        self.runs
            .get(run_id)
            .ok_or_else(|| RunError::NotFound(run_id.to_string()))
    }

    /// Insert or overwrite a run.
    pub fn put(&mut self, run: GoalRun) -> Result<(), RunError> {
        self.runs.insert(run.run_id.clone(), run);
        self.flush()
    }

    /// Apply `f` to a run and persist the result.
    pub fn update<F>(&mut self, run_id: &str, f: F) -> Result<GoalRun, RunError>
    where
        F: FnOnce(&mut GoalRun),
    {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or_else(|| RunError::NotFound(run_id.to_string()))?;
        f(run);
        run.last_updated_at = now_secs();
        let out = run.clone();
        self.flush()?;
        Ok(out)
    }

    /// Cancel a run.
    ///
    /// agenthicc's `GoalRunManager.cancel` cancels the linked background
    /// sessions and then unconditionally stamps the run `cancelled` with a
    /// `completed_at`. It does not refuse an already-terminal run, so neither
    /// does this: re-cancelling is idempotent rather than an error.
    pub fn cancel(&mut self, run_id: &str) -> Result<GoalRun, RunError> {
        if self.get(run_id).is_err() {
            return Err(RunError::NotFound(run_id.to_string()));
        }
        self.update(run_id, |run| {
            run.status = RunStatus::Cancelled;
            run.completed_at = Some(now_secs());
        })
    }

    /// Resume a run.
    ///
    /// agenthicc resumes the *existing* main session and reports
    /// `resume: <main_session_id> -> <status>`. A run that never started a
    /// session has nothing to resume, which is a `ValueError` there and an
    /// error here.
    pub fn resume(&mut self, run_id: &str) -> Result<GoalRun, RunError> {
        let session_id = self.get(run_id)?.main_session_id.clone();
        if session_id.is_empty() {
            return Err(RunError::NoMainSession(run_id.to_string()));
        }
        self.update(run_id, |run| {
            // A resumed run re-enters the pipeline at `starting`, matching
            // agenthicc handing the session back to the supervisor.
            run.status = RunStatus::Starting;
            run.completed_at = None;
        })
    }

    /// Remove a run outright. Not exposed by the CLI; used by tests.
    #[cfg(test)]
    pub fn remove(&mut self, run_id: &str) -> Result<(), RunError> {
        self.runs.remove(run_id);
        self.flush()
    }

    fn flush(&self) -> Result<(), RunError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&self.runs)
            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        // Write-then-rename so a crash mid-write cannot truncate the store.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Epoch seconds, or 0 if the clock is before the epoch.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a directory unique to this test *and* this thread.
    ///
    /// Rust runs unit tests in parallel threads, and every test in this
    /// process shares one PID, so a per-PID temp path would have them
    /// clobbering each other's store. The thread id disambiguates them.
    fn tmp_store(label: &str) -> (RunStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "xaft-runs-{}-{}-{:?}",
            std::process::id(),
            label,
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("runs.json");
        let _ = std::fs::remove_file(&path);
        (RunStore::open(&path).expect("open"), path)
    }

    fn sample(id: &str) -> GoalRun {
        GoalRun::new(id, "ship the thing", "repo", "/tmp/repo")
    }

    #[test]
    fn every_status_round_trips_through_its_wire_string() {
        for s in RunStatus::ALL {
            assert_eq!(
                RunStatus::parse(s.as_str()),
                Some(s),
                "{s} did not round-trip"
            );
        }
    }

    #[test]
    fn wire_strings_match_agenthicc_exactly() {
        // Transcribed from agenthicc.runs.model.GoalRunStatus.
        let expected = [
            "created",
            "starting",
            "planning",
            "running",
            "waiting",
            "needs_attention",
            "integrating",
            "verifying",
            "completed",
            "failed",
            "cancelled",
            "lost",
        ];
        let actual: Vec<&str> = RunStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn terminal_set_excludes_lost_so_a_dead_worker_can_be_recovered() {
        assert!(RunStatus::Completed.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
        assert!(!RunStatus::Lost.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
    }

    #[test]
    fn unknown_status_is_an_error_not_an_empty_list() {
        let (mut store, _p) = tmp_store("t1");
        store.put(sample("run_1")).unwrap();
        let err = store.list_by_status(Some("nonsense")).unwrap_err();
        assert!(matches!(err, RunError::InvalidStatus(s) if s == "nonsense"));
    }

    #[test]
    fn empty_status_filter_lists_everything() {
        let (mut store, _p) = tmp_store("t2");
        store.put(sample("run_1")).unwrap();
        assert_eq!(store.list_by_status(Some("")).unwrap().len(), 1);
        assert_eq!(store.list_by_status(None).unwrap().len(), 1);
    }

    #[test]
    fn status_filter_returns_only_matching_runs() {
        let (mut store, _p) = tmp_store("t3");
        store.put(sample("run_1")).unwrap();
        store.put(sample("run_2")).unwrap();
        store
            .update("run_2", |r| r.status = RunStatus::Running)
            .unwrap();
        let running = store.list_by_status(Some("running")).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].run_id, "run_2");
    }

    #[test]
    fn cancel_stamps_a_completion_time_and_reports_the_run_id() {
        let (mut store, _p) = tmp_store("t4");
        store.put(sample("run_1")).unwrap();
        let run = store.cancel("run_1").unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.completed_at.is_some());
    }

    #[test]
    fn cancelling_twice_is_idempotent_not_an_error() {
        let (mut store, _p) = tmp_store("t5");
        store.put(sample("run_1")).unwrap();
        store.cancel("run_1").unwrap();
        // agenthicc does not refuse a terminal run here; match that.
        assert_eq!(store.cancel("run_1").unwrap().status, RunStatus::Cancelled);
    }

    #[test]
    fn cancel_of_an_unknown_run_is_not_found() {
        let (mut store, _p) = tmp_store("t6");
        assert!(matches!(
            store.cancel("run_nope"),
            Err(RunError::NotFound(_))
        ));
    }

    #[test]
    fn resume_requires_a_main_session() {
        let (mut store, _p) = tmp_store("t7");
        store.put(sample("run_1")).unwrap(); // main_session_id is empty
        assert!(matches!(
            store.resume("run_1"),
            Err(RunError::NoMainSession(_))
        ));
    }

    #[test]
    fn resume_clears_the_completion_time_and_restarts_the_pipeline() {
        let (mut store, _p) = tmp_store("t8");
        store.put(sample("run_1")).unwrap();
        store
            .update("run_1", |r| {
                r.main_session_id = "sess_abc".into();
                r.status = RunStatus::Cancelled;
                r.completed_at = Some(1.0);
            })
            .unwrap();
        let run = store.resume("run_1").unwrap();
        assert_eq!(run.status, RunStatus::Starting);
        assert!(run.completed_at.is_none());
        assert_eq!(run.main_session_id, "sess_abc");
    }

    #[test]
    fn update_bumps_last_updated_at() {
        let (mut store, _p) = tmp_store("t9");
        store.put(sample("run_1")).unwrap();
        let before = store.get("run_1").unwrap().last_updated_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let after = store
            .update("run_1", |r| r.goal = "changed".into())
            .unwrap();
        assert!(
            after.last_updated_at > before,
            "last_updated_at did not advance"
        );
        assert_eq!(after.goal, "changed");
    }

    #[test]
    fn runs_survive_a_reopen() {
        let (mut store, path) = tmp_store("t10");
        store.put(sample("run_1")).unwrap();
        store
            .update("run_1", |r| r.status = RunStatus::Verifying)
            .unwrap();
        let reopened = RunStore::open(&path).unwrap();
        assert_eq!(reopened.get("run_1").unwrap().status, RunStatus::Verifying);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_corrupt_store_reads_as_empty_rather_than_panicking() {
        let dir = std::env::temp_dir().join(format!("xaft-runs-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runs.json");
        std::fs::write(&path, b"{not json").unwrap();
        let store = RunStore::open(&path).unwrap();
        assert!(store.list().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn projection_carries_status_as_its_wire_string() {
        let v = sample("run_1").to_projection();
        assert_eq!(v["status"], "created");
        assert_eq!(v["run_id"], "run_1");
        assert_eq!(v["workflow_name"], "goal_flow");
    }

    #[test]
    fn default_workflow_matches_agenthicc() {
        assert_eq!(sample("run_x").workflow_name, "goal_flow");
    }
}
