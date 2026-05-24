//! Handler for `xaft run`.

use std::path::PathBuf;
use std::sync::Arc;

use xaft_config::ConfigLoader;
use xaft_runtime::dispatch::{RunRequest, RuntimeDispatch};
use xaft_runtime::ExitCode;

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

    // 3. Determine the task (error if not provided)
    let task = args
        .task
        .clone()
        .ok_or_else(|| XaftError::Usage("task description is required for `xaft run`".into()))?;

    if task.trim().is_empty() {
        return Err(XaftError::Usage(
            "task description must not be empty".into(),
        ));
    }

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
        resume_session_id: args.session.clone(),
    };

    tracing::info!(
        task = %task,
        dry_run = args.dry_run,
        headless = request.headless,
        "dispatching run request"
    );

    // 6. Dispatch to runtime
    let result = runtime.run(request).await?;

    Ok(result.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{LogLevelArg, RunArgs};
    use async_trait::async_trait;
    use xaft_runtime::{
        dispatch::{RunResult, RuntimeDispatch},
        session::{AgentSession, SessionId},
        RuntimeError,
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
    async fn run_without_task_returns_usage_error() {
        let args = RunArgs {
            task: None,
            ..make_run_args("unused")
        };
        let runtime = Arc::new(SuccessRuntime);
        let err = handle_run(&args, runtime).await.unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn run_empty_task_returns_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = write_minimal_config(&tmp);

        let mut args = make_run_args("   ");
        args.config = Some(config_path);

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
                let session = AgentSession::new(req.task.clone(), req.working_dir.clone(), "default".into(), "m".into());
                Ok(RunResult { exit_code: ExitCode::SUCCESS, session, summary: "ok".into() })
            }
            async fn list_sessions(&self, _: &std::path::Path) -> Result<Vec<AgentSession>, RuntimeError> { Ok(vec![]) }
            async fn resume_session(&self, _: &str, _: xaft_config::XaftConfig) -> Result<RunResult, RuntimeError> { Err(RuntimeError::NotImplemented("stub".into())) }
        }

        let code = handle_run(&args, Arc::new(CheckDryRun)).await.unwrap();
        assert!(code.is_success());
    }
}
