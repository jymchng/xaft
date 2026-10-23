//! Handler for `xaft attach` (parity matrix row 60).
//!
//! Port of agenthicc's `attach` (`cli/commands/runs.py:91`) and the shared
//! `attach_background_session` (`cli/commands/background.py`): hand one exact
//! background session to the foreground TUI.
//!
//! The handoff has two halves, kept apart exactly as agenthicc keeps them:
//!
//! 1. **The store half** ([`JobStore::attach_foreground`]): a deleted session
//!    is refused; an active one is moved through `cancelling` to `cancelled`
//!    with the reason `foreground handoff requested`, so the background record
//!    and the foreground TUI can never both own the session; a finished one is
//!    returned as it is.
//! 2. **The TUI half**: the TUI is opened resuming that session, in the
//!    session's own working directory.
//!
//! `--json` stops after the store half and prints the handoff record
//! (`{"background_status", "cwd", "session_id", "status": "handoff_ready"}`),
//! which is how a script or an editor integration takes over a session.
//! `--run RUN` attaches a goal run's main session instead.
//!
//! Exit codes are agenthicc's: 2 for a missing or goal-run-shaped id, 1 for a
//! session that cannot be attached or a TUI that cannot be opened.
//!
//! # Deliberate divergence
//!
//! agenthicc's `attach --run` requires a background session for the run's
//! main session and fails with a `KeyError` otherwise. A run whose main
//! session never ran in the background has no worker to stop, so xaft hands
//! it over directly, in the run's repository root.

use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use xaft_runtime::ExitCode;
use xaft_runtime::SessionBackend;

use crate::args::AttachArgs;
use crate::error::XaftError;
use crate::jobs::{JobError, JobStore};
use crate::pyjson;
use crate::runs::RunStore;

/// Opens the foreground TUI on a session.
///
/// A trait so the whole command can be tested without a terminal.
pub trait Foreground {
    /// Open the TUI resuming `session_id` in `cwd`; `Err` explains why not.
    fn open(&self, session_id: &str, cwd: &Path) -> impl Future<Output = Result<(), String>>;
}

/// The real [`Foreground`]: xaft's TUI, after checking the session exists.
pub struct TuiForeground {
    /// The durable session store, when SQLite is available.
    pub sessions: Option<Arc<dyn SessionBackend>>,
}

impl Foreground for TuiForeground {
    async fn open(&self, session_id: &str, cwd: &Path) -> Result<(), String> {
        if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            return Err(format!(
                "no interactive terminal; use `xaft attach {session_id} --json` to hand the session off"
            ));
        }
        if let Some(backend) = &self.sessions {
            let id = xaft_runtime::session::SessionId::from_string(session_id);
            match backend.load_with_history(&id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(format!("session {session_id} is not in the session store"));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        open_tui(session_id, cwd).await
    }
}

#[cfg(feature = "tui")]
async fn open_tui(session_id: &str, cwd: &Path) -> Result<(), String> {
    let config = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides {
        project_dir: Some(cwd.to_path_buf()),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let request = xaft_runtime::dispatch::RunRequest {
        task: String::new(),
        config: config.clone(),
        working_dir: cwd.to_path_buf(),
        headless: false,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id: Some(session_id.to_string()),
        workflow: xaft_runtime::WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
        mode_system_patch: None,
        mode_tool_filter: None,
    };
    xaft_tui::TuiApp::new(config)
        .run(request)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "tui"))]
async fn open_tui(_session_id: &str, _cwd: &Path) -> Result<(), String> {
    Err("this build of xaft has no TUI".into())
}

/// Execute `xaft attach` against the default stores.
pub async fn handle_attach(
    args: &AttachArgs,
    sessions: Option<Arc<dyn SessionBackend>>,
) -> Result<ExitCode, XaftError> {
    let mut jobs = JobStore::open_default().map_err(job_io)?;
    let runs = RunStore::open_default()
        .map_err(|e| XaftError::Io(std::io::Error::other(e.to_string())))?;
    let mut out = String::new();
    let code = render_attach(
        args,
        &mut jobs,
        &runs,
        &TuiForeground { sessions },
        &mut out,
    )
    .await;
    print!("{out}");
    Ok(code)
}

fn job_io(e: JobError) -> XaftError {
    XaftError::Io(std::io::Error::other(e.to_string()))
}

