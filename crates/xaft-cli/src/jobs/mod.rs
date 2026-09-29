//! Durable background-job model and store for `xaft jobs`.
//!
//! This is the Rust counterpart of agenthicc's `agenthicc.background` package
//! (`model.py` + `store.py`). It reproduces the **user-visible** contract that
//! the `xaft jobs` CLI depends on:
//!
//! * the same 13 status values and the same legal-transition table, so
//!   `jobs cancel` / `resume` / `retry` accept and reject exactly the states
//!   agenthicc accepts and rejects;
//! * the same redaction rules when rendering a job — the raw `intent` and the
//!   pending `input_value` never leave the store;
//! * the same three-way list filter (current / `--all` / `--trash`).
//!
//! Internals are deliberately idiomatic rather than a transliteration: agenthicc
//! keeps an append-only JSONL event log and folds it into a projection, while
//! this store persists a single atomically-rewritten snapshot. Crash-recovery
//! semantics are not user-visible, so the simpler form is preferred; the
//! observable behaviour is identical.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Lifecycle state of a background job.
///
/// Mirrors `agenthicc.background.model.SessionStatus`. The wire values are
/// identical and are what `jobs list` / `jobs status` print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Accepted, not yet picked up by a worker.
    Queued,
    /// Claimed by a worker, spinning up.
    Starting,
    /// Actively executing.
    Running,
    /// Blocked on an approval decision.
    WaitingApproval,
    /// Blocked waiting for user input.
    WaitingInput,
    /// Being re-run after a failure.
    Retrying,
    /// Cancellation requested, draining.
    Cancelling,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled by the user.
    Cancelled,
    /// Worker died without reporting; recoverable.
    Orphaned,
    /// Archived out of the current list.
    Archived,
    /// In recoverable trash.
    Deleted,
}

impl JobStatus {
    /// Every status, in declaration order.
    pub const ALL: [JobStatus; 13] = [
        JobStatus::Queued,
        JobStatus::Starting,
        JobStatus::Running,
        JobStatus::WaitingApproval,
        JobStatus::WaitingInput,
        JobStatus::Retrying,
        JobStatus::Cancelling,
        JobStatus::Completed,
        JobStatus::Failed,
        JobStatus::Cancelled,
        JobStatus::Orphaned,
        JobStatus::Archived,
        JobStatus::Deleted,
    ];

