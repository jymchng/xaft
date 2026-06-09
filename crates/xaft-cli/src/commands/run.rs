//! Handler for `xaft run`.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::ConfigLoader;
use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};

use crate::args::RunArgs;
use crate::error::XaftError;

/// Resolve the session ID to resume.
///
/// Priority: explicit `--resume <id>` > `--continue` (latest session for dir) > None.
async fn resolve_resume_id(
    args: &RunArgs,
    runtime: &Arc<dyn RuntimeDispatch>,
    working_dir: &std::path::Path,
) -> Option<String> {
    // Explicit --resume always wins.
    if let Some(id) = args.resume.clone().or(args.session.clone()) {
        return Some(id);
    }
    // --continue: find the most recently updated *resumable* session for this directory.
    if args.r#continue {
        match runtime.list_sessions(working_dir).await {
            Ok(mut sessions) => {
                sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                // Skip Failed/Cancelled sessions — they cannot be resumed.
                if let Some(s) = sessions.into_iter().find(|s| s.is_resumable()) {
                    return Some(s.id.to_string());
                }
                eprintln!(
                    "xaft: --continue: no prior session found for this directory, starting fresh"
                );
            }
            Err(e) => {
                eprintln!(
                    "xaft: --continue: could not list sessions ({}), starting fresh",
                    e
                );
            }
        }
    }
    None
}

/// Execute `xaft run`.
pub async fn handle_run(
    args: &RunArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    // 1. Build CLI overrides from args
    let overrides = args.to_cli_overrides();

    // 2. Load config with overrides applied
    let config = ConfigLoader::load(&overrides)?;

    // 3. Determine working directory (needed for --continue resolution).
    let working_dir = args
        .project_dir
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // 4. Resolve resume session ID (--resume / --continue).
    let resume_session_id = resolve_resume_id(args, &runtime, &working_dir).await;

    // 5. Determine the task — if not provided, open interactive TUI (non-headless only)
    let task = match args.task.clone() {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            if args.headless || args.json {
                return Err(XaftError::Usage(
                    "task description is required for headless/JSON mode".into(),
                ));
            }
            return handle_run_interactive(runtime, resume_session_id).await;
        }
    };

    // 6. Build run request
    let request = RunRequest {
        task: task.clone(),
        config,
        working_dir,
        headless: args.headless || args.json,
        dry_run: args.dry_run,
        auto_approve: args.auto_approve,
        dangerously_skip_permissions: args.dangerously_skip_permissions,
        resume_session_id,
        workflow: xaft_runtime::WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
    };

    tracing::info!(
        task = %task,
        dry_run = args.dry_run,
        headless = request.headless,
        "dispatching run request"
    );

    // 7. Dispatch: use TUI when interactive + real terminal, bare runtime when headless
    if !request.headless && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        #[cfg(feature = "tui")]
        {
            let tui_app = xaft_tui::TuiApp::new(request.config.clone());
            tui_app.run(request).await.map_err(|e| {
                XaftError::Runtime(xaft_runtime::RuntimeError::Agent(e.to_string()))
            })?;
            return Ok(ExitCode::SUCCESS);
        }
        #[cfg(not(feature = "tui"))]
        {
            let result = runtime.run(request).await?;
            return Ok(result.exit_code);
        }
    }

    let result = runtime.run(request).await?;
    Ok(result.exit_code)
}