/// Run `xaft attach`, writing its output into `out`.
pub async fn render_attach<F: Foreground>(
    args: &AttachArgs,
    jobs: &mut JobStore,
    runs: &RunStore,
    foreground: &F,
    out: &mut String,
) -> ExitCode {
    if let Some(run_id) = args.run.as_deref().filter(|r| !r.is_empty()) {
        return attach_goal_run(runs, jobs, run_id, args.json, foreground, out).await;
    }
    let Some(session_id) = args.session_id.as_deref().filter(|s| !s.is_empty()) else {
        return usage(
            "attach requires an exact SESSION_ID (or explicit --run RUN_ID)",
            args.json,
            out,
        );
    };
    if session_id.starts_with("run_") {
        return usage(
            &format!(
                "{session_id} is a goal-run ID, not a session ID; use xaft agents --run \
                 {session_id} or xaft attach SESSION_ID --run {session_id}"
            ),
            args.json,
            out,
        );
    }
    attach_background_session(jobs, session_id, args.json, foreground, out).await
}

fn usage(message: &str, json: bool, out: &mut String) -> ExitCode {
    if json {
        let line = pyjson::dumps_ordered(&[("status", json!("error")), ("error", json!(message))]);
        let _ = writeln!(out, "{line}");
    } else {
        let _ = writeln!(out, "{message}");
    }
    ExitCode::USAGE_ERROR
}

/// Hand one background session to the foreground.
///
/// Shared by `xaft attach` and the manager behind `xaft agents`, as agenthicc
/// shares `attach_background_session` between them, so the two paths cannot
/// disagree about what a handoff does.
pub async fn attach_background_session<F: Foreground>(
    jobs: &mut JobStore,
    session_id: &str,
    json: bool,
    foreground: &F,
    out: &mut String,
) -> ExitCode {
    let job = match jobs.attach_foreground(session_id) {
        Ok(job) => job,
        Err(e) => {
            let error = e.to_string();
            if json {
                let line = pyjson::dumps_ordered(&[
                    ("session_id", json!(session_id)),
                    ("status", json!("error")),
                    ("error", json!(error)),
                ]);
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(
                    out,
                    "Unable to attach background session {session_id}: {error}"
                );
            }
            return ExitCode::TASK_FAILED;
        }
    };
    if json {
        let line = pyjson::dumps(&json!({
            "session_id": session_id,
            "status": "handoff_ready",
            "background_status": job.status.as_str(),
            "cwd": job.cwd,
        }));
        let _ = writeln!(out, "{line}");
        return ExitCode::SUCCESS;
    }
    let cwd = session_cwd(&job.cwd);
    match foreground.open(session_id, &cwd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                out,
                "Unable to open foreground session {session_id}: {error}"
            );
            ExitCode::TASK_FAILED
        }
    }
}

fn session_cwd(recorded: &str) -> PathBuf {
    if recorded.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(recorded)
    }
}

/// Hand a goal run's main session to the foreground (`--run`).
pub async fn attach_goal_run<F: Foreground>(
    runs: &RunStore,
    jobs: &mut JobStore,
    run_id: &str,
    json: bool,
    foreground: &F,
    out: &mut String,
) -> ExitCode {
    let resolved = (|| -> Result<(String, PathBuf), String> {
        let run = runs.get(run_id).map_err(|e| e.to_string())?;
        if run.main_session_id.is_empty() {
            return Err("goal run has no main session".into());
        }
        let session = run.main_session_id.clone();
        let cwd = match jobs.attach_foreground(&session) {
            Ok(job) if !job.cwd.is_empty() => PathBuf::from(job.cwd),
            Ok(_) | Err(JobError::NotFound(_)) => PathBuf::from(&run.repository_root),
            Err(e) => return Err(e.to_string()),
        };
        Ok((session, cwd))
    })();
    let (session_id, cwd) = match resolved {
        Ok(found) => found,
        Err(error) => {
            if json {
                let line = pyjson::dumps_ordered(&[
                    ("run_id", json!(run_id)),
                    ("status", json!("error")),
                    ("error", json!(error)),
                ]);
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "Unable to attach goal run {run_id}: {error}");
            }
            return ExitCode::TASK_FAILED;
        }
    };
    if json {
        let line = pyjson::dumps(&json!({
            "run_id": run_id,
            "session_id": session_id,
            "status": "handoff_ready",
        }));
        let _ = writeln!(out, "{line}");
        return ExitCode::SUCCESS;
    }
    match foreground.open(&session_id, &cwd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(out, "Unable to attach {run_id}: {error}");
            ExitCode::TASK_FAILED
        }
    }
}

/// A JSON value for `null`-or-string fields.
pub(crate) fn opt(value: Option<f64>) -> Value {
    value.map_or(Value::Null, Value::from)
}
