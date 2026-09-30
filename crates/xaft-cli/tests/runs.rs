//! Integration tests for `xaft runs`.
//!
//! These exercise the store and the handlers directly against a temporary
//! `$XAFT_HOME`, so they assert the *observable* contract a user or script
//! depends on: the wire strings, the list column layout, the confirmation
//! lines, and the exit codes.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use xaft_cli::args::{OutputFormat, RunsListArgs, RunsSubcommand};
use xaft_cli::runs::{GoalRun, RunStatus, RunStore};
use xaft_runtime::ExitCode;

/// A store rooted in a throwaway directory.
///
/// These tests deliberately do NOT set `XAFT_HOME` for the store-level cases.
/// That variable is process-global and Rust runs a test binary on many
/// threads, so mutating it would race against every other test here. Only the
/// handler tests -- the ones that must resolve the *default* store path -- take
/// the `XAFT_HOME` lock first.
fn store() -> (RunStore, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("runs.json");
    (RunStore::open(&path).expect("open store"), path)
}

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
/// The guard OWNS the `XAFT_HOME` lock for its whole lifetime, not just
/// during construction. That matters: two handler tests running concurrently
/// would otherwise both set `XAFT_HOME` and the second would resolve the
/// first's store, making `runs cancel` operate on the wrong file.
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
        // released after `Drop` restores the previous value, so no other test
        // in this binary can read or write the environment concurrently.
        unsafe {
            std::env::set_var("XAFT_HOME", dir.path());
        }
        HomeGuard {
            previous,
            _dir: dir,
            _lock: lock,
        }
    }

    /// The path the default store resolves to under this `XAFT_HOME`.
    fn runs_file(&self) -> PathBuf {
        self._dir.path().join("runs").join("runs.json")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: see the comment in `HomeGuard::new`; the lock is still held
        // by the guard that created this scope.
        unsafe {
            match self.previous.take() {
                Some(v) => std::env::set_var("XAFT_HOME", v),
                None => std::env::remove_var("XAFT_HOME"),
            }
        }
    }
}

fn seed(store: &mut RunStore, id: &str, status: RunStatus, goal: &str) {
    let mut run = GoalRun::new(id, goal, "demo-repo", "/tmp/demo-repo");
    run.status = status;
    store.put(run).expect("seed run");
}

// ── status vocabulary ─────────────────────────────────────────────────────────

#[test]
fn all_twelve_statuses_have_agenthiccs_exact_wire_strings() {
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
    assert_eq!(actual, expected, "status vocabulary drifted from agenthicc");
    assert_eq!(RunStatus::ALL.len(), 12);
}

#[test]
fn every_status_round_trips() {
    for s in RunStatus::ALL {
        assert_eq!(RunStatus::parse(s.as_str()), Some(s));
    }
}

// ── store behaviour ───────────────────────────────────────────────────────────

#[test]
fn a_fresh_store_lists_nothing() {
    let (store, _p) = store();
    assert!(store.list().is_empty());
}

#[test]
fn runs_persist_across_reopen() {
    let (mut store, path) = store();
    seed(&mut store, "run_a", RunStatus::Running, "keep going");
    drop(store);
    let reopened = RunStore::open(&path).expect("reopen");
    assert_eq!(reopened.get("run_a").unwrap().status, RunStatus::Running);
}

#[test]
fn unknown_run_id_reports_not_found() {
    let (store, _p) = store();
    assert!(store.get("run_missing").is_err());
}

#[test]
fn status_filter_selects_only_matching_runs() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Running, "a");
    seed(&mut store, "run_b", RunStatus::Completed, "b");
    let running = store.list_by_status(Some("running")).unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].run_id, "run_a");
}

#[test]
fn unknown_status_filter_is_an_error_not_an_empty_list() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Running, "a");
    // agenthicc raises ValueError and exits 2; silently listing all runs would
    // hide a typo in a user's script.
    assert!(store.list_by_status(Some("runnig")).is_err());
}

// ── cancel ────────────────────────────────────────────────────────────────────

#[test]
fn cancel_moves_a_run_to_cancelled_and_stamps_completion() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Running, "a");
    let run = store.cancel("run_a").unwrap();
    assert_eq!(run.status, RunStatus::Cancelled);
    assert!(run.completed_at.is_some(), "cancel must stamp completed_at");
}

#[test]
fn cancel_is_idempotent_like_agenthicc() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Running, "a");
    store.cancel("run_a").unwrap();
    // agenthicc does not refuse an already-terminal run.
    assert_eq!(store.cancel("run_a").unwrap().status, RunStatus::Cancelled);
}

#[test]
fn cancel_of_unknown_run_is_not_found() {
    let (mut store, _p) = store();
    assert!(store.cancel("run_nope").is_err());
}

// ── resume ────────────────────────────────────────────────────────────────────