/// Open the TUI in interactive mode with no initial task.
///
/// The InputBar is focused and the user types a task before the agent starts.
/// Used when `xaft` is invoked with no arguments.
pub async fn handle_run_interactive(
    runtime: Arc<dyn RuntimeDispatch>,
    resume_session_id: Option<String>,
) -> Result<ExitCode, XaftError> {
    // If stdout is not a real terminal (e.g. CI, pipes, test harness), fall back
    // to headless mode with an error so we don't corrupt the terminal.
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("xaft: no interactive terminal — use `xaft run \"task description\"`");
        return Ok(ExitCode(1));
    }

    let config = ConfigLoader::load(&xaft_config::CliOverrides::default())?;
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let request = RunRequest {
        task: String::new(), // empty — TUI waits for user input
        config,
        working_dir,
        headless: false,
        dry_run: false,
        auto_approve: false,
        dangerously_skip_permissions: false,
        resume_session_id,
        workflow: xaft_runtime::WorkflowConfig::default(),
        prior_messages: vec![],
        user_message: None,
    };

    #[cfg(feature = "tui")]
    {
        let tui_app = xaft_tui::TuiApp::new(request.config.clone());
        tui_app
            .run(request)
            .await
            .map_err(|e| XaftError::Runtime(xaft_runtime::RuntimeError::Agent(e.to_string())))?;
        return Ok(ExitCode::SUCCESS);
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = runtime;
        eprintln!("xaft: TUI not available — use `xaft run \"task\"`");
        return Ok(ExitCode(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{LogLevelArg, RunArgs};
    use async_trait::async_trait;
    use xaft_runtime::{
        RuntimeError,
        dispatch::{RunResult, RuntimeDispatch},
        session::{AgentSession, SessionId},
    };

    struct SuccessRuntime;

    #[async_trait]
    impl RuntimeDispatch for SuccessRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session = AgentSession::new(
                req.task.clone(),
                req.working_dir.clone(),
                "default".into(),
                "test-model".into(),
            );
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: format!("ran: {}", req.task),
            })
        }

        async fn list_sessions(
            &self,
            _: &std::path::Path,
        ) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![])
        }

        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    fn make_run_args(task: &str) -> RunArgs {
        RunArgs {
            task: Some(task.into()),
            model: None,
            provider: None,
            agent: None,
            max_turns: None,
            temperature: None,
            r#continue: false,
            resume: None,
            session: None,
            config: None,
            project_dir: None,
            headless: true,
            json: false,
            dry_run: false,
            auto_approve: true,
            dangerously_skip_permissions: false,
            log_level: Some(LogLevelArg::Error),
            no_telemetry: true,
        }
    }

    fn write_minimal_config(dir: &tempfile::TempDir) -> PathBuf {
        let path = dir.path().join("xaft.toml");
        std::fs::write(
            &path,
            r#"
[core]
log_level = "error"
telemetry = false

[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
timeout_secs = 120

[agent.default]
model = "claude-3-5-sonnet-20241022"
provider = "anthropic"
max_turns = 25
temperature = 0.0
top_p = 1.0
"#,
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn run_with_task_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = write_minimal_config(&tmp);

        let mut args = make_run_args("test task");
        args.config = Some(config_path);

        let runtime = Arc::new(SuccessRuntime);
        let code = handle_run(&args, runtime).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn run_without_task_headless_returns_usage_error() {
        // headless + no task → Usage error (not TUI redirect)
        let args = RunArgs {
            task: None,
            headless: true,
            ..make_run_args("unused")
        };
        let runtime = Arc::new(SuccessRuntime);
        let err = handle_run(&args, runtime).await.unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn run_empty_task_headless_returns_usage_error() {
        let mut args = make_run_args("   ");
        args.headless = true;
        let runtime = Arc::new(SuccessRuntime);
        let err = handle_run(&args, runtime).await.unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn run_with_dry_run_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = write_minimal_config(&tmp);

        let mut args = make_run_args("test task");
        args.config = Some(config_path);
        args.dry_run = true;

        struct CheckDryRun;
        #[async_trait]
        impl RuntimeDispatch for CheckDryRun {
            async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
                assert!(req.dry_run);
                let session = AgentSession::new(
                    req.task.clone(),
                    req.working_dir.clone(),
                    "default".into(),
                    "m".into(),
                );
                Ok(RunResult {
                    exit_code: ExitCode::SUCCESS,
                    session,
                    summary: "ok".into(),
                })
            }
            async fn list_sessions(
                &self,
                _: &std::path::Path,
            ) -> Result<Vec<AgentSession>, RuntimeError> {
                Ok(vec![])
            }
            async fn resume_session(
                &self,
                _: &str,
                _: xaft_config::XaftConfig,
            ) -> Result<RunResult, RuntimeError> {
                Err(RuntimeError::NotImplemented("stub".into()))
            }
        }

        let code = handle_run(&args, Arc::new(CheckDryRun)).await.unwrap();
        assert!(code.is_success());
    }

    // ── resolve_resume_id tests ───────────────────────────────────────────────

    fn make_session(status: xaft_runtime::session::SessionStatus) -> AgentSession {
        let mut s = AgentSession::new(
            "task",
            std::path::PathBuf::from("/project"),
            "default".into(),
            "model".into(),
        );
        s.status = status;
        s
    }

    // A runtime that returns a fixed list of sessions.
    struct SessionListRuntime(Vec<AgentSession>);

    #[async_trait]
    impl RuntimeDispatch for SessionListRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session = AgentSession::new(req.task.clone(), req.working_dir.clone(), "default".into(), "m".into());
            Ok(RunResult { exit_code: ExitCode::SUCCESS, session, summary: "ok".into() })
        }
        async fn list_sessions(&self, _: &std::path::Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(self.0.clone())
        }
        async fn resume_session(&self, _: &str, _: xaft_config::XaftConfig) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    // A runtime that errors on list_sessions.
    struct ErrorListRuntime;

    #[async_trait]
    impl RuntimeDispatch for ErrorListRuntime {
        async fn run(&self, _: RunRequest) -> Result<RunResult, RuntimeError> { unimplemented!() }
        async fn list_sessions(&self, _: &std::path::Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Err(RuntimeError::NotImplemented("session store unavailable".into()))
        }
        async fn resume_session(&self, _: &str, _: xaft_config::XaftConfig) -> Result<RunResult, RuntimeError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn continue_picks_completed_over_failed() {
        use xaft_runtime::session::SessionStatus;
        let failed = make_session(SessionStatus::Failed { error: "oops".into() });
        let completed = make_session(SessionStatus::Completed { summary: "done".into() });
        let failed_id = failed.id.to_string();
        let completed_id = completed.id.to_string();

        // List: failed has a MORE RECENT updated_at (it would be picked by old `.next()`),
        // but it is not resumable, so the fix should skip it and pick completed.
        let mut sessions = vec![failed.clone(), completed.clone()];
        // Ensure failed sorts first (more recently updated).
        sessions[0].updated_at = chrono::Utc::now() + chrono::Duration::seconds(10);
        sessions[1].updated_at = chrono::Utc::now();

        let runtime = Arc::new(SessionListRuntime(sessions));
        let args = RunArgs {
            r#continue: true,
            headless: true,
            ..make_run_args("task")
        };
        let id = resolve_resume_id(&args, &(runtime as Arc<dyn RuntimeDispatch>), std::path::Path::new("/project")).await;
        assert_eq!(id, Some(completed_id), "should skip Failed and pick Completed");
        assert_ne!(id, Some(failed_id));
    }

    #[tokio::test]
    async fn continue_returns_none_when_all_sessions_non_resumable() {
        use xaft_runtime::session::SessionStatus;
        let sessions = vec![
            make_session(SessionStatus::Failed { error: "e1".into() }),
            make_session(SessionStatus::Cancelled),
        ];
        let runtime = Arc::new(SessionListRuntime(sessions));
        let args = RunArgs {
            r#continue: true,
            headless: true,
            ..make_run_args("task")
        };
        let id = resolve_resume_id(&args, &(runtime as Arc<dyn RuntimeDispatch>), std::path::Path::new("/p")).await;
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn continue_returns_none_on_list_error() {
        let runtime = Arc::new(ErrorListRuntime);
        let args = RunArgs {
            r#continue: true,
            headless: true,
            ..make_run_args("task")
        };
        let id = resolve_resume_id(&args, &(runtime as Arc<dyn RuntimeDispatch>), std::path::Path::new("/p")).await;
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn continue_picks_active_session() {
        use xaft_runtime::session::SessionStatus;
        let active = make_session(SessionStatus::Active);
        let active_id = active.id.to_string();
        let runtime = Arc::new(SessionListRuntime(vec![active]));
        let args = RunArgs {
            r#continue: true,
            headless: true,
            ..make_run_args("task")
        };
        let id = resolve_resume_id(&args, &(runtime as Arc<dyn RuntimeDispatch>), std::path::Path::new("/p")).await;
        assert_eq!(id, Some(active_id));
    }

    #[tokio::test]
    async fn explicit_resume_ignores_continue_flag() {
        use xaft_runtime::session::SessionStatus;
        // Even if --continue is set and there are resumable sessions,
        // an explicit --resume <id> wins.
        let session = make_session(SessionStatus::Active);
        let runtime = Arc::new(SessionListRuntime(vec![session]));
        let args = RunArgs {
            r#continue: true,
            resume: Some("explicit-id".into()),
            headless: true,
            ..make_run_args("task")
        };
        let id = resolve_resume_id(&args, &(runtime as Arc<dyn RuntimeDispatch>), std::path::Path::new("/p")).await;
        assert_eq!(id, Some("explicit-id".into()));
    }
}
