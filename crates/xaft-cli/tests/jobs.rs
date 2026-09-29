//! Tests for the background-job model and store.
//!
//! These lock in the user-visible contract that `xaft jobs` shares with
//! agenthicc: the status vocabulary, the legal-transition table, the redaction
//! rules, and the current/archived/trash list filters.

use xaft_cli::jobs::*;

/// Build a store in a throwaway directory.
fn store() -> (tempfile::TempDir, JobStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JobStore::open(dir.path()).expect("open");
    (dir, store)
}

#[test]
fn status_wire_values_match_agenthicc() {
    // agenthicc's SessionStatus, verbatim.
    let expected = [
        "queued",
        "starting",
        "running",
        "waiting_approval",
        "waiting_input",
        "retrying",
        "cancelling",
        "completed",
        "failed",
        "cancelled",
        "orphaned",
        "archived",
        "deleted",
    ];
    let actual: Vec<_> = JobStatus::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(actual, expected);
}

#[test]
fn status_round_trips_through_its_wire_value() {
    for status in JobStatus::ALL {
        assert_eq!(JobStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(JobStatus::parse("nonsense"), None);
}

#[test]
fn active_and_terminal_partition_the_vocabulary() {
    // agenthicc's ACTIVE_STATUSES / TERMINAL_STATUSES.
    let active = [
        JobStatus::Queued,
        JobStatus::Starting,
        JobStatus::Running,
        JobStatus::WaitingApproval,
        JobStatus::WaitingInput,
        JobStatus::Retrying,
        JobStatus::Cancelling,
    ];
    for status in JobStatus::ALL {
        assert_eq!(
            status.is_active(),
            active.contains(&status),
            "{status} active classification"
        );
        assert_eq!(status.is_active(), !status.is_terminal(), "{status}");
    }
    assert_eq!(active.len() + JobStatus::ALL.len() - active.len(), 13);
}

#[test]
fn transition_table_matches_agenthicc() {
    use JobStatus::*;
    // A representative slice of the exact pairs agenthicc permits.
    let legal: &[(JobStatus, JobStatus)] = &[
        (Queued, Starting),
        (Queued, Cancelling),
        (Queued, Orphaned),
        (Queued, Failed),
        (Starting, Running),
        (Running, WaitingApproval),
        (Running, WaitingInput),
        (Running, Retrying),
        (Running, Cancelling),
        (Running, Completed),
        (Running, Failed),
        (Running, Orphaned),
        (WaitingApproval, Running),
        (WaitingApproval, Cancelling),
        (WaitingInput, Running),
        (WaitingInput, Cancelling),
        (Retrying, Starting),
        (Cancelling, Cancelled),
        (Cancelling, Orphaned),
        (Completed, Archived),
        (Failed, Retrying),
        (Failed, Starting),
        (Failed, Archived),
        (Cancelled, Starting),
        (Cancelled, Archived),
        (Orphaned, Starting),
        (Orphaned, Cancelling),
        (Orphaned, Archived),
        (Archived, Starting),
        (Archived, Completed),
        (Archived, Failed),
    ];
    for (from, to) in legal {
        assert!(from.can_transition_to(*to), "{from} -> {to} should be legal");
    }
    // And a representative slice of the pairs it forbids.
    let illegal = &[
        (Queued, Running),
        (Queued, Completed),
        (Running, Queued),
        (Completed, Running),
        (Completed, Failed),
        (Cancelled, Running),
        (Cancelled, Queued),
        (Archived, Archived),
        (Deleted, Queued),
        (Deleted, Deleted),
    ];
    for (from, to) in illegal {
        assert!(!from.can_transition_to(*to), "{from} -> {to} should be illegal");
    }
}

#[test]
fn deleted_is_frozen() {
    // Matches agenthicc: DELETED maps to an empty transition set.
    for target in JobStatus::ALL {
        assert!(!JobStatus::Deleted.can_transition_to(target));
    }
}

#[test]
fn create_and_get_round_trip() {
    let (_d, mut s) = store();
    let job = s.create(Job::new("s-1", "add rate limiting")).expect("create");
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(s.get("s-1", false).expect("get").session_id, "s-1");
}

#[test]
fn duplicate_ids_are_rejected() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    let err = s.create(Job::new("s-1", "b")).expect_err("duplicate must fail");
    assert!(matches!(err, JobError::Invalid(_)));
}

#[test]
fn unknown_id_reports_not_found() {
    let (_d, s) = store();
    let err = s.get("nope", false).expect_err("must fail");
    assert!(matches!(err, JobError::NotFound(id) if id == "nope"));
}

#[test]
fn list_filters_archived_and_trashed() {
    let (_d, mut s) = store();
    s.create(Job::new("live", "a")).expect("create");
    s.create(Job::new("arch", "b")).expect("create");
    // queued -> starting -> running -> completed -> archived
    s.transition("arch", JobStatus::Starting).expect("start");
    s.transition("arch", JobStatus::Running).expect("run");
    s.create(Job::new("gone", "c")).expect("create");
    s.transition("arch", JobStatus::Completed).expect("complete");
    s.transition("arch", JobStatus::Archived).expect("archive");
    s.delete("gone").expect("delete");

    // Default: neither archived nor trashed.
    let ids: Vec<_> = s.list(false, false).iter().map(|j| j.session_id.clone()).collect();
    assert_eq!(ids, vec!["live"]);

    // --all: adds archived, still hides trash.
    let ids: Vec<_> = s.list(true, false).iter().map(|j| j.session_id.clone()).collect();
    assert_eq!(ids, vec!["arch", "live"]);

    // --trash: adds trash, still hides archived.
    let ids: Vec<_> = s.list(false, true).iter().map(|j| j.session_id.clone()).collect();
    assert_eq!(ids, vec!["gone", "live"]);

    // Both.
    let ids: Vec<_> = s.list(true, true).iter().map(|j| j.session_id.clone()).collect();
    assert_eq!(ids, vec!["arch", "gone", "live"]);
}

#[test]
fn illegal_transition_is_rejected_with_context() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    // queued -> running is not in agenthicc's table.
    let err = s.transition("s-1", JobStatus::Running).expect_err("must fail");
    match err {
        JobError::IllegalTransition { current, target } => {
            assert_eq!(current, JobStatus::Queued);
            assert_eq!(target, JobStatus::Running);
        }
        other => panic!("expected IllegalTransition, got {other:?}"),
    }
    // State is unchanged after a rejected transition.
    assert_eq!(s.get("s-1", false).expect("get").status, JobStatus::Queued);
}