#[test]
fn resume_without_a_main_session_fails() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Created, "never started");
    // agenthicc: `if not run.main_session_id: raise ValueError`.
    assert!(
        store.resume("run_a").is_err(),
        "resuming a run that never started must fail"
    );
}

#[test]
fn resume_restarts_a_cancelled_run_and_keeps_its_session() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Cancelled, "a");
    store
        .update("run_a", |r| r.main_session_id = "sess_123".into())
        .unwrap();
    let run = store.resume("run_a").unwrap();
    assert_eq!(run.status, RunStatus::Starting);
    assert!(run.completed_at.is_none(), "resume must clear completed_at");
    assert_eq!(run.main_session_id, "sess_123");
}

#[test]
fn lost_runs_are_recoverable_because_lost_is_not_terminal() {
    let (mut store, _p) = store();
    seed(&mut store, "run_a", RunStatus::Lost, "worker died");
    assert!(!RunStatus::Lost.is_terminal());
    store
        .update("run_a", |r| r.main_session_id = "sess_9".into())
        .unwrap();
    assert_eq!(store.resume("run_a").unwrap().status, RunStatus::Starting);
}

// ── projection ────────────────────────────────────────────────────────────────

#[test]
fn json_projection_exposes_status_as_its_wire_string() {
    let (mut store, _p) = store();
    seed(
        &mut store,
        "run_a",
        RunStatus::NeedsAttention,
        "needs a human",
    );
    let v = store.get("run_a").unwrap().to_projection();
    assert_eq!(v["status"], "needs_attention");
    assert_eq!(v["run_id"], "run_a");
    assert_eq!(
        v["workflow_name"], "goal_flow",
        "agenthicc's default workflow"
    );
}

// ── CLI surface ───────────────────────────────────────────────────────────────

#[test]
fn the_runs_subcommand_parses_every_documented_form() {
    use clap::Parser;
    let cli = xaft_cli::args::XaftCli::try_parse_from([
        "xaft", "runs", "list", "--status", "running", "--format", "json",
    ])
    .expect("runs list must parse");
    match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => match cmd.resolve() {
            RunsSubcommand::List(args) => {
                assert_eq!(args.status, "running");
                assert!(matches!(args.format, OutputFormat::Json));
            }
            other => panic!("expected List, got {other:?}"),
        },
        other => panic!("expected Runs, got {other:?}"),
    }
}

#[test]
fn runs_show_accepts_a_run_id_and_json() {
    use clap::Parser;
    let cli = xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs", "show", "run_a", "--json"])
        .expect("runs show must parse");
    match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => match cmd.resolve() {
            RunsSubcommand::Show { run_id, json } => {
                assert_eq!(run_id, "run_a");
                assert!(json);
            }
            other => panic!("expected Show, got {other:?}"),
        },
        other => panic!("expected Runs, got {other:?}"),
    }
}

#[test]
fn runs_cancel_and_resume_take_a_bare_run_id() {
    use clap::Parser;
    for (argv, expect) in [
        (vec!["xaft", "runs", "cancel", "run_a"], "cancel"),
        (vec!["xaft", "runs", "resume", "run_a"], "resume"),
    ] {
        let cli = xaft_cli::args::XaftCli::try_parse_from(&argv).expect("must parse");
        match cli.command {
            Some(xaft_cli::args::Commands::Runs(cmd)) => match cmd.resolve() {
                RunsSubcommand::Cancel { run_id } if expect == "cancel" => {
                    assert_eq!(run_id, "run_a")
                }
                RunsSubcommand::Resume { run_id } if expect == "resume" => {
                    assert_eq!(run_id, "run_a")
                }
                other => panic!("unexpected subcommand for {expect}: {other:?}"),
            },
            other => panic!("expected Runs, got {other:?}"),
        }
    }
}

#[test]
fn runs_list_defaults_to_no_status_filter_and_pretty_output() {
    use clap::Parser;
    let cli =
        xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs", "list"]).expect("must parse");
    match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => match cmd.resolve() {
            RunsSubcommand::List(args) => {
                assert_eq!(args.status, "", "default status filter must be empty");
                assert!(matches!(args.format, OutputFormat::Pretty));
            }
            other => panic!("expected List, got {other:?}"),
        },
        other => panic!("expected Runs, got {other:?}"),
    }
}

#[test]
fn bare_runs_parses_and_resolves_to_list() {
    // Regression guard. Before the alias existed, clap rejected this argv with
    // "error: 'xaft runs' requires a subcommand but one was not provided" and
    // the binary exited 2, while agenthicc printed the listing. Asserting on
    // the parse alone would still have passed against the old code had it been
    // written as "resolves to List", because the old code had no None at all —
    // so both halves are checked: the parse succeeds AND nothing was typed.
    use clap::Parser;
    let cli =
        xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs"]).expect("bare runs must parse");
    match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => {
            assert!(
                cmd.subcommand.is_none(),
                "bare `xaft runs` must leave the subcommand unset so resolve() can alias it",
            );
            match cmd.resolve() {
                RunsSubcommand::List(args) => {
                    assert_eq!(args.status, "", "the alias must not invent a status filter");
                    assert!(matches!(args.format, OutputFormat::Pretty));
                }
                other => panic!("bare `xaft runs` must resolve to List, got {other:?}"),
            }
        }
        other => panic!("expected Runs, got {other:?}"),
    }
}

