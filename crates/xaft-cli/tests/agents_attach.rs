//! Handler tests for `xaft agents` (row 59) and `xaft attach` (row 60).
//!
//! Both commands read xaft's three durable stores — background jobs, goal
//! runs and parallel orchestrations — so every test builds real stores in a
//! temp dir and drives the real handlers (`render_agents_json`,
//! `render_attach`). The one thing replaced is the TUI: `attach` takes a
//! `Foreground`, and the fake here records the handoff instead of opening a
//! terminal UI, which is exactly the boundary agenthicc's `--json` form
//! exposes to scripts.
//!
//! The interactive manager behind a bare `xaft agents` is a pure state machine
//! with its own unit tests (`background_manager.rs`); what is pinned here is
//! what it hands off to — the same `attach_background_session` as `attach`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::Value;
use xaft_cli::args::{AgentsArgs, AttachArgs, Commands, XaftCli};
use xaft_cli::commands::agents::{render_agents_json, run_manager_state};
use xaft_cli::commands::attach::{Foreground, render_attach};
use xaft_cli::jobs::{Job, JobStatus, JobStore};
use xaft_cli::runs::{GoalRun, RunStatus, RunStore};
use xaft_cli::worktrees::{
    ManifestStore, OrchestrationStatus, ParallelManifest, ParallelTask, TaskStatus, WorktreeRecord,
    WorktreeStatus,
};
use xaft_runtime::ExitCode;

/// Records what would have been opened in the TUI.
#[derive(Default)]
struct FakeForeground {
    opened: RefCell<Vec<(String, PathBuf)>>,
    fail_with: Option<String>,
}

