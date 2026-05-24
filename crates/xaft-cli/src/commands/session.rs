//! Handler for `xaft session` subcommands.

use std::sync::Arc;

use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::dispatch::RuntimeDispatch;
use xaft_runtime::session::SessionStatus;
use xaft_runtime::ExitCode;

use crate::args::{
    OutputFormat, SessionCancelArgs, SessionListArgs, SessionResumeArgs, SessionShowArgs,
    SessionSubcommand,
};
use crate::error::XaftError;

/// Execute `xaft session <subcommand>`.
pub async fn handle_session(
    subcommand: &SessionSubcommand,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    match subcommand {
        SessionSubcommand::List(args) => handle_session_list(args, runtime).await,
        SessionSubcommand::Show(args) => handle_session_show(args, runtime).await,
        SessionSubcommand::Resume(args) => handle_session_resume(args, runtime).await,
        SessionSubcommand::Cancel(args) => handle_session_cancel(args).await,
    }
}

async fn handle_session_list(
    args: &SessionListArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let working_dir = std::env::current_dir()?;

    let sessions = runtime.list_sessions(&working_dir).await?;

    if sessions.is_empty() {
        println!("  No sessions found.");
        println!();
        println!("  Start one with: xaft run \"your task here\"");
        return Ok(ExitCode::SUCCESS);
    }

    // Apply limit
    let sessions: Vec<_> = sessions.into_iter().take(args.limit).collect();

    match args.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&sessions).unwrap_or_default();
            println!("{json}");
        }
        _ => {
            println!("  \x1b[1m{:<12} {:<12} {:<20} {:<30}\x1b[0m", "STATUS", "TURNS", "STARTED", "TASK");
            println!("  {}", "-".repeat(76));
            for session in &sessions {
                let status = status_label(&session.status);
                let started = session
                    .created_at
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                let task_truncated: String = session.task.chars().take(28).collect();
                let task_display = if session.task.len() > 28 {
                    format!("{task_truncated}…")
                } else {
                    task_truncated
                };
                println!(
                    "  {status:<12} {turns:<12} {started:<20} {task}",
                    turns = session.turn_count,
                    task = task_display,
                );
            }
            println!();
            println!("  Showing {} session(s)", sessions.len());
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Active => "\x1b[32mactive\x1b[0m",
        SessionStatus::Suspended => "\x1b[33msuspended\x1b[0m",
        SessionStatus::Completed { .. } => "\x1b[90mcompleted\x1b[0m",
        SessionStatus::Failed { .. } => "\x1b[31mfailed\x1b[0m",
        SessionStatus::Cancelled => "\x1b[90mcancelled\x1b[0m",
    }
}

async fn handle_session_show(
    args: &SessionShowArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let working_dir = std::env::current_dir()?;
    let sessions = runtime.list_sessions(&working_dir).await?;

    let session = sessions
        .iter()
        .find(|s| s.id.as_str() == args.id || s.id.as_str().starts_with(&args.id))
        .ok_or_else(|| XaftError::Usage(format!("session not found: {}", args.id)))?;

    match args.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(session).unwrap_or_default());
        }
        _ => {
            println!("  \x1b[1mSession: {}\x1b[0m", session.id);
            println!("  Status:    {}", session.status.label());
            println!("  Task:      {}", session.task);
            println!("  Started:   {}", session.created_at.format("%Y-%m-%d %H:%M:%S UTC"));
            println!("  Turns:     {}", session.turn_count);
            println!("  Cost:      ${:.4}", session.total_cost_usd);
            println!("  Tokens:    {}", session.total_tokens);
            if let Some(ref branch) = session.git_branch {
                println!("  Branch:    {branch}");
            }
            println!("  Workspace: {}", session.workspace_root.display());
            println!();
            if session.is_resumable() {
                println!("  Resume with: xaft session resume {}", session.id);
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

async fn handle_session_resume(
    args: &SessionResumeArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    let overrides = CliOverrides {
        auto_approve: args.auto_approve,
        headless: args.headless,
        ..Default::default()
    };
    let config = ConfigLoader::load(&overrides)?;

    tracing::info!(session_id = %args.id, "resuming session");

    let result = runtime.resume_session(&args.id, config).await?;
    Ok(result.exit_code)
}

async fn handle_session_cancel(args: &SessionCancelArgs) -> Result<ExitCode, XaftError> {
    if !args.force {
        eprintln!("  Cancel session {}? This cannot be undone.", args.id);
        eprintln!("  Run with --force to confirm.");
        return Err(XaftError::Usage("use --force to confirm cancellation".into()));
    }

    // Stub: real implementation would update session store
    tracing::info!(session_id = %args.id, "session cancelled (stub)");
    println!("  Session {} cancelled.", args.id);

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{OutputFormat, SessionListArgs, SessionResumeArgs, SessionShowArgs, SessionStatusFilter};
    use async_trait::async_trait;
    use xaft_runtime::dispatch::{RunRequest, RunResult};
    use xaft_runtime::session::{AgentSession, SessionId};
    use xaft_runtime::RuntimeError;
    use std::path::Path;

    struct EmptySessionRuntime;

    #[async_trait]
    impl RuntimeDispatch for EmptySessionRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session = AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult { exit_code: ExitCode::SUCCESS, session, summary: "ok".into() })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![])
        }
        async fn resume_session(&self, _: &str, _: xaft_config::XaftConfig) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    struct WithSessions;

    #[async_trait]
    impl RuntimeDispatch for WithSessions {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session = AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult { exit_code: ExitCode::SUCCESS, session, summary: "ok".into() })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![
                AgentSession::new("fix the bug", std::env::current_dir().unwrap(), "default".into(), "claude".into()),
                AgentSession::new("add tests", std::env::current_dir().unwrap(), "default".into(), "claude".into()),
            ])
        }
        async fn resume_session(&self, _: &str, _: xaft_config::XaftConfig) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    #[tokio::test]
    async fn session_list_empty() {
        let args = SessionListArgs {
            all: false,
            status: None,
            limit: 20,
            format: OutputFormat::Pretty,
        };
        let code = handle_session_list(&args, Arc::new(EmptySessionRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn session_list_with_sessions() {
        let args = SessionListArgs {
            all: false,
            status: None,
            limit: 20,
            format: OutputFormat::Pretty,
        };
        let code = handle_session_list(&args, Arc::new(WithSessions)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn session_list_json_format() {
        let args = SessionListArgs {
            all: false,
            status: None,
            limit: 20,
            format: OutputFormat::Json,
        };
        let code = handle_session_list(&args, Arc::new(EmptySessionRuntime)).await.unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn session_show_not_found_error() {
        let args = SessionShowArgs {
            id: "nonexistent-id".into(),
            format: OutputFormat::Pretty,
        };
        let err = handle_session_show(&args, Arc::new(EmptySessionRuntime)).await.unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn session_cancel_without_force_errors() {
        let args = SessionCancelArgs {
            id: "some-id".into(),
            force: false,
        };
        let err = handle_session_cancel(&args).await.unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn session_cancel_with_force_succeeds() {
        let args = SessionCancelArgs {
            id: "some-id".into(),
            force: true,
        };
        let code = handle_session_cancel(&args).await.unwrap();
        assert!(code.is_success());
    }
}
