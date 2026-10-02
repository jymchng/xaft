//! Handler tests for `xaft jobs`.
//!
//! The store model in the sibling `tests/jobs.rs` is well covered, but the
//! HANDLER — `handle_jobs` and the eleven private functions it dispatches to —
//! had no test at all, so every wire string and exit code the command family
//! actually produces was unverified.
//!
//! These follow the convention established in `tests/runs.rs`: assert the
//! returned `ExitCode` (user-visible, and what a script branches on) and the
//! resulting store state (also user-visible, via the next invocation), rather
//! than trying to capture `println!` output. Two handler tests running
//! concurrently would both resolve the same default store, so every test that
//! touches it is serialised behind a process-wide lock.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use xaft_cli::args::{JobsListArgs, JobsStatusArgs, JobsSubcommand, OutputFormat};
use xaft_cli::commands::jobs::handle_jobs;
use xaft_cli::jobs::{Job, JobStatus, JobStore};
use xaft_runtime::ExitCode;

/// Serialise the tests that repoint `XAFT_HOME` at the default store.
fn xaft_home_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Point `XAFT_HOME` at a throwaway dir for as long as the guard lives, and
/// restore the previous value on drop so a panicking test cannot leak it.
///
/// The guard owns the lock for its whole lifetime, not just during
/// construction: two handler tests running concurrently would otherwise both
/// set `XAFT_HOME`, and the second would resolve the first's store.
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
    /// Dropped last, so the env var is restored while the lock is still held.
    _lock: MutexGuard<'static, ()>,
}

impl HomeGuard {
    fn new() -> HomeGuard {
        let lock = xaft_home_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("XAFT_HOME");
        // SAFETY: `lock` is held for this guard's entire lifetime and is only
        // released after `Drop` restores the previous value.
        unsafe { std::env::set_var("XAFT_HOME", dir.path()) };
        HomeGuard {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }

    /// The directory `handle_jobs` will actually open.
    ///
    /// Derived with the SAME call the handler uses rather than being
    /// reconstructed here, so a future change to the layout cannot silently
    /// point the test at a different store than the command uses.
    fn store_dir(&self) -> PathBuf {
        JobStore::default_root().expect("default root")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: the lock is still held by the guard owning this scope.
        unsafe {
            match self.previous.take() {
                Some(v) => std::env::set_var("XAFT_HOME", v),
                None => std::env::remove_var("XAFT_HOME"),
            }
        }
    }
}

/// Seed a job in a chosen state, then return the guard.
///
/// `Job::new` only ever produces `Queued`; the `status` field is public, so a
/// test can place a job in any state directly. That is what lets these tests
/// drive the legal-transition table from both sides.
fn seeded(status: JobStatus) -> HomeGuard {
    let home = HomeGuard::new();
    let mut store = JobStore::open(home.store_dir()).expect("open store");
    let mut job = Job::new("sess_0123456789abcdef", "do the thing");
    job.title = "nightly refactor".to_string();
    job.workflow_name = "release_review".to_string();
    job.status = status;
    store.create(job).expect("seed job");
    home
}

/// Re-read the seeded job through the default store, proving a handler
/// actually persisted its change rather than only reporting success.
fn reread(home: &HomeGuard) -> Job {
    JobStore::open(home.store_dir())
        .expect("reopen store")
        .get("sess_0123456789abcdef", true)
        .expect("seeded job must still be present")
}

fn list(all: bool, trash: bool, format: OutputFormat) -> JobsSubcommand {
    JobsSubcommand::List(JobsListArgs { all, trash, format })
}

const UNKNOWN: &str = "no_such_session";

// ── list ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_succeeds_on_an_empty_store() {
    let _home = HomeGuard::new();
    let code = handle_jobs(&list(false, false, OutputFormat::Pretty))
        .await
        .expect("list must not error");
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn list_succeeds_with_seeded_jobs() {
    let _home = seeded(JobStatus::Running);
    for format in [OutputFormat::Pretty, OutputFormat::Json] {
        let code = handle_jobs(&list(false, false, format))
            .await
            .expect("list");
        assert_eq!(code, ExitCode::SUCCESS, "format {format:?}");
    }
}

#[tokio::test]
async fn list_honours_the_all_and_trash_filters() {
    let _home = HomeGuard::new();
    let mut store = JobStore::open(_home.store_dir()).expect("open store");

    let mut current = Job::new("sess_current", "current");
    current.status = JobStatus::Running;
    store.create(current).expect("seed current");

    let mut archived = Job::new("sess_archived", "archived");
    archived.status = JobStatus::Archived;
    store.create(archived).expect("seed archived");

    let mut trashed = Job::new("sess_trashed", "trashed");
    trashed.status = JobStatus::Deleted;
    store.create(trashed).expect("seed trashed");

    // The default view is current-only, and neither flag widens it silently:
    // each is a distinct query the handler must issue.
    assert_eq!(store.list(false, false).len(), 1, "current only");
    assert_eq!(store.list(true, false).len(), 2, "current + archived");
    assert_eq!(store.list(false, true).len(), 2, "current + trash");
    assert_eq!(store.list(true, true).len(), 3, "everything");

    for (all, trash) in [(false, false), (true, false), (false, true), (true, true)] {
        let code = handle_jobs(&list(all, trash, OutputFormat::Pretty))
            .await
            .expect("list");
        assert_eq!(code, ExitCode::SUCCESS, "all={all} trash={trash}");
    }
}

// ── status ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_reports_a_known_job() {
    let _home = seeded(JobStatus::Running);
    for format in [OutputFormat::Pretty, OutputFormat::Json] {
        let code = handle_jobs(&JobsSubcommand::Status(JobsStatusArgs {
            session_id: "sess_0123456789abcdef".to_string(),
            format,
        }))
        .await
        .expect("status");
        assert_eq!(code, ExitCode::SUCCESS, "format {format:?}");
    }
}

