//! Handler for `xaft agents` (parity matrix row 59).
//!
//! Port of agenthicc's `agents` command (`cli/commands/background.py:295`):
//!
//! | form | what it does |
//! | --- | --- |
//! | `xaft agents` | open the interactive background-sessions manager ([`crate::background_manager`]); Enter attaches the selection |
//! | `xaft agents --json` | print the current background sessions (the `jobs` store) as `indent=2, sort_keys=True` JSON |
//! | `xaft agents --run RUN --json` | print the agents linked to one goal run |
//! | `xaft agents --run RUN` | open the manager on that run; Enter attaches the run's main session |
//!
//! A run's agents are the projection agenthicc's `GoalRunManager.projection`
//! builds, over xaft's three stores: the run's main session, and every worker
//! session of every parallel orchestration linked to the run (by `run_id`, or
//! by the run's main session as the orchestration's parent). Each is joined
//! with its background job, when one exists, for status and activity, and
//! with its worktree record for branch, path and commits. The record has
//! agenthicc's `RunAgentRecord.to_dict()` keys.
//!
//! # Deliberate divergence
//!
//! agenthicc only lists agents that have a background session. A run's main
//! session and a dispatched worker are agents whether or not a background job
//! was ever recorded for them, so xaft lists them too, with the run's (or the
//! task's) status when there is no job to read.

use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::{Value, json};
use xaft_runtime::ExitCode;
use xaft_runtime::SessionBackend;

use crate::args::AgentsArgs;
use crate::background_manager::{ManagerAction, ManagerState, Row, run_manager};
use crate::commands::attach::{TuiForeground, attach_background_session, attach_goal_run};
use crate::error::XaftError;
use crate::jobs::{Job, JobStore};
use crate::pyjson;
use crate::runs::{GoalRun, RunStore};
use crate::worktrees::{ManifestStore, ParallelManifest};

/// Execute `xaft agents` against the default stores.
pub async fn handle_agents(
    args: &AgentsArgs,
    sessions: Option<Arc<dyn SessionBackend>>,
) -> Result<ExitCode, XaftError> {
    let io = |e: String| XaftError::Io(std::io::Error::other(e));
    let mut jobs = JobStore::open_default().map_err(|e| io(e.to_string()))?;
    let runs = RunStore::open_default().map_err(|e| io(e.to_string()))?;
    let manifests = ManifestStore::open_default().map_err(|e| io(e.to_string()))?;
    let foreground = TuiForeground { sessions };

    let mut out = String::new();
    if args.json {
        let code = render_agents_json(args, &jobs, &runs, &manifests, &mut out);
        print!("{out}");
        return Ok(code);
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!(
            "xaft: `agents` opens an interactive manager and no terminal is attached; \
             use `xaft agents --json`"
        );
        return Ok(ExitCode::TASK_FAILED);
    }

    let mut state = match args.run.as_deref().filter(|r| !r.is_empty()) {
        Some(run_id) => {
            let Ok(run) = runs.get(run_id) else {
                println!("Run not found: {run_id}");
                return Ok(ExitCode::TASK_FAILED);
            };
            run_manager_state(run, &jobs, &manifests.list())
        }
        None => ManagerState::for_jobs(&jobs),
    };
    // The manager blocks on the terminal by design: it is the only thing this
    // process is doing until the user closes it or picks a session.
    let action = run_manager(&mut state, &mut jobs)?;
    let code = match action {
        ManagerAction::Attach(session_id) => {
            attach_background_session(&mut jobs, &session_id, false, &foreground, &mut out).await
        }
        ManagerAction::AttachRun(run_id) => {
            attach_goal_run(&runs, &mut jobs, &run_id, false, &foreground, &mut out).await
        }
        ManagerAction::Quit | ManagerAction::Continue => ExitCode::SUCCESS,
    };
    print!("{out}");
    Ok(code)
}

/// The `--json` forms, written into `out`.
pub fn render_agents_json(
    args: &AgentsArgs,
    jobs: &JobStore,
    runs: &RunStore,
    manifests: &ManifestStore,
    out: &mut String,
) -> ExitCode {
    if let Some(run_id) = args.run.as_deref().filter(|r| !r.is_empty()) {
        let Ok(run) = runs.get(run_id) else {
            let _ = writeln!(out, "Run not found: {run_id}");
            return ExitCode::TASK_FAILED;
        };
        let agents = run_agents(run, jobs, &manifests.list());
        let _ = writeln!(out, "{}", pyjson::dumps_pretty(&Value::Array(agents)));
        return ExitCode::SUCCESS;
    }
    let sessions: Vec<Value> = jobs.list(false, false).iter().map(public_session).collect();
    let _ = writeln!(out, "{}", pyjson::dumps_pretty(&Value::Array(sessions)));
    ExitCode::SUCCESS
}

/// agenthicc's `_public_session`: the redacted job, plus the advisory queue
/// position — always `null` here, because xaft schedules no queue.
pub fn public_session(job: &Job) -> Value {
    let mut value = serde_json::to_value(job.public()).unwrap_or_else(|_| json!({}));
    if let Some(map) = value.as_object_mut() {
        map.insert("queue_position".into(), Value::Null);
        map.insert("queue_position_advisory".into(), Value::Null);
    }
    value
}