impl Foreground for FakeForeground {
    async fn open(&self, session_id: &str, cwd: &Path) -> Result<(), String> {
        self.opened
            .borrow_mut()
            .push((session_id.to_string(), cwd.to_path_buf()));
        match &self.fail_with {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

struct Stores {
    _dir: tempfile::TempDir,
    jobs: JobStore,
    runs: RunStore,
    manifests: ManifestStore,
}

fn stores() -> Stores {
    let dir = tempfile::tempdir().unwrap();
    Stores {
        jobs: JobStore::open(dir.path().join("jobs")).unwrap(),
        runs: RunStore::open(dir.path().join("runs/runs.json")).unwrap(),
        manifests: ManifestStore::new(dir.path().join("orchestrations")),
        _dir: dir,
    }
}

fn job(id: &str, status: JobStatus) -> Job {
    let mut job = Job::new(id, "the raw intent must never be printed");
    job.status = status;
    job.cwd = format!("/work/{id}");
    job.title = format!("Title {id}");
    job.latest_activity = format!("Activity {id}");
    job
}

fn attach(session: Option<&str>, run: Option<&str>, json: bool) -> AttachArgs {
    AttachArgs {
        session_id: session.map(str::to_string),
        run: run.map(str::to_string),
        json,
    }
}

async fn run_attach(s: &mut Stores, args: AttachArgs, fg: &FakeForeground) -> (ExitCode, String) {
    let mut out = String::new();
    let code = render_attach(&args, &mut s.jobs, &s.runs, fg, &mut out).await;
    (code, out)
}

#[test]
fn both_commands_parse_with_agenthiccs_flags() {
    for argv in [
        vec!["xaft", "agents"],
        vec!["xaft", "agents", "--json"],
        vec!["xaft", "agents", "--run", "run_1", "--json"],
    ] {
        let cli = XaftCli::try_parse_from(&argv).unwrap();
        assert!(matches!(cli.command, Some(Commands::Agents(_))), "{argv:?}");
    }
    for argv in [
        vec!["xaft", "attach"],
        vec!["xaft", "attach", "s-1", "--json"],
        vec!["xaft", "attach", "--run", "run_1"],
    ] {
        let cli = XaftCli::try_parse_from(&argv).unwrap();
        assert!(matches!(cli.command, Some(Commands::Attach(_))), "{argv:?}");
    }
}

// ── agents ──────────────────────────────────────────────────────────────────

#[test]
fn agents_json_lists_current_sessions_redacted_with_queue_fields() {
    let mut s = stores();
    s.jobs.create(job("s-run", JobStatus::Running)).unwrap();
    s.jobs.create(job("s-done", JobStatus::Completed)).unwrap();
    s.jobs.create(job("s-arch", JobStatus::Completed)).unwrap();
    s.jobs.transition("s-arch", JobStatus::Archived).unwrap();
    s.jobs.create(job("s-trash", JobStatus::Failed)).unwrap();
    s.jobs.delete("s-trash").unwrap();

    let mut out = String::new();
    let code = render_agents_json(
        &AgentsArgs {
            run: None,
            json: true,
        },
        &s.jobs,
        &s.runs,
        &s.manifests,
        &mut out,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        out.starts_with("[\n  {\n    \"approval_decision\": null,"),
        "{out}"
    );
    assert!(!out.contains("the raw intent"), "intent must be redacted");
    let sessions: Vec<Value> = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["s-done", "s-run"],
        "archived and trashed sessions are not current"
    );
    assert_eq!(sessions[0]["queue_position"], Value::Null);
    assert_eq!(sessions[0]["queue_position_advisory"], Value::Null);
}

fn linked_run(s: &mut Stores) {
    let mut run = GoalRun::new("run_1", "ship the feature", "/repo", "/repo");
    run.status = RunStatus::Running;
    run.main_session_id = "s-main".into();
    s.runs.put(run).unwrap();
    s.jobs.create(job("s-main", JobStatus::Running)).unwrap();
    let now = 1_700_000_000.0;
    s.manifests
        .create(ParallelManifest {
            orchestration_id: "orch-1".into(),
            parent_session_id: "s-main".into(),
            run_id: String::new(),
            repository: "/repo".into(),
            parent_branch: "main".into(),
            base_commit: "abc1234".into(),
            status: OrchestrationStatus::Running,
            tasks: vec![ParallelTask {
                task_id: "t-1".into(),
                description: "write tests".into(),
                dependencies: vec![],
                status: TaskStatus::Running,
                worker_session_id: "w-1".into(),
                worktree_id: "wt-1".into(),
                result: None,
                error: String::new(),
                created_at: now,
                updated_at: now,
            }],
            worktrees: vec![WorktreeRecord {
                worktree_id: "wt-1".into(),
                repository: "/repo".into(),
                path: "/worktrees/wt-1".into(),
                branch: "xaft/s-main/t-1-wt-1".into(),
                base_commit: "abc1234".into(),
                owner_session_id: "s-main".into(),
                task_id: "t-1".into(),
                status: WorktreeStatus::Active,
                head_commit: "def5678".into(),
                dirty: true,
                conflict_paths: vec![],
                created_at: now,
                updated_at: now,
                last_error: String::new(),
                environment_receipts: vec![],
            }],
            created_at: now,
            updated_at: now,
            last_error: String::new(),
            verification: vec![],
        })
        .unwrap();
}

#[test]
fn agents_for_a_run_joins_the_main_session_and_its_workers() {
    let mut s = stores();
    linked_run(&mut s);
    let mut out = String::new();
    let code = render_agents_json(
        &AgentsArgs {
            run: Some("run_1".into()),
            json: true,
        },
        &s.jobs,
        &s.runs,
        &s.manifests,
        &mut out,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    let agents: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(agents.len(), 2);

    let main = &agents[0];
    assert_eq!(main["role"], "main");
    assert_eq!(main["session_id"], "s-main");
    assert_eq!(
        main["status"], "running",
        "status comes from the background job"
    );
    assert_eq!(main["current_operation"], "Activity s-main");

    let worker = &agents[1];
    assert_eq!(worker["role"], "worker");
    assert_eq!(worker["task_id"], "t-1");
    assert_eq!(worker["status"], "running", "no job: the task's status");
    assert_eq!(worker["worktree_path"], "/worktrees/wt-1");
    assert_eq!(worker["branch"], "xaft/s-main/t-1-wt-1");
    assert_eq!(worker["head_commit"], "def5678");
    assert_eq!(worker["attention_reason"], "dirty worktree");

    // agenthicc's RunAgentRecord.to_dict() key set, exactly.
    let mut keys: Vec<&str> = main
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys.len(), 29);
    for key in [
        "agent_id",
        "pid",
        "process_id",
        "heartbeat_stale",
        "mode_application_status",
    ] {
        assert!(keys.contains(&key), "{key} missing");
    }
}

#[test]
fn agents_for_an_unknown_run_exits_one() {
    let s = stores();
    let mut out = String::new();
    let code = render_agents_json(
        &AgentsArgs {
            run: Some("run_nope".into()),
            json: true,
        },
        &s.jobs,
        &s.runs,
        &s.manifests,
        &mut out,
    );
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(out, "Run not found: run_nope\n");
}

#[test]
fn the_run_manager_lists_the_runs_agents() {
    let mut s = stores();
    linked_run(&mut s);
    let run = s.runs.get("run_1").unwrap().clone();
    let state = run_manager_state(&run, &s.jobs, &s.manifests.list());
    let frame = state.render(120, 20);
    assert_eq!(frame[0], "Goal run run_1 · running · ship the feature");
    assert!(
        frame
            .iter()
            .any(|l| l.contains("s-main") && l.contains("main"))
    );
    assert!(frame.iter().any(|l| l.contains("w-1") && l.contains("t-1")));
}

// ── attach ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn attach_without_an_id_is_a_usage_error() {
    let mut s = stores();
    let fg = FakeForeground::default();
    let message = "attach requires an exact SESSION_ID (or explicit --run RUN_ID)";
    assert_eq!(
        run_attach(&mut s, attach(None, None, false), &fg).await,
        (ExitCode::USAGE_ERROR, format!("{message}\n"))
    );
    assert_eq!(
        run_attach(&mut s, attach(None, None, true), &fg).await,
        (
            ExitCode::USAGE_ERROR,
            format!("{{\"status\": \"error\", \"error\": \"{message}\"}}\n")
        )
    );
}

#[tokio::test]
async fn a_goal_run_id_is_refused_with_a_pointer_to_the_right_form() {
    let mut s = stores();
    let fg = FakeForeground::default();
    let (code, out) = run_attach(&mut s, attach(Some("run_42"), None, false), &fg).await;
    assert_eq!(code, ExitCode::USAGE_ERROR);
    assert_eq!(
        out,
        "run_42 is a goal-run ID, not a session ID; use xaft agents --run run_42 or \
         xaft attach SESSION_ID --run run_42\n"
    );
}

#[tokio::test]
async fn an_unknown_or_deleted_session_cannot_be_attached() {
    let mut s = stores();
    let fg = FakeForeground::default();
    let (code, out) = run_attach(&mut s, attach(Some("ghost"), None, false), &fg).await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to attach background session ghost: no background job with id ghost\n"
    );
    let (_, json) = run_attach(&mut s, attach(Some("ghost"), None, true), &fg).await;
    assert_eq!(
        json,
        "{\"session_id\": \"ghost\", \"status\": \"error\", \"error\": \"no background job with id ghost\"}\n"
    );

