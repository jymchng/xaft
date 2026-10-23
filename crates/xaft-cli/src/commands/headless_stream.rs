//! `xaft run --headless` with no task: agenthicc's stdin protocol.
//!
//! agenthicc's headless runner (`runners/headless.py`) reads one intent per
//! stdin line and writes JSON lines to stdout, so another program can drive a
//! long-lived session:
//!
//! ```text
//! → {"status": "ready", "mode": "headless", "session_id": null, "workflow": "code_plan"}
//! ← add a --verbose flag
//! → {"session_id": "…", "workflow_name": "code_plan", "run_id": "…", "status": "complete", "phases": [...], "error": null}
//! ```
//!
//! * The ready line comes first; `workflow` is present when `--workflow` was
//!   given.
//! * Blank lines are skipped; `/loop` and `/loops` answer agenthicc's
//!   `loop_interactive_required` error.
//! * With a phased `--workflow`, each line runs the workflow and prints
//!   agenthicc's `WorkflowExecutionResult` shape; when the previous line's run
//!   stopped resumably, the next line resumes it (the line is the user's
//!   continuation), as agenthicc's `_select_headless_workflow_resume` does.
//! * Without a phased workflow, each line is run as a task in the same
//!   session and answers a `RunCompleted` line. agenthicc's client-neutral
//!   runner only *enqueues* the intent there (`IntentCreated`); xaft runs it,
//!   which is what a caller of a headless stream wants.
//!
//! The loop is generic over its input and output so it is testable without
//! a real stdin.

use std::io::Write;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use xaft_config::XaftConfig;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::phased::state::CheckpointStore;
use xaft_runtime::{ExitCode, WorkflowConfig};

use crate::error::XaftError;

use super::workflows::find_phased_run;

/// What one stream needs besides its input and output.
pub struct StreamSettings {
    /// The resolved run config.
    pub config: XaftConfig,
    /// The resolved workflow (from `--workflow` or the config default).
    pub workflow: WorkflowConfig,
    /// The workflow name, when one was selected explicitly.
    pub workflow_name: Option<String>,
    /// Working directory for every run.
    pub working_dir: PathBuf,
    /// Session to continue (`--resume` / `--continue`).
    pub resume_session_id: Option<String>,
    /// `--dry-run`.
    pub dry_run: bool,
    /// `--yes`.
    pub auto_approve: bool,
    /// `--dangerously-skip-permissions`.
    pub dangerously_skip_permissions: bool,
}

fn emit<W: Write>(out: &mut W, value: &Value) -> Result<(), XaftError> {
    writeln!(out, "{value}")?;
    out.flush()?;
    Ok(())
}