/// agenthicc reports a missing id as status `not_found` rather than failing,
/// so a script can probe without special-casing the exit code. Both output
/// formats must keep that contract.
#[tokio::test]
async fn status_of_an_unknown_id_succeeds_rather_than_failing() {
    let _home = seeded(JobStatus::Running);
    for format in [OutputFormat::Pretty, OutputFormat::Json] {
        let code = handle_jobs(&JobsSubcommand::Status(JobsStatusArgs {
            session_id: UNKNOWN.to_string(),
            format,
        }))
        .await
        .expect("status must not error");
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "a missing id is not_found, not a failure (format {format:?})"
        );
    }
}

// ── cancel ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_moves_a_cancelling_job_to_cancelled_and_persists_it() {
    let home = seeded(JobStatus::Cancelling);
    let code = handle_jobs(&JobsSubcommand::Cancel {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("cancel");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).status,
        JobStatus::Cancelled,
        "the handler must persist the transition, not just print it"
    );
}

#[tokio::test]
async fn cancel_of_a_queued_job_takes_the_two_step_path_to_cancelled() {
    // `queued` is active, so the handler's two-step path applies just as it
    // does for a running job: queued -> cancelling -> cancelled. The first hop
    // is the only one agenthicc's table allows directly; the second is the
    // drain completing. Cancelling a job that has not started yet is a normal
    // thing to want, so it must not be reported as an illegal transition.
    let home = seeded(JobStatus::Queued);
    let code = handle_jobs(&JobsSubcommand::Cancel {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("cancel must not return Err");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).status,
        JobStatus::Cancelled,
        "an active job must be cancellable regardless of which active state it is in"
    );
}

