//! Handler for `xaft run`.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::ConfigLoader;
use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};

use crate::args::RunArgs;
use crate::error::XaftError;

/// Execute `xaft run`.
pub async fn handle_run(
    args: &RunArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    // 1. Build CLI overrides from args
    let overrides = args.to_cli_overrides();

    // 2. Load config with overrides applied
    let config = ConfigLoader::load(&overrides)?;

    // 3. Determine the task — if not provided, open interactive TUI (non-headless only)
    let task = match args.task.clone() {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            if args.headless || args.json {
                return Err(XaftError::Usage(
                    "task description is required for headless/JSON mode".into(),
                ));
            }
            return handle_run_interactive(runtime).await;
        }
    };

    // 4. Determine working directory
    let working_dir = args
        .project_dir
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // 5. Build run request
    let request = RunRequest {
        task: task.clone(),
        config,
        working_dir,
        headless: args.headless || args.json,
        dry_run: args.dry_run,
        auto_approve: args.auto_approve,
        dangerously_skip_permissions: args.dangerously_skip_permissions,
        resume_session_id: args.session.clone(),
    };

    tracing::info!(
        task = %task,
        dry_run = args.dry_run,
        headless = request.headless,
        "dispatching run request"
    );

    // 6. Dispatch: use TUI when interactive + real terminal, bare runtime when headless
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
        resume_session_id: None,
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
}