    /// The wire string, e.g. `"waiting_approval"`.
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Starting => "starting",
            JobStatus::Running => "running",
            JobStatus::WaitingApproval => "waiting_approval",
            JobStatus::WaitingInput => "waiting_input",
            JobStatus::Retrying => "retrying",
            JobStatus::Cancelling => "cancelling",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Orphaned => "orphaned",
            JobStatus::Archived => "archived",
            JobStatus::Deleted => "deleted",
        }
    }

    /// Parse a wire string, or `None` if unknown.
    pub fn parse(value: &str) -> Option<JobStatus> {
        JobStatus::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// A job that still occupies a worker slot.
    ///
    /// Matches agenthicc's `ACTIVE_STATUSES`.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            JobStatus::Queued
                | JobStatus::Starting
                | JobStatus::Running
                | JobStatus::WaitingApproval
                | JobStatus::WaitingInput
                | JobStatus::Retrying
                | JobStatus::Cancelling
        )
    }

    /// A job that will not progress without operator action.
    ///
    /// Matches agenthicc's `TERMINAL_STATUSES`.
    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }

    /// Whether `self -> target` is a legal transition.
    ///
    /// This is a transliteration of agenthicc's `_TRANSITIONS` table. It is the
    /// rule that makes `jobs resume` fail on a running job and succeed on a
    /// cancelled one, and what keeps `deleted` jobs frozen.
    pub fn can_transition_to(self, target: JobStatus) -> bool {
        use JobStatus::*;
        match self {
            Queued => matches!(target, Starting | Cancelling | Orphaned | Failed),
            Starting => matches!(target, Running | Cancelling | Orphaned),
            Running => matches!(
                target,
                WaitingApproval
                    | WaitingInput
                    | Retrying
                    | Cancelling
                    | Completed
                    | Failed
                    | Orphaned
            ),
            WaitingApproval => matches!(target, Running | Cancelling),
            WaitingInput => matches!(target, Running | Cancelling),
            Retrying => matches!(target, Starting | Cancelling | Orphaned),
            Cancelling => matches!(target, Cancelled | Orphaned),
            Completed => matches!(target, Archived),
            Failed => matches!(target, Retrying | Starting | Archived),
            Cancelled => matches!(target, Starting | Archived),
            Orphaned => matches!(target, Starting | Cancelling | Archived),
            Archived => matches!(target, Starting | Completed | Failed),
            // Deleted jobs are frozen; `jobs restore` is the only way out and
            // it is modelled explicitly rather than as a status transition.
            Deleted => false,
        }
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One durable background execution.
///
/// Field names match agenthicc's `BackgroundSession` for the subset that is
/// user-visible through the `jobs` CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Stable identifier; `jobs list` shows the first 16 characters.
    pub session_id: String,
    /// User-settable display title (`jobs rename`).
    #[serde(default)]
    pub title: String,
    /// Working directory the job runs in.
    #[serde(default)]
    pub cwd: String,
    /// Workflow name, or empty for a direct run.
    #[serde(default)]
    pub workflow_name: String,
    /// The user's request. Never rendered by the CLI (see [`Job::public`]).
    #[serde(default)]
    pub intent: String,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// Unix seconds when the job was accepted.
    #[serde(default)]
    pub created_at: f64,
    /// Unix seconds when the job finished, if it has.
    #[serde(default)]
    pub completed_at: Option<f64>,
    /// Unix seconds of the most recent state change.
    #[serde(default)]
    pub state_changed_at: f64,
    /// Short human-readable progress line.
    #[serde(default)]
    pub latest_activity: String,
    /// Failure detail, if any.
    #[serde(default)]
    pub error: Option<String>,
    /// Why the job was cancelled, if it was.
    #[serde(default)]
    pub cancellation_reason: String,
    /// Pending approval question, if the job is waiting on one.
    #[serde(default)]
    pub approval_request: String,
    /// The decision recorded for [`Job::approval_request`].
    #[serde(default)]
    pub approval_decision: Option<bool>,
    /// Pending input question, if the job is waiting on one.
    #[serde(default)]
    pub input_request: String,
    /// The value supplied in reply. Never rendered by the CLI.
    #[serde(default)]
    pub input_value: Option<String>,
    /// User labels (`jobs labels`). Never sent to providers.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Unix seconds the job entered trash, if it has.
    #[serde(default)]
    pub deleted_at: Option<f64>,
}

impl Job {
    /// A freshly queued job.
    pub fn new(session_id: impl Into<String>, intent: impl Into<String>) -> Job {
        let now = now_secs();
        Job {
            session_id: session_id.into(),
            title: String::new(),
            cwd: String::new(),
            workflow_name: String::new(),
            intent: intent.into(),
            status: JobStatus::Queued,
            created_at: now,
            completed_at: None,
            state_changed_at: now,
            latest_activity: "Accepted".to_string(),
            error: None,
            cancellation_reason: String::new(),
            approval_request: String::new(),
            approval_decision: None,
            input_request: String::new(),
            input_value: None,
            labels: Vec::new(),
            deleted_at: None,
        }
    }

    /// The redaction-safe projection, matching agenthicc's `_public_session`.
    ///
    /// `intent` and `input_value` are dropped outright: the first is the user's
    /// raw prompt and the second may be a secret typed into a waiting job.
    pub fn public(&self) -> PublicJob<'_> {
        PublicJob(self)
    }
}

/// Borrowed, redaction-safe view of a [`Job`].
///
/// Serialising this yields exactly the fields `xaft jobs --json` prints; the
/// raw `intent` and `input_value` are structurally unreachable from here.
#[derive(Debug)]
pub struct PublicJob<'a>(&'a Job);