    s.jobs.create(job("gone", JobStatus::Failed)).unwrap();
    s.jobs.delete("gone").unwrap();
    let (code, out) = run_attach(&mut s, attach(Some("gone"), None, false), &fg).await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to attach background session gone: deleted background sessions cannot be attached\n"
    );
    assert!(fg.opened.borrow().is_empty());
}

#[tokio::test]
async fn attaching_an_active_session_takes_ownership_from_the_background() {
    let mut s = stores();
    s.jobs.create(job("s1", JobStatus::Running)).unwrap();
    let fg = FakeForeground::default();
    let (code, out) = run_attach(&mut s, attach(Some("s1"), None, true), &fg).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "{\"background_status\": \"cancelled\", \"cwd\": \"/work/s1\", \"session_id\": \"s1\", \"status\": \"handoff_ready\"}\n"
    );
    let record = s.jobs.get("s1", false).unwrap();
    assert_eq!(record.status, JobStatus::Cancelled);
    assert_eq!(record.cancellation_reason, "foreground handoff requested");
    assert_eq!(
        record.latest_activity,
        "Background worker stopped; ready for foreground"
    );
    assert!(fg.opened.borrow().is_empty(), "--json never opens the TUI");
}

#[tokio::test]
async fn attaching_opens_the_tui_in_the_sessions_directory() {
    let mut s = stores();
    s.jobs.create(job("s1", JobStatus::Completed)).unwrap();
    let fg = FakeForeground::default();
    let (code, out) = run_attach(&mut s, attach(Some("s1"), None, false), &fg).await;
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    assert_eq!(
        fg.opened.borrow().as_slice(),
        &[("s1".to_string(), PathBuf::from("/work/s1"))]
    );
    assert_eq!(
        s.jobs.get("s1", false).unwrap().status,
        JobStatus::Completed,
        "a finished session is handed over unchanged"
    );

    let failing = FakeForeground {
        fail_with: Some("session s1 is not in the session store".into()),
        ..Default::default()
    };
    let (code, out) = run_attach(&mut s, attach(Some("s1"), None, false), &failing).await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to open foreground session s1: session s1 is not in the session store\n"
    );
}

#[tokio::test]
async fn attach_run_hands_over_the_runs_main_session() {
    let mut s = stores();
    let mut run = GoalRun::new("run_1", "goal", "/repo", "/repo/root");
    run.main_session_id = "s-main".into();
    s.runs.put(run).unwrap();
    s.runs
        .put(GoalRun::new("run_2", "goal", "/repo", "/repo"))
        .unwrap();
    let fg = FakeForeground::default();

    let (code, out) = run_attach(&mut s, attach(None, Some("run_1"), true), &fg).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "{\"run_id\": \"run_1\", \"session_id\": \"s-main\", \"status\": \"handoff_ready\"}\n"
    );

    let (code, _) = run_attach(&mut s, attach(None, Some("run_1"), false), &fg).await;
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        fg.opened.borrow().as_slice(),
        &[("s-main".to_string(), PathBuf::from("/repo/root"))],
        "no background job: the run's repository root"
    );

    let (code, out) = run_attach(&mut s, attach(None, Some("run_2"), false), &fg).await;
    assert_eq!(code, ExitCode::TASK_FAILED);
    assert_eq!(
        out,
        "Unable to attach goal run run_2: goal run has no main session\n"
    );
    let (_, json) = run_attach(&mut s, attach(None, Some("run_9"), true), &fg).await;
    assert_eq!(
        json,
        "{\"run_id\": \"run_9\", \"status\": \"error\", \"error\": \"Run not found: run_9\"}\n"
    );
}