/// Run the stdin protocol until EOF.
pub async fn run_stream<R, W>(
    input: R,
    out: &mut W,
    settings: StreamSettings,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError>
where
    R: AsyncBufRead + Unpin,
    W: Write,
{
    let phased_name = match &settings.workflow {
        WorkflowConfig::Phased { name, .. } => Some(name.clone()),
        _ => None,
    };
    let mut ready = json!({
        "status": "ready",
        "mode": "headless",
        "session_id": settings.resume_session_id,
    });
    if let Some(name) = settings.workflow_name.as_ref().or(phased_name.as_ref()) {
        ready["workflow"] = json!(name);
    }
    emit(out, &ready)?;

    let store = CheckpointStore::for_working_dir(&settings.working_dir);
    let mut session_id = settings.resume_session_id.clone();
    let mut resumable_run: Option<String> = None;
    let mut lines = input.lines();
    while let Some(line) = lines.next_line().await? {
        let intent = line.trim();
        if intent.is_empty() {
            continue;
        }
        if intent == "/loops" || intent == "/loop" || intent.starts_with("/loop ") {
            emit(
                out,
                &json!({
                    "event_type": "Error",
                    "code": "loop_interactive_required",
                    "message": "Loop scheduling and job management require an interactive TUI session.",
                }),
            )?;
            continue;
        }

        let mut workflow = settings.workflow.clone();
        let resumed = match &mut workflow {
            WorkflowConfig::Phased { resume_run_id, .. } => {
                *resume_run_id = resumable_run.take();
                resume_run_id.clone()
            }
            _ => None,
        };
        let request = RunRequest {
            task: intent.to_string(),
            config: settings.config.clone(),
            working_dir: settings.working_dir.clone(),
            headless: true,
            dry_run: settings.dry_run,
            auto_approve: settings.auto_approve,
            dangerously_skip_permissions: settings.dangerously_skip_permissions,
            resume_session_id: session_id.clone(),
            workflow,
            prior_messages: vec![],
            user_message: None,
            mode_system_patch: None,
            mode_tool_filter: None,
            mode_approval_filter: None,
        };
        let started = chrono::Utc::now();
        let outcome = runtime.run(request).await;
        if let Ok(result) = &outcome {
            session_id = Some(result.session.id.to_string());
        }

        let line_out = match &phased_name {
            Some(name) => {
                let state = find_phased_run(&store, name, started, resumed.as_deref());
                if let Some(s) = &state {
                    if s.status.is_resumable() {
                        resumable_run = Some(s.run_id.clone());
                    }
                    if session_id.is_none() {
                        session_id = Some(s.session_id.clone());
                    }
                }
                workflow_result(name, session_id.as_deref(), state.as_ref(), &outcome)
            }
            None => run_result(session_id.as_deref(), &outcome),
        };
        emit(out, &line_out)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// agenthicc's `WorkflowExecutionResult.to_dict()` shape.
fn workflow_result(
    workflow: &str,
    session_id: Option<&str>,
    state: Option<&xaft_runtime::phased::state::WorkflowRunState>,
    outcome: &Result<xaft_runtime::RunResult, xaft_runtime::RuntimeError>,
) -> Value {
    let error = outcome.as_ref().err().map(ToString::to_string);
    let status = state
        .map(|s| s.status.label().to_string())
        .unwrap_or_else(|| {
            if error.is_some() {
                "failed"
            } else {
                "complete"
            }
            .into()
        });
    json!({
        "session_id": session_id,
        "workflow_name": workflow,
        "run_id": state.map(|s| s.run_id.clone()).unwrap_or_default(),
        "status": status,
        "phases": state
            .map(|s| s.phase_history.iter().map(|r| r.phase_name.clone()).collect::<Vec<_>>())
            .unwrap_or_default(),
        "summary": outcome.as_ref().map(|r| r.summary.clone()).unwrap_or_default(),
        "error": error,
    })
}

fn run_result(
    session_id: Option<&str>,
    outcome: &Result<xaft_runtime::RunResult, xaft_runtime::RuntimeError>,
) -> Value {
    match outcome {
        Ok(r) => json!({
            "event_type": "RunCompleted",
            "session_id": session_id,
            "status": if r.exit_code.is_success() { "complete" } else { "failed" },
            "exit_code": r.exit_code.code(),
            "summary": r.summary,
        }),
        Err(e) => json!({
            "event_type": "RunCompleted",
            "session_id": session_id,
            "status": "failed",
            "exit_code": e.exit_code().code(),
            "error": e.to_string(),
        }),
    }
}

/// The real stdin/stdout entry point used by `xaft run --headless`.
pub async fn run_stdin(
    settings: StreamSettings,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = std::io::stdout();
    run_stream(stdin, &mut stdout, settings, runtime).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_runs(path: &Path) -> bool {
        CheckpointStore::for_working_dir(path)
            .list()
            .map(|l| !l.is_empty())
            .unwrap_or(false)
    }
    use async_trait::async_trait;
    use std::sync::Mutex;
    use xaft_runtime::RuntimeError;
    use xaft_runtime::dispatch::RunResult;
    use xaft_runtime::phased::state::{RunStatus, WorkflowRunState};
    use xaft_runtime::session::AgentSession;

    /// Records each request; for phased requests, writes the checkpoint a real
    /// run would, failing the first run so the second line must resume it.
    struct Scripted {
        seen: Mutex<Vec<RunRequest>>,
    }

    #[async_trait]
    impl RuntimeDispatch for Scripted {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let n = {
                let mut seen = self.seen.lock().unwrap();
                seen.push(req.clone());
                seen.len()
            };
            let session = AgentSession::new(
                req.task.clone(),
                req.working_dir.clone(),
                "default".into(),
                "m".into(),
            );
            if let WorkflowConfig::Phased {
                name,
                resume_run_id,
                ..
            } = &req.workflow
            {
                let store = CheckpointStore::for_working_dir(&req.working_dir);
                let mut state = match resume_run_id {
                    Some(id) => store.load(id).unwrap(),
                    None => WorkflowRunState::new(
                        format!("run{n}"),
                        name.as_str(),
                        req.task.as_str(),
                        session.id.as_str(),
                        req.working_dir.clone(),
                        "fp",
                    ),
                };
                state.status = if n == 1 {
                    RunStatus::Failed
                } else {
                    RunStatus::Complete
                };
                store.save(&mut state).unwrap();
                if n == 1 {
                    return Err(RuntimeError::Agent("phase 'execute' failed".into()));
                }
            }
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: format!("did: {}", req.task),
            })
        }
        async fn list_sessions(
            &self,
            _: &std::path::Path,
        ) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![])
        }
        async fn resume_session(&self, _: &str, _: XaftConfig) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    fn settings(dir: &Path, workflow: WorkflowConfig, name: Option<&str>) -> StreamSettings {
        StreamSettings {
            config: XaftConfig::default(),
            workflow,
            workflow_name: name.map(str::to_string),
            working_dir: dir.to_path_buf(),
            resume_session_id: None,
            dry_run: false,
            auto_approve: false,
            dangerously_skip_permissions: false,
        }
    }

    fn lines(out: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(out)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn plain_lines_run_in_one_session_after_a_ready_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(Scripted {
            seen: Mutex::new(vec![]),
        });
        let input: &[u8] = b"first task\n\n   \n/loop 5m check\nsecond task\n";
        let mut out = Vec::new();
        run_stream(
            input,
            &mut out,
            settings(tmp.path(), WorkflowConfig::Standard, None),
            runtime.clone(),
        )
        .await
        .unwrap();
        let events = lines(&out);
        assert_eq!(events[0]["status"], "ready");
        assert_eq!(events[0]["mode"], "headless");
        assert_eq!(events[1]["event_type"], "RunCompleted");
        assert_eq!(events[1]["summary"], "did: first task");
        assert_eq!(events[2]["code"], "loop_interactive_required");
        assert_eq!(events[3]["summary"], "did: second task");
        let seen = runtime.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            2,
            "blank and /loop lines never reach the runtime"
        );
        assert!(
            seen[1].resume_session_id.is_some(),
            "the second line continues the session"
        );
    }

    #[tokio::test]
    async fn a_workflow_stream_resumes_a_run_that_stopped_resumably() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = Arc::new(Scripted {
            seen: Mutex::new(vec![]),
        });
        let workflow = WorkflowConfig::Phased {
            name: "code_plan".into(),
            definition: Box::default(),
            resume_run_id: None,
        };
        let input: &[u8] = b"add a flag\nuse clap derive instead\n";
        let mut out = Vec::new();
        run_stream(
            input,
            &mut out,
            settings(tmp.path(), workflow, Some("code_plan")),
            runtime.clone(),
        )
        .await
        .unwrap();
        let events = lines(&out);
        assert_eq!(events[0]["workflow"], "code_plan");
        assert_eq!(events[1]["status"], "failed");
        assert_eq!(events[1]["run_id"], "run1");
        assert!(events[1]["error"].as_str().unwrap().contains("execute"));
        assert_eq!(events[2]["status"], "complete");
        assert_eq!(
            events[2]["run_id"], "run1",
            "the second line resumed the same run"
        );
        let seen = runtime.seen.lock().unwrap();
        match &seen[1].workflow {
            WorkflowConfig::Phased { resume_run_id, .. } => {
                assert_eq!(resume_run_id.as_deref(), Some("run1"));
            }
            other => panic!("expected a phased request, got {other:?}"),
        }
        assert!(has_runs(tmp.path()));
    }
}