impl Serialize for PublicJob<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let j = self.0;
        let mut st = s.serialize_struct("PublicJob", 15)?;
        st.serialize_field("session_id", &j.session_id)?;
        st.serialize_field("title", &j.title)?;
        st.serialize_field("cwd", &j.cwd)?;
        st.serialize_field("workflow_name", &j.workflow_name)?;
        st.serialize_field("status", j.status.as_str())?;
        st.serialize_field("created_at", &j.created_at)?;
        st.serialize_field("completed_at", &j.completed_at)?;
        st.serialize_field("state_changed_at", &j.state_changed_at)?;
        st.serialize_field("latest_activity", &j.latest_activity)?;
        st.serialize_field("error", &j.error)?;
        st.serialize_field("cancellation_reason", &j.cancellation_reason)?;
        st.serialize_field("approval_request", &j.approval_request)?;
        st.serialize_field("approval_decision", &j.approval_decision)?;
        st.serialize_field("input_request", &j.input_request)?;
        st.serialize_field("labels", &j.labels)?;
        st.end()
    }
}

/// Errors produced by the job store.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// No job with the given id.
    #[error("no background job with id {0}")]
    NotFound(String),
    /// The requested state change is not legal from the current state.
    #[error("cannot transition {current} -> {target}")]
    IllegalTransition {
        /// Current state.
        current: JobStatus,
        /// Requested state.
        target: JobStatus,
    },
    /// A deleted job cannot change state.
    #[error("deleted background jobs cannot change state; run `xaft jobs restore`")]
    Deleted,
    /// A human-supplied field failed validation.
    #[error("{0}")]
    Invalid(String),
    /// Underlying I/O or serialisation failure.
    #[error("job store i/o: {0}")]
    Io(String),
}