#[test]
fn the_explicit_list_form_still_resolves_to_list_with_its_own_flags() {
    // The alias must not change `xaft runs list`: its flags are still honoured,
    // which is what distinguishes "alias of list" from "list with flags removed".
    use clap::Parser;
    let cli =
        xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs", "list", "--status", "running"])
            .expect("runs list must parse");
    match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => {
            assert!(
                cmd.subcommand.is_some(),
                "`runs list` names its subcommand explicitly"
            );
            match cmd.resolve() {
                RunsSubcommand::List(args) => assert_eq!(args.status, "running"),
                other => panic!("expected List, got {other:?}"),
            }
        }
        other => panic!("expected Runs, got {other:?}"),
    }
}

#[test]
fn an_unknown_runs_subcommand_is_still_rejected() {
    // Making the subcommand optional must not turn every typo into `list`.
    use clap::Parser;
    let err = xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs", "lst"])
        .expect_err("`runs lst` must not silently list");
    assert_eq!(err.exit_code(), 2, "clap usage errors keep their exit code");
}

#[tokio::test]
async fn the_bare_form_dispatches_to_the_list_handler_and_succeeds() {
    // End-to-end through the real dispatch shape: parse argv, resolve, handle.
    // Against the pre-fix binary this never got past clap (exit 2); the exit
    // code is the observable a script depends on.
    use clap::Parser;
    let _home = HomeGuard::new();
    let cli =
        xaft_cli::args::XaftCli::try_parse_from(["xaft", "runs"]).expect("bare runs must parse");
    let cmd = match cli.command {
        Some(xaft_cli::args::Commands::Runs(cmd)) => cmd,
        other => panic!("expected Runs, got {other:?}"),
    };
    let code = xaft_cli::commands::runs::handle_runs(&cmd.resolve())
        .await
        .expect("handler runs");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        xaft_cli::commands::runs::NO_RUNS,
        "No goal runs.",
        "the empty-store line is the literal agenthicc prints, and the bare form prints it",
    );
}

#[tokio::test]
async fn handler_reports_success_for_an_empty_store() {
    let _home = HomeGuard::new();
    let args = RunsSubcommand::List(RunsListArgs {
        status: String::new(),
        format: OutputFormat::Pretty,
    });
    let code = xaft_cli::commands::runs::handle_runs(&args)
        .await
        .expect("handler runs");
    assert_eq!(code, ExitCode::SUCCESS);
}

#[tokio::test]
async fn handler_exits_two_on_an_unknown_status() {
    let _home = HomeGuard::new();
    let args = RunsSubcommand::List(RunsListArgs {
        status: "nonsense".into(),
        format: OutputFormat::Pretty,
    });
    let code = xaft_cli::commands::runs::handle_runs(&args)
        .await
        .expect("handler runs");
    assert_eq!(
        code,
        ExitCode::USAGE_ERROR,
        "unknown --status must exit 2 like agenthicc"
    );
}

#[tokio::test]
async fn handler_exits_one_when_showing_an_unknown_run() {
    let _home = HomeGuard::new();
    let args = RunsSubcommand::Show {
        run_id: "run_missing".into(),
        json: false,
    };
    let code = xaft_cli::commands::runs::handle_runs(&args)
        .await
        .expect("handler runs");
    assert_eq!(code, ExitCode::TASK_FAILED);
}

#[tokio::test]
async fn handler_cancels_through_the_default_store_path() {
    let home = HomeGuard::new();
    let path = home.runs_file();
    {
        let mut store = RunStore::open(&path).expect("open");
        seed(&mut store, "run_a", RunStatus::Running, "a");
    }
    let args = RunsSubcommand::Cancel {
        run_id: "run_a".into(),
    };
    let code = xaft_cli::commands::runs::handle_runs(&args)
        .await
        .expect("handler runs");
    assert_eq!(code, ExitCode::SUCCESS);
    let after = RunStore::open(&path).expect("reopen");
    assert_eq!(after.get("run_a").unwrap().status, RunStatus::Cancelled);
}

#[tokio::test]
async fn handler_resume_fails_cleanly_when_no_main_session() {
    let home = HomeGuard::new();
    let path = home.runs_file();
    {
        let mut store = RunStore::open(&path).expect("open");
        seed(&mut store, "run_a", RunStatus::Created, "a");
    }
    let args = RunsSubcommand::Resume {
        run_id: "run_a".into(),
    };
    let code = xaft_cli::commands::runs::handle_runs(&args)
        .await
        .expect("handler runs");
    assert_eq!(code, ExitCode::TASK_FAILED);
}
