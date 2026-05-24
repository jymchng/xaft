//! Top-level dispatch logic.

use std::sync::Arc;

use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::RuntimeDispatch;

use crate::args::{Commands, XaftCli};
use crate::commands::{completions, config, run, session, version};
use crate::error::XaftError;
use crate::tracing_init;

/// Main entry point: parse, configure, and dispatch a CLI invocation.
///
/// This function:
/// 1. Reads the base log level from the run args (if applicable)
/// 2. Initialises tracing
/// 3. Dispatches to the appropriate command handler
///
/// # Arguments
///
/// * `cli` — Parsed CLI arguments.
/// * `runtime` — Runtime dispatch backend. Use `StubRuntime` for testing;
///   the full `XaftRuntime` when the agent layer is ready.
pub async fn dispatch(
    cli: XaftCli,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    // Determine log level and json flag from run args (before config load)
    let (log_level, json_output) = match &cli.command {
        Commands::Run(args) => {
            let level = args
                .log_level
                .as_ref()
                .map(|l| l.to_log_level())
                .unwrap_or(xaft_config::LogLevel::Info);
            (level, args.json)
        }
        _ => (xaft_config::LogLevel::Info, false),
    };

    // Initialise tracing (idempotent — if already initialised, this is a no-op via try_init)
    tracing_init::init(&log_level, json_output);

    tracing::debug!(command = ?std::mem::discriminant(&cli.command), "dispatching command");

    match cli.command {
        Commands::Run(ref args) => run::handle_run(args, runtime).await,
        Commands::Config(ref cmd) => config::handle_config(&cmd.subcommand).await,
        Commands::Session(ref cmd) => session::handle_session(&cmd.subcommand, runtime).await,
        Commands::Completions(ref args) => completions::handle_completions(args).await,
        Commands::Version(ref args) => version::handle_version(args).await,
    }
}

/// Convenience wrapper: parse from `std::env::args()` and dispatch.
///
/// Handles error display and exit code translation. Designed to be called
/// directly from `fn main()`.
///
/// ```rust,no_run
/// #[tokio::main]
/// async fn main() {
///     let runtime = std::sync::Arc::new(xaft_runtime::dispatch::StubRuntime);
///     xaft_cli::run(runtime).await;
/// }
/// ```
pub async fn run(runtime: Arc<dyn RuntimeDispatch>) {
    use clap::Parser;

    let cli = XaftCli::parse();

    match dispatch(cli, runtime).await {
        Ok(code) if code.is_success() => {}
        Ok(code) => std::process::exit(code.code() as i32),
        Err(e) => {
            e.print_diagnostic();
            std::process::exit(e.exit_code().code() as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use clap::Parser;
    use std::path::Path;
    use tempfile::TempDir;
    use xaft_runtime::RuntimeError;
    use xaft_runtime::dispatch::{RunRequest, RunResult};
    use xaft_runtime::session::AgentSession;

    struct PassthroughRuntime;

    #[async_trait]
    impl RuntimeDispatch for PassthroughRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
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
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
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

    fn write_minimal_config(dir: &TempDir) -> std::path::PathBuf {
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
    async fn dispatch_run_command() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli = XaftCli::try_parse_from([
            "xaft",
            "run",
            "test task",
            "-c",
            cfg.to_str().unwrap(),
            "--headless",
        ])
        .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_version_command() {
        let cli = XaftCli::try_parse_from(["xaft", "version"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_config_show() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli = XaftCli::try_parse_from(["xaft", "config", "show", "-c", cfg.to_str().unwrap()])
            .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_config_validate() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli =
            XaftCli::try_parse_from(["xaft", "config", "validate", "-c", cfg.to_str().unwrap()])
                .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_session_list() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "list"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_completions() {
        let cli = XaftCli::try_parse_from(["xaft", "completions", "bash"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_run_no_task_fails() {
        let cli = XaftCli::try_parse_from(["xaft", "run"]).unwrap();
        let err = dispatch(cli, Arc::new(PassthroughRuntime))
            .await
            .unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn dispatch_run_with_dry_run() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli = XaftCli::try_parse_from([
            "xaft",
            "run",
            "task",
            "--dry-run",
            "--headless",
            "-c",
            cfg.to_str().unwrap(),
        ])
        .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime)).await.unwrap();
        assert!(code.is_success());
    }
}