/// The agents linked to `run`, as agenthicc's `RunAgentRecord.to_dict()`s.
pub fn run_agents(run: &GoalRun, jobs: &JobStore, manifests: &[ParallelManifest]) -> Vec<Value> {
    let mut agents = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let main = run.main_session_id.as_str();
    if !main.is_empty() {
        let job = jobs.get(main, true).ok();
        let status = job
            .as_ref()
            .map(|j| j.status.as_str().to_string())
            .unwrap_or_else(|| run.status.as_str().to_string());
        agents.push(agent_record(AgentFields {
            run,
            role: "main",
            session_id: main,
            task_id: "",
            status,
            job: job.as_ref(),
            worktree: None,
            started_at: run.started_at,
            fallback_completed_at: run.completed_at,
            fallback_attention: run.failure_reason.clone(),
        }));
        seen.push(main.to_string());
    }
    for manifest in manifests {
        let linked = manifest.run_id == run.run_id
            || (!main.is_empty() && manifest.parent_session_id == main);
        if !linked {
            continue;
        }
        for task in &manifest.tasks {
            let session = task.worker_session_id.as_str();
            if session.is_empty() || seen.iter().any(|s| s == session) {
                continue;
            }
            seen.push(session.to_string());
            let job = jobs.get(session, true).ok();
            let status = job
                .as_ref()
                .map(|j| j.status.as_str().to_string())
                .unwrap_or_else(|| task.status.as_str().to_string());
            agents.push(agent_record(AgentFields {
                run,
                role: "worker",
                session_id: session,
                task_id: &task.task_id,
                status,
                job: job.as_ref(),
                worktree: manifest.worktree(&task.worktree_id),
                started_at: Some(task.created_at).filter(|t| *t > 0.0),
                fallback_completed_at: None,
                fallback_attention: task.error.clone(),
            }));
        }
    }
    agents
}

struct AgentFields<'a> {
    run: &'a GoalRun,
    role: &'a str,
    session_id: &'a str,
    task_id: &'a str,
    status: String,
    job: Option<&'a Job>,
    worktree: Option<&'a crate::worktrees::WorktreeRecord>,
    started_at: Option<f64>,
    fallback_completed_at: Option<f64>,
    fallback_attention: String,
}

fn agent_record(f: AgentFields<'_>) -> Value {
    let failed = matches!(f.status.as_str(), "failed" | "cancelled");
    let attention = f
        .job
        .and_then(|j| j.error.clone())
        .filter(|_| failed)
        .or_else(|| {
            f.worktree
                .map(|w| w.last_error.clone())
                .filter(|e| !e.is_empty())
        })
        .or_else(|| {
            f.worktree
                .filter(|w| w.dirty)
                .map(|_| "dirty worktree".to_string())
        })
        .unwrap_or_else(|| {
            if failed {
                f.fallback_attention.clone()
            } else {
                String::new()
            }
        });
    json!({
        "agent_id": f.session_id,
        "run_id": f.run.run_id,
        "role": f.role,
        "task_id": f.task_id,
        "session_id": f.session_id,
        "process_id": null,
        "pid": null,
        "workflow_run_id": "",
        "status": f.status,
        "worktree_id": f.worktree.map(|w| w.worktree_id.clone()).unwrap_or_default(),
        "worktree_path": f.worktree.map(|w| w.path.clone()).unwrap_or_default(),
        "branch": f.worktree.map(|w| w.branch.clone()).unwrap_or_default(),
        "base_commit": f.worktree.map(|w| w.base_commit.clone()).unwrap_or_default(),
        "head_commit": f.worktree.map(|w| w.head_commit.clone()).unwrap_or_default(),
        "started_at": crate::commands::attach::opt(f.started_at),
        "completed_at": crate::commands::attach::opt(
            f.job.and_then(|j| j.completed_at).or(f.fallback_completed_at)
        ),
        "last_heartbeat_at": null,
        "last_activity_at": crate::commands::attach::opt(f.job.map(|j| j.state_changed_at)),
        "current_phase": "",
        "current_operation": f.job.map(|j| j.latest_activity.clone()).unwrap_or_default(),
        "attention_reason": attention,
        "exit_code": null,
        "exit_reason": "",
        "attempt": 0,
        "heartbeat_stale": false,
        "requested_mode_name": null,
        "mode_name": "",
        "mode_application_status": "pending",
        "mode_application_error": "",
    })
}

/// The manager state for `agents --run RUN`.
pub fn run_manager_state(
    run: &GoalRun,
    jobs: &JobStore,
    manifests: &[ParallelManifest],
) -> ManagerState {
    let goal: String = run.goal.chars().take(60).collect();
    let header = format!("Goal run {} · {} · {goal}", run.run_id, run.status);
    let rows = run_agents(run, jobs, manifests)
        .into_iter()
        .map(|agent| Row {
            session_id: text(&agent, "session_id"),
            status: text(&agent, "status"),
            title: text(&agent, "role"),
            flow: match text(&agent, "task_id") {
                task if task.is_empty() => "-".into(),
                task => task,
            },
            activity: text(&agent, "current_operation"),
            cwd: text(&agent, "worktree_path"),
        })
        .collect();
    ManagerState::for_run(&run.run_id, header, rows)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