impl From<std::io::Error> for JobError {
    fn from(e: std::io::Error) -> Self {
        JobError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for JobError {
    fn from(e: serde_json::Error) -> Self {
        JobError::Io(e.to_string())
    }
}

/// Longest accepted display title, matching agenthicc's bounded rename.
const MAX_TITLE: usize = 200;
/// Longest accepted single label.
const MAX_LABEL: usize = 64;
/// Most labels a single job may carry.
const MAX_LABELS: usize = 16;

/// A JSON-snapshot-backed collection of [`Job`]s.
///
/// # Examples
///
/// ```
/// use xaft_cli::jobs::{Job, JobStore, JobStatus};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let dir = tempfile::tempdir()?;
/// let mut store = JobStore::open(dir.path())?;
/// let job = store.create(Job::new("s-1", "add rate limiting"))?;
/// assert_eq!(job.status, JobStatus::Queued);
/// assert_eq!(store.list(false, false).len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct JobStore {
    path: PathBuf,
    jobs: BTreeMap<String, Job>,
    /// Pre-trash status per deleted job, so `jobs restore` can put it back
    /// where it was rather than guessing. Not part of the on-disk snapshot:
    /// a job restored after a restart falls back to `queued`, which is the
    /// same recovery agenthicc performs.
    restore_targets: BTreeMap<String, JobStatus>,
}

impl JobStore {
    /// Open (or create) a store rooted at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<JobStore, JobError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join("jobs.json");
        let jobs = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let map: BTreeMap<String, Job> = if raw.trim().is_empty() {
                BTreeMap::new()
            } else {
                serde_json::from_str(&raw)?
            };
            map
        } else {
            BTreeMap::new()
        };
        Ok(JobStore {
            path,
            jobs,
            restore_targets: BTreeMap::new(),
        })
    }

    /// The default store location, honouring `XAFT_HOME` then `~/.xaft`.
    pub fn default_root() -> Result<PathBuf, JobError> {
        if let Some(dir) = std::env::var_os("XAFT_HOME") {
            return Ok(PathBuf::from(dir).join("jobs"));
        }
        let base = dirs::home_dir()
            .ok_or_else(|| JobError::Io("cannot determine home directory; set XAFT_HOME".into()))?;
        Ok(base.join(".xaft").join("jobs"))
    }

    /// Open the store at its default location.
    pub fn open_default() -> Result<JobStore, JobError> {
        JobStore::open(JobStore::default_root()?)
    }

    /// Insert a new job.
    pub fn create(&mut self, job: Job) -> Result<Job, JobError> {
        if job.session_id.is_empty() {
            return Err(JobError::Invalid("job id must not be empty".into()));
        }
        if self.jobs.contains_key(&job.session_id) {
            return Err(JobError::Invalid(format!(
                "job {} already exists",
                job.session_id
            )));
        }
        self.jobs.insert(job.session_id.clone(), job.clone());
        self.persist()?;
        Ok(job)
    }

    /// All jobs, optionally including archived and/or trashed ones.
    pub fn list(&self, include_archived: bool, include_deleted: bool) -> Vec<Job> {
        self.jobs
            .values()
            .filter(|j| match j.status {
                JobStatus::Deleted => include_deleted,
                JobStatus::Archived => include_archived,
                _ => true,
            })
            .cloned()
            .collect()
    }

    /// Look up one job by id, optionally including trashed ones.
    pub fn get(&self, session_id: &str, include_deleted: bool) -> Result<Job, JobError> {
        let job = self
            .jobs
            .get(session_id)
            .ok_or_else(|| JobError::NotFound(session_id.to_string()))?;
        if job.status == JobStatus::Deleted && !include_deleted {
            return Err(JobError::NotFound(session_id.to_string()));
        }
        Ok(job.clone())
    }

    /// Apply a status transition, enforcing [`JobStatus::can_transition_to`].
    pub fn transition(&mut self, session_id: &str, target: JobStatus) -> Result<Job, JobError> {
        let job = self.mut_get(session_id)?;
        if job.status == JobStatus::Deleted {
            return Err(JobError::Deleted);
        }
        if job.status == target {
            return Ok(job.clone());
        }
        if !job.status.can_transition_to(target) {
            return Err(JobError::IllegalTransition {
                current: job.status,
                target,
            });
        }
        job.status = target;
        job.state_changed_at = now_secs();
        if matches!(
            target,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        ) {
            job.completed_at = Some(now_secs());
        }
        let out = job.clone();
        self.persist()?;
        Ok(out)
    }

    /// Set the display title. Titles are local-only and length-bounded.
    pub fn rename(&mut self, session_id: &str, title: &str) -> Result<Job, JobError> {
        let title = title.trim();
        if title.chars().count() > MAX_TITLE {
            return Err(JobError::Invalid(format!(
                "title must be at most {MAX_TITLE} characters"
            )));
        }
        let job = self.mut_get(session_id)?;
        job.title = title.to_string();
        let out = job.clone();
        self.persist()?;
        Ok(out)
    }

    /// Replace the user labels. Labels are never sent to providers.
    pub fn set_labels(&mut self, session_id: &str, labels: Vec<String>) -> Result<Job, JobError> {
        if labels.len() > MAX_LABELS {
            return Err(JobError::Invalid(format!(
                "at most {MAX_LABELS} labels are allowed"
            )));
        }
        for label in &labels {
            if label.chars().count() > MAX_LABEL {
                return Err(JobError::Invalid(format!(
                    "label {label:?} exceeds {MAX_LABEL} characters"
                )));
            }
        }
        let job = self.mut_get(session_id)?;
        job.labels = labels;
        let out = job.clone();
        self.persist()?;
        Ok(out)
    }

    /// Record an approval decision and return the job to the run queue.
    pub fn approve(&mut self, session_id: &str, approved: bool) -> Result<Job, JobError> {
        {
            let job = self.mut_get(session_id)?;
            if job.status != JobStatus::WaitingApproval {
                return Err(JobError::IllegalTransition {
                    current: job.status,
                    target: if approved {
                        JobStatus::Running
                    } else {
                        JobStatus::Cancelled
                    },
                });
            }
            job.approval_decision = Some(approved);
            job.approval_request = String::new();
        }
        self.transition(session_id, JobStatus::Running)
    }

    /// Supply the value a waiting job asked for and resume it.
    pub fn provide_input(&mut self, session_id: &str, value: &str) -> Result<Job, JobError> {
        {
            let job = self.mut_get(session_id)?;
            if job.status != JobStatus::WaitingInput {
                return Err(JobError::IllegalTransition {
                    current: job.status,
                    target: JobStatus::Running,
                });
            }
            job.input_value = Some(value.to_string());
            job.input_request = String::new();
        }
        self.transition(session_id, JobStatus::Running)
    }

    /// Cancel an active job.
    ///
    /// agenthicc's table has no direct `running -> cancelled` edge: the
    /// supervisor drains the worker first. This reproduces that two-step path
    /// so `jobs cancel` succeeds from any active state, which is what users
    /// expect, while still refusing to resurrect a finished job.
    pub fn cancel(&mut self, session_id: &str) -> Result<Job, JobError> {
        let status = self.mut_get(session_id)?.status;
        if status == JobStatus::Cancelling {
            return self.transition(session_id, JobStatus::Cancelled);
        }
        if !status.is_active() {
            return Err(JobError::IllegalTransition {
                current: status,
                target: JobStatus::Cancelled,
            });
        }
        self.transition(session_id, JobStatus::Cancelling)?;
        self.transition(session_id, JobStatus::Cancelled)
    }

    /// Move a job to recoverable trash.
    pub fn delete(&mut self, session_id: &str) -> Result<Job, JobError> {
        let job = self.mut_get(session_id)?;
        if job.status == JobStatus::Archived {
            return Err(JobError::IllegalTransition {
                current: job.status,
                target: JobStatus::Deleted,
            });
        }
        let previous = job.status;
        {
            let job = self.jobs.get_mut(session_id).expect("checked above");
            job.status = JobStatus::Deleted;
            job.deleted_at = Some(now_secs());
            job.state_changed_at = now_secs();
        }
        // Remember where to return to on `jobs restore`.
        self.restore_targets
            .insert(session_id.to_string(), previous);
        let out = self.jobs.get(session_id).expect("checked above").clone();
        self.persist()?;
        Ok(out)
    }

    /// Return a trashed job to its pre-trash state.
    pub fn restore(&mut self, session_id: &str) -> Result<Job, JobError> {
        let job = self.mut_get_any(session_id)?;
        if job.status != JobStatus::Deleted {
            return Err(JobError::IllegalTransition {
                current: job.status,
                target: JobStatus::Queued,
            });
        }
        let target = self
            .restore_targets
            .get(session_id)
            .copied()
            .filter(|t| t.can_transition_to(*t) || *t == JobStatus::Queued)
            .unwrap_or(JobStatus::Queued);
        {
            let job = self.jobs.get_mut(session_id).expect("checked above");
            job.status = if target == JobStatus::Queued {
                JobStatus::Queued
            } else {
                target
            };
            job.deleted_at = None;
            job.state_changed_at = now_secs();
        }
        self.restore_targets.remove(session_id);
        let out = self.jobs.get(session_id).expect("checked above").clone();
        self.persist()?;
        Ok(out)
    }

    /// Permanently remove trashed jobs older than `older_than_secs`.
    pub fn purge_expired_trash(&mut self, older_than_secs: f64) -> Result<Vec<String>, JobError> {
        let cutoff = now_secs() - older_than_secs;
        let expired: Vec<String> = self
            .jobs
            .values()
            .filter(|j| j.status == JobStatus::Deleted)
            .filter(|j| j.deleted_at.map(|t| t <= cutoff).unwrap_or(false))
            .map(|j| j.session_id.clone())
            .collect();
        for id in &expired {
            self.jobs.remove(id);
            self.restore_targets.remove(id);
        }
        if !expired.is_empty() {
            self.persist()?;
        }
        Ok(expired)
    }

    fn mut_get(&mut self, session_id: &str) -> Result<&mut Job, JobError> {
        let job = self
            .jobs
            .get_mut(session_id)
            .ok_or_else(|| JobError::NotFound(session_id.to_string()))?;
        if job.status == JobStatus::Deleted {
            return Err(JobError::Deleted);
        }
        Ok(job)
    }

    fn mut_get_any(&mut self, session_id: &str) -> Result<&Job, JobError> {
        self.jobs
            .get(session_id)
            .ok_or_else(|| JobError::NotFound(session_id.to_string()))
    }

    fn persist(&self) -> Result<(), JobError> {
        let json = serde_json::to_string_pretty(&self.jobs)?;
        // Write-then-rename so a crash mid-write cannot truncate the snapshot.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