#[test]
fn transition_to_the_same_state_is_a_no_op() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    let job = s.transition("s-1", JobStatus::Queued).expect("self-transition");
    assert_eq!(job.status, JobStatus::Queued);
}

#[test]
fn terminal_states_stamp_completed_at() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    // cancelled is terminal, so reaching it must stamp completed_at.
    s.cancel("s-1").expect("cancel");
    assert!(s.get("s-1", false).expect("get").completed_at.is_some());
    // and so must completed
    s.create(Job::new("s-2", "a")).expect("create");
    s.transition("s-2", JobStatus::Starting).expect("start");
    s.transition("s-2", JobStatus::Running).expect("run");
    s.transition("s-2", JobStatus::Completed).expect("complete");
    assert!(s.get("s-2", false).expect("get").completed_at.is_some());
    // a non-terminal state must not be stamped
    assert!(s.get("s-1", false).expect("get").completed_at.is_some());
}

#[test]
fn cancel_then_resume_round_trips() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    // Cancel from a queued job drains through `cancelling` first.
    assert_eq!(s.cancel("s-1").expect("cancel").status, JobStatus::Cancelled);
    // cancelled -> queued is not legal, so resume goes via starting.
    assert!(s.transition("s-1", JobStatus::Queued).is_err());
    assert_eq!(
        s.transition("s-1", JobStatus::Starting).expect("start").status,
        JobStatus::Starting
    );
}

#[test]
fn approve_requires_waiting_approval() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    s.transition("s-1", JobStatus::Starting).expect("start");
    s.transition("s-1", JobStatus::Running).expect("run");
    s.transition("s-1", JobStatus::WaitingApproval).expect("wait");

    let job = s.approve("s-1", true).expect("approve");
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.approval_decision, Some(true));
    assert!(job.approval_request.is_empty());
}

#[test]
fn approve_rejects_when_not_waiting() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    let err = s.approve("s-1", true).expect_err("queued job cannot approve");
    assert!(matches!(err, JobError::IllegalTransition { .. }));
}

#[test]
fn provide_input_records_and_clears_the_request() {
    let (_d, mut s) = store();
    let mut job = Job::new("s-1", "a");
    job.status = JobStatus::WaitingInput;
    job.input_request = "which branch?".into();
    s.create(job).expect("create");

    let job = s.provide_input("s-1", "main").expect("input");
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.input_value.as_deref(), Some("main"));
    assert!(job.input_request.is_empty());
}

#[test]
fn public_projection_never_leaks_intent_or_input() {
    let (_d, mut s) = store();
    let mut job = Job::new("s-1", "SECRET-PROMPT");
    job.status = JobStatus::WaitingInput;
    job.input_value = Some("SECRET-ANSWER".into());
    s.create(job).expect("create");

    let json = serde_json::to_string(&s.get("s-1", true).expect("get").public()).expect("json");
    assert!(!json.contains("SECRET-PROMPT"), "intent leaked: {json}");
    assert!(!json.contains("SECRET-ANSWER"), "input leaked: {json}");
    // The fields the CLI does surface are present.
    assert!(json.contains("session_id"));
    assert!(json.contains("status"));
    assert!(json.contains("labels"));
}

#[test]
fn rename_is_bounded_and_trimmed() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    assert_eq!(s.rename("s-1", "  nightly  ").expect("rename").title, "nightly");
    let too_long = "x".repeat(201);
    assert!(s.rename("s-1", &too_long).is_err());
    assert!(s.rename("s-1", &"x".repeat(200)).is_ok());
}

