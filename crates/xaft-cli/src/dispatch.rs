//! Top-level dispatch logic.

use std::sync::Arc;

use xaft_runtime::ExitCode;
use xaft_runtime::SessionBackend;
use xaft_runtime::dispatch::RuntimeDispatch;

use crate::args::{Commands, XaftCli};
use crate::commands::{
    agents, attach, completions, config, init, jobs, mcp, run, runs, session, skills, trust,
    version, workflows, worktrees,
};
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
/// * `sessions` — The history-reading half of the same store. `main.rs`
///   constructs one `SessionManager`, hands its `session_store()` to `runtime`
///   and the manager itself here, so `xaft session export`/`inspect` read the
///   database the runtime was bootstrapped with instead of resolving
///   `core.data_dir` a second time. `None` when SQLite was unavailable and xaft
///   fell back to a filesystem store.
pub async fn dispatch(
    cli: XaftCli,
    runtime: Arc<dyn RuntimeDispatch>,
    sessions: Option<Arc<dyn SessionBackend>>,
) -> Result<ExitCode, XaftError> {
    // Determine log level and json flag from run args (before config load)
    let (log_level, json_output, tui_mode) = match &cli.command {
        Some(Commands::Run(args)) => {
            let level = args
                .log_level
                .as_ref()
                .map(|l| l.to_log_level())
                .unwrap_or(xaft_config::LogLevel::Info);
            let tui = !args.headless && !args.json && cfg!(feature = "tui");
            (level, args.json, tui)
        }
        _ => (xaft_config::LogLevel::Info, false, false),
    };

    // Initialise tracing. When the TUI is active, redirect to a log file under
    // data_dir so output does not bleed into the alternate screen.
    // data_dir comes from config (defaults to ~/.xaft, overridable in .xaft.toml
    // via [core] data_dir = "/path/to/dir").
    if tui_mode {
        // Config may not be loaded yet — use a quick peek at the default data_dir.
        // The full config is loaded inside handle_run; we only need the path here.
        let data_dir = xaft_config::defaults::default_data_dir();
        let _ = std::fs::create_dir_all(&data_dir);
        let log_path = data_dir.join(format!("debug-{}.log", std::process::id()));
        tracing_init::init_to_file(&log_level, &log_path);
    } else {
        tracing_init::init(&log_level, json_output);
    }

    tracing::debug!("dispatching command");

    match cli.command {
        // Bare `xaft` with no subcommand → interactive TUI with no initial task
        None => run::handle_run_interactive(runtime, None).await,
        Some(Commands::Run(ref args)) => run::handle_run(args, runtime).await,
        Some(Commands::Config(ref cmd)) => config::handle_config(&cmd.subcommand).await,
        Some(Commands::Session(ref cmd)) => {
            session::handle_session(&cmd.subcommand, runtime, sessions).await
        }
        Some(Commands::Jobs(ref cmd)) => jobs::handle_jobs(&cmd.subcommand).await,
        Some(Commands::Runs(ref cmd)) => runs::handle_runs(&cmd.resolve()).await,
        Some(Commands::Mcp(ref cmd)) => mcp::handle_mcp(&cmd.subcommand).await,
        Some(Commands::Workflows(ref cmd)) => {
            let config = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides::default())?;
            workflows::handle_workflows(&cmd.subcommand, &config, runtime).await
        }
        Some(Commands::Completions(ref args)) => completions::handle_completions(args).await,
        Some(Commands::Version(ref args)) => version::handle_version(args).await,
        Some(Commands::Init(ref args)) => init::handle_init(args).await,
        Some(Commands::Skills(ref cmd)) => skills::handle_skills(&cmd.subcommand).await,
        Some(Commands::Trust(ref cmd)) => trust::handle_trust(&cmd.subcommand).await,
        Some(Commands::Worktrees(ref cmd)) => worktrees::handle_worktrees(&cmd.subcommand).await,
        Some(Commands::Agents(ref args)) => agents::handle_agents(args, sessions).await,
        Some(Commands::Attach(ref args)) => attach::handle_attach(args, sessions).await,
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
///     xaft_cli::run(runtime, None).await;
/// }
/// ```
pub async fn run(runtime: Arc<dyn RuntimeDispatch>, sessions: Option<Arc<dyn SessionBackend>>) {
    let cli = match parse_with_plugins() {
        Parsed::Builtin(cli) => cli,
        Parsed::Plugin(code) => std::process::exit(code),
    };

    match dispatch(cli, runtime, sessions).await {
        Ok(code) if code.is_success() => {}
        Ok(code) => std::process::exit(code.code() as i32),
        Err(e) => {
            e.print_diagnostic();
            std::process::exit(e.exit_code().code() as i32);
        }
    }
}

/// What the process's arguments turned out to name.
enum Parsed {
    /// A built-in command, to be dispatched.
    Builtin(XaftCli),
    /// A CLI plugin that has already run, with its exit status.
    Plugin(i32),
}

/// Parse `std::env::args()` against the built-in tree plus trusted plugins.
///
/// Plugins come from `cli.toml` (see [`crate::cli_plugins`]): the user's, and
/// the project's once `xaft trust` has recorded its exact content. Discovery
/// warnings (an untrusted or malformed table, a shadowed command) go to
/// stderr before parsing, so a user who expected a project command to exist
/// is told why it does not.
///
/// `xaft trust …` never loads plugins. Trust management is the escape hatch
/// for a table that fails to load — including one rejected by
/// `[plugins] strict_cli_shadow` — so it must not depend on that table, in
/// the same way agenthicc answers `--help` without importing project code.
fn parse_with_plugins() -> Parsed {
    use clap::{CommandFactory, FromArgMatches};

    let builtin = XaftCli::command();
    let first = std::env::args().nth(1);
    let skip_plugins = first.as_deref() == Some("trust");

    let registry = if skip_plugins {
        crate::cli_plugins::PluginRegistry::default()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let builtins = crate::cli_plugins::builtin_names(&builtin);
        match crate::cli_plugins::discover_default(&cwd, &builtins) {
            Ok(discovery) => {
                for warning in &discovery.warnings {
                    eprintln!("xaft: {warning}");
                }
                discovery.registry
            }
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
    };

    let matches = registry.augment(builtin).get_matches();
    match registry.resolve(&matches) {
        Some(Ok(invocation)) => Parsed::Plugin(invocation.run()),
        Some(Err(error)) => {
            eprintln!("error: {error}");
            Parsed::Plugin(1)
        }
        None => Parsed::Builtin(XaftCli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())),
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
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_version_command() {
        let cli = XaftCli::try_parse_from(["xaft", "version"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_config_show() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli = XaftCli::try_parse_from(["xaft", "config", "show", "-c", cfg.to_str().unwrap()])
            .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_config_validate() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli =
            XaftCli::try_parse_from(["xaft", "config", "validate", "-c", cfg.to_str().unwrap()])
                .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_config_profiles() {
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);

        let cli =
            XaftCli::try_parse_from(["xaft", "config", "profiles", "-c", cfg.to_str().unwrap()])
                .unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_session_list() {
        let cli = XaftCli::try_parse_from(["xaft", "session", "list"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_completions() {
        let cli = XaftCli::try_parse_from(["xaft", "completions", "bash"]).unwrap();
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn dispatch_run_no_task_opens_interactive() {
        // `xaft run --headless` with no task returns a Usage error.
        let tmp = TempDir::new().unwrap();
        let cfg = write_minimal_config(&tmp);
        let cli =
            XaftCli::try_parse_from(["xaft", "run", "--headless", "-c", cfg.to_str().unwrap()])
                .unwrap();
        let err = dispatch(cli, Arc::new(PassthroughRuntime), None)
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
        let code = dispatch(cli, Arc::new(PassthroughRuntime), None)
            .await
            .unwrap();
        assert!(code.is_success());
    }
}