/// Cancelling a running job is the single most common thing a user does.
///
/// This previously asserted `TASK_FAILED`, pinning the bug where `handle_jobs`
/// called `mutate(..., Cancelled)` — a raw one-hop `transition` that only
/// succeeds from `cancelling`. The handler now calls `JobStore::cancel`, which
/// drives the same `running -> cancelling -> cancelled` sequence agenthicc's
/// `BackgroundSupervisor.cancel` uses.
#[tokio::test]
async fn cancel_of_a_running_job_succeeds_via_the_two_step_path() {
    let home = seeded(JobStatus::Running);
    let code = handle_jobs(&JobsSubcommand::Cancel {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("cancel must not return Err");
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "cancelling a running job must succeed, not report an illegal transition"
    );
    assert_eq!(
        reread(&home).status,
        JobStatus::Cancelled,
        "the handler must persist the two-step transition, not just print it"
    );
}

#[tokio::test]
async fn cancel_of_an_unknown_id_reports_failure() {
    let _home = seeded(JobStatus::Cancelling);
    let code = handle_jobs(&JobsSubcommand::Cancel {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("cancel must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

// ── resume ──────────────────────────────────────────────────────────────

/// `jobs resume` targets the same states agenthicc's
/// `BackgroundSupervisor.resume` admits — orphaned, failed, cancelled,
/// archived — and picks the target the way `BackgroundSupervisor.handoff`
/// does: `failed -> retrying`, everything else `-> starting`.
///
/// This previously asserted `TASK_FAILED` for every case, pinning the bug
/// where the handler transitioned to `Queued`. No arm of `can_transition_to`
/// lists `queued` as a target, so every resume failed. `queued` is not a
/// resume target: in agenthicc too, a session only enters `queued` when it is
/// created.
#[tokio::test]
async fn resume_admits_the_four_states_agenthicc_admits() {
    let home = seeded(JobStatus::Cancelled);
    let code = handle_jobs(&JobsSubcommand::Resume {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("resume must not return Err");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).status,
        JobStatus::Starting,
        "a cancelled job resumes into `starting`, matching handoff"
    );

    // The first guard is dropped first: the XAFT_HOME lock is not reentrant.
    drop(home);
    for (status, expected) in [
        (JobStatus::Failed, JobStatus::Retrying),
        (JobStatus::Orphaned, JobStatus::Starting),
        (JobStatus::Archived, JobStatus::Starting),
    ] {
        let scoped = seeded(status);
        let code = handle_jobs(&JobsSubcommand::Resume {
            session_id: "sess_0123456789abcdef".to_string(),
        })
        .await
        .expect("resume");
        assert_eq!(code, ExitCode::SUCCESS, "resume from {status}");
        assert_eq!(reread(&scoped).status, expected, "resume from {status}");
    }
}

/// A state agenthicc does not admit must still be refused, and left untouched.
///
/// This is the half of the contract that the previous implementation got right
/// by accident: targeting `Queued` rejected everything. Now that resume really
/// works, the rejection has to come from an explicit state check instead.
#[tokio::test]
async fn resume_of_an_active_or_terminal_job_is_refused_and_leaves_it_alone() {
    for status in [JobStatus::Queued, JobStatus::Running, JobStatus::Completed] {
        let home = seeded(status);
        let code = handle_jobs(&JobsSubcommand::Resume {
            session_id: "sess_0123456789abcdef".to_string(),
        })
        .await
        .expect("resume must not return Err");
        assert_eq!(code, ExitCode::TASK_FAILED, "resume from {status}");
        assert_eq!(
            reread(&home).status,
            status,
            "a refused resume must not change the job"
        );
    }
}

// ── retry / archive ─────────────────────────────────────────────────────

#[tokio::test]
async fn retry_moves_a_failed_job_to_retrying_and_persists_it() {
    let home = seeded(JobStatus::Failed);
    let code = handle_jobs(&JobsSubcommand::Retry {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("retry");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(reread(&home).status, JobStatus::Retrying);
}

#[tokio::test]
async fn retry_of_an_unknown_id_reports_failure() {
    let _home = seeded(JobStatus::Failed);
    let code = handle_jobs(&JobsSubcommand::Retry {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("retry must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

#[tokio::test]
async fn archive_moves_a_completed_job_to_archived_and_persists_it() {
    let home = seeded(JobStatus::Completed);
    let code = handle_jobs(&JobsSubcommand::Archive {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("archive");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(reread(&home).status, JobStatus::Archived);
}

#[tokio::test]
async fn archive_of_an_unknown_id_reports_failure() {
    let _home = seeded(JobStatus::Completed);
    let code = handle_jobs(&JobsSubcommand::Archive {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("archive must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

// ── approve / reject ────────────────────────────────────────────────────

#[tokio::test]
async fn approve_and_reject_a_waiting_job_and_reject_an_unknown_id() {
    let home = seeded(JobStatus::WaitingApproval);
    let code = handle_jobs(&JobsSubcommand::Approve {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("approve");
    assert_eq!(code, ExitCode::SUCCESS);
    let approved = reread(&home);
    assert_eq!(approved.status, JobStatus::Running);
    assert_eq!(approved.approval_decision, Some(true));

    // `seeded` takes the process-wide XAFT_HOME lock, and std::sync::Mutex is
    // not reentrant, so the first guard must be released before a second one
    // is taken -- otherwise this test deadlocks against itself.
    drop(home);
    let home = seeded(JobStatus::WaitingApproval);
    let code = handle_jobs(&JobsSubcommand::Reject {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("reject");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(reread(&home).approval_decision, Some(false));

    // Same rule for the third guard in this test.
    drop(home);
    let _home = seeded(JobStatus::WaitingApproval);
    let code = handle_jobs(&JobsSubcommand::Approve {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("approve must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

// ── input ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn input_records_the_value_a_waiting_job_asked_for() {
    let home = seeded(JobStatus::WaitingInput);
    let code = handle_jobs(&JobsSubcommand::Input {
        session_id: "sess_0123456789abcdef".to_string(),
        value: "yes, go ahead".to_string(),
    })
    .await
    .expect("input");
    assert_eq!(code, ExitCode::SUCCESS);

    let job = reread(&home);
    assert_eq!(job.input_value.as_deref(), Some("yes, go ahead"));
    assert_eq!(job.status, JobStatus::Running);
}

#[tokio::test]
async fn input_for_an_unknown_id_reports_failure() {
    let _home = seeded(JobStatus::WaitingInput);
    let code = handle_jobs(&JobsSubcommand::Input {
        session_id: UNKNOWN.to_string(),
        value: "x".to_string(),
    })
    .await
    .expect("input must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

// ── rename / labels ─────────────────────────────────────────────────────

#[tokio::test]
async fn rename_sets_the_title_and_rejects_an_unknown_id() {
    let home = seeded(JobStatus::Running);
    let code = handle_jobs(&JobsSubcommand::Rename {
        session_id: "sess_0123456789abcdef".to_string(),
        title: "  trimmed title  ".to_string(),
    })
    .await
    .expect("rename");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).title,
        "trimmed title",
        "the handler's wire line echoes the stored title, so trimming matters"
    );

    drop(home);
    let _home = seeded(JobStatus::Running);
    let code = handle_jobs(&JobsSubcommand::Rename {
        session_id: UNKNOWN.to_string(),
        title: "x".to_string(),
    })
    .await
    .expect("rename must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

#[tokio::test]
async fn labels_split_on_commas_drop_empties_and_clear_to_none() {
    let home = seeded(JobStatus::Running);
    let code = handle_jobs(&JobsSubcommand::Labels {
        session_id: "sess_0123456789abcdef".to_string(),
        labels: " alpha , beta ,, gamma ".to_string(),
    })
    .await
    .expect("labels");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).labels,
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "empty segments must be dropped, not stored as blanks"
    );

    // An all-empty label string clears the set; the handler prints "none" for
    // that case, so the cleared state has to round-trip.
    let code = handle_jobs(&JobsSubcommand::Labels {
        session_id: "sess_0123456789abcdef".to_string(),
        labels: " , , ".to_string(),
    })
    .await
    .expect("labels");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(reread(&home).labels.is_empty());
}

#[tokio::test]
async fn labels_on_an_unknown_id_report_failure() {
    let _home = seeded(JobStatus::Running);
    let code = handle_jobs(&JobsSubcommand::Labels {
        session_id: UNKNOWN.to_string(),
        labels: "a".to_string(),
    })
    .await
    .expect("labels must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

// ── delete / restore / purge ────────────────────────────────────────────

#[tokio::test]
async fn delete_then_restore_round_trips_through_the_store() {
    let home = seeded(JobStatus::Completed);
    let id = "sess_0123456789abcdef".to_string();

    let code = handle_jobs(&JobsSubcommand::Delete {
        session_id: id.clone(),
    })
    .await
    .expect("delete");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).status,
        JobStatus::Deleted,
        "a deleted job is frozen, not merely hidden"
    );

    let code = handle_jobs(&JobsSubcommand::Restore {
        session_id: id.clone(),
    })
    .await
    .expect("restore");
    assert_eq!(code, ExitCode::SUCCESS);
    // `restore` honours the pre-trash state recorded on the job itself
    // (`Job::pre_delete_status`). The state used to be kept in an in-process
    // `restore_targets` map and then filtered through `t.can_transition_to(*t)`,
    // which rejects every possible value (no arm of the table lists its own
    // state as a target) -- so the remembered state was silently discarded and
    // every restored job came back as `Queued`. The in-process map was also
    // unreachable in practice: `jobs delete` and `jobs restore` are separate
    // CLI invocations, so a fresh `JobStore` never had anything in it.
    assert_eq!(
        reread(&home).status,
        JobStatus::Completed,
        "restore must return the job to the state it held before deletion"
    );

    let code = handle_jobs(&JobsSubcommand::Delete {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("delete must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);

    let code = handle_jobs(&JobsSubcommand::Restore {
        session_id: UNKNOWN.to_string(),
    })
    .await
    .expect("restore must not return Err");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

/// A `jobs.json` written before `pre_delete_status` existed must still load,
/// and its trashed job must come back runnable rather than frozen.
///
/// `Job::pre_delete_status` is `#[serde(default)]`, so an older snapshot
/// deserialises with `None` and `restore` falls back to `queued` -- the state a
/// session is created in, and the same recovery agenthicc performs. Without the
/// default, one stale `jobs.json` would make every `xaft jobs` invocation fail
/// to open the store.
#[tokio::test]
async fn a_snapshot_predating_the_field_still_loads_and_restores_to_queued() {
    let home = HomeGuard::new();
    let path = home.store_dir().join("jobs.json");
    std::fs::create_dir_all(home.store_dir()).expect("store dir");
    std::fs::write(
        &path,
        r#"{
  "sess_0123456789abcdef": {
    "session_id": "sess_0123456789abcdef",
    "title": "nightly refactor",
    "workflow_name": "release_review",
    "input_value": null,
    "status": "deleted",
    "created_at": 1000.0,
    "state_changed_at": 1000.0,
    "labels": [],
    "deleted_at": 1000.0
  }
}"#,
    )
    .expect("write legacy snapshot");

    // The legacy file must load rather than erroring the whole command family.
    let code = handle_jobs(&list(true, true, OutputFormat::Pretty))
        .await
        .expect("list must not error");
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "a snapshot without the field must load"
    );

    let code = handle_jobs(&JobsSubcommand::Restore {
        session_id: "sess_0123456789abcdef".to_string(),
    })
    .await
    .expect("restore must not return Err");
    assert_eq!(code, ExitCode::SUCCESS);
    let job = reread(&home);
    assert_eq!(
        job.status,
        JobStatus::Queued,
        "with no recorded pre-trash state, restore falls back to queued"
    );
    assert_eq!(
        job.deleted_at, None,
        "restore must clear the trash timestamp"
    );
}

#[tokio::test]
async fn purge_succeeds_on_an_empty_store_and_keeps_fresh_trash() {
    // The retention window is 7 days, so a job trashed seconds ago must
    // survive; only genuinely expired trash may be removed.
    let home = seeded(JobStatus::Deleted);
    let code = handle_jobs(&JobsSubcommand::Purge).await.expect("purge");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        reread(&home).status,
        JobStatus::Deleted,
        "fresh trash is inside the 7-day retention window and must be kept"
    );

    drop(home);
    let _home = HomeGuard::new();
    let code = handle_jobs(&JobsSubcommand::Purge).await.expect("purge");
    assert_eq!(code, ExitCode::SUCCESS);
}

/// `Deleted` jobs are frozen for transitions but `restore` must still work,
/// which means the freeze cannot live in the generic `mut_get` path.
#[tokio::test]
async fn a_deleted_job_refuses_every_ordinary_transition() {
    let _home = seeded(JobStatus::Deleted);
    for status in [
        JobStatus::Starting,
        JobStatus::Running,
        JobStatus::Cancelled,
        JobStatus::Retrying,
    ] {
        let mut store = JobStore::open(_home.store_dir()).expect("open");
        assert!(
            store.transition("sess_0123456789abcdef", status).is_err(),
            "{status} must be refused while the job is deleted"
        );
    }
}