#[test]
fn labels_are_split_bounded_and_clearable() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    let job = s
        .set_labels("s-1", vec!["ci".into(), "urgent".into()])
        .expect("labels");
    assert_eq!(job.labels, vec!["ci", "urgent"]);
    assert!(s.set_labels("s-1", Vec::new()).expect("clear").labels.is_empty());
    assert!(s.set_labels("s-1", vec!["x".repeat(65)]).is_err());
    assert!(s.set_labels("s-1", (0..17).map(|i| format!("l{i}")).collect()).is_err());
}

#[test]
fn delete_freezes_state_and_restore_undoes_it() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    s.transition("s-1", JobStatus::Starting).expect("start");
    s.transition("s-1", JobStatus::Running).expect("run");
    s.transition("s-1", JobStatus::Completed).expect("complete");

    assert_eq!(s.delete("s-1").expect("delete").status, JobStatus::Deleted);
    // Frozen: no transition out of deleted.
    assert!(matches!(
        s.transition("s-1", JobStatus::Starting),
        Err(JobError::Deleted)
    ));
    // Hidden from the default listing, visible with include_deleted.
    assert!(s.get("s-1", false).is_err());
    assert!(s.get("s-1", true).is_ok());

    // Restore returns it to a workable state.
    let restored = s.restore("s-1").expect("restore");
    assert_ne!(restored.status, JobStatus::Deleted);
    assert!(restored.deleted_at.is_none());
}

#[test]
fn archived_cannot_be_deleted_directly() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    s.transition("s-1", JobStatus::Starting).expect("start");
    s.transition("s-1", JobStatus::Running).expect("run");
    s.transition("s-1", JobStatus::Completed).expect("complete");
    s.transition("s-1", JobStatus::Archived).expect("archive");
    assert!(s.delete("s-1").is_err());
}

#[test]
fn purge_only_removes_expired_trash() {
    let (_d, mut s) = store();
    s.create(Job::new("old", "a")).expect("create");
    s.create(Job::new("new", "b")).expect("create");
    s.delete("old").expect("delete");
    s.delete("new").expect("delete");

    // Nothing is old enough yet.
    assert!(s.purge_expired_trash((7 * 24 * 3600) as f64).expect("purge").is_empty());
    assert_eq!(s.list(false, true).len(), 2);

    // Everything is old enough.
    let purged = s.purge_expired_trash(-1.0).expect("purge");
    assert_eq!(purged.len(), 2);
    assert!(s.list(false, true).is_empty());
}

#[test]
fn state_survives_reopening() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut s = JobStore::open(dir.path()).expect("open");
        s.create(Job::new("s-1", "a")).expect("create");
        s.rename("s-1", "kept").expect("rename");
        s.set_labels("s-1", vec!["x".into()]).expect("labels");
    }
    let s = JobStore::open(dir.path()).expect("reopen");
    let job = s.get("s-1", false).expect("get");
    assert_eq!(job.title, "kept");
    assert_eq!(job.labels, vec!["x"]);
    assert_eq!(job.intent, "a");
}

#[test]
fn an_empty_store_file_loads_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("jobs.json"), "   \n").expect("write");
    let s = JobStore::open(dir.path()).expect("open");
    assert!(s.list(true, true).is_empty());
}

#[test]
fn default_root_honours_xaft_home() {
    // The CLI resolves its store from XAFT_HOME, so tests can isolate it.
    // SAFETY: env mutation is process-global, so it is confined to this one
    // test's own thread and the previous value is restored before returning.
    let prev = std::env::var_os("XAFT_HOME");
    let dir = tempfile::tempdir().expect("tempdir");
    unsafe {
        std::env::set_var("XAFT_HOME", dir.path());
    }
    let root = JobStore::default_root().expect("root");
    assert_eq!(root, dir.path().join("jobs"));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("XAFT_HOME", v),
            None => std::env::remove_var("XAFT_HOME"),
        }
    }
}

#[test]
fn cancel_works_from_every_active_state() {
    use JobStatus::*;
    for (label, mut setup) in [
        ("queued", Vec::new()),
        ("starting", vec![Starting]),
        ("running", vec![Starting, Running]),
        ("waiting_input", vec![Starting, Running, WaitingInput]),
    ] {
        let (_d, mut s) = store();
        s.create(Job::new("s-1", "a")).expect("create");
        for step in setup.drain(..) {
            s.transition("s-1", step).expect("setup");
        }
        assert_eq!(
            s.cancel("s-1").expect("cancel").status,
            Cancelled,
            "cancel from {label}"
        );
    }
}

#[test]
fn cancel_refuses_to_touch_a_finished_job() {
    let (_d, mut s) = store();
    s.create(Job::new("s-1", "a")).expect("create");
    s.transition("s-1", JobStatus::Starting).expect("start");
    s.transition("s-1", JobStatus::Running).expect("run");
    s.transition("s-1", JobStatus::Completed).expect("complete");
    assert!(s.cancel("s-1").is_err(), "completed jobs cannot be cancelled");
}
