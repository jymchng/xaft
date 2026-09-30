//! Handler for `xaft session` subcommands.

use std::sync::Arc;

use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::ExitCode;
use xaft_runtime::dispatch::RuntimeDispatch;
use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};

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
        SessionSubcommand::Cancel(args) => handle_session_cancel(args, runtime).await,
        // The `sessions.py` family: these open the store directly, because
        // `export` needs the full message history the dispatch trait hides.
        SessionSubcommand::Export(args) => crate::commands::sessions::export(args).await,
        SessionSubcommand::Inspect(args) => crate::commands::sessions::inspect(args).await,
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
            println!(
                "  \x1b[1m{:<12} {:<12} {:<20} {:<30}\x1b[0m",
                "STATUS", "TURNS", "STARTED", "TASK"
            );
            println!("  {}", "-".repeat(76));
            for session in &sessions {
                let status = status_label(&session.status);
                let started = session.created_at.format("%Y-%m-%d %H:%M").to_string();
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
    // Resolve by exact ID against the whole store first.
    //
    // This used to scan `list_sessions(&cwd)`, which filters by working
    // directory: a session started somewhere else was reported as "not
    // found" here while `xaft session inspect` — which loads globally by ID —
    // could still show it. The unfiltered lookup removes that contradiction.
    if let Some(session) = runtime
        .load_session(&SessionId::from_string(&args.id))
        .await?
    {
        return print_session(&args.format, &session).await;
    }

    // Fall back to the historical unique-prefix match, still cwd-scoped.
    let working_dir = std::env::current_dir()?;
    let sessions = runtime.list_sessions(&working_dir).await?;

    let session = sessions
        .iter()
        .find(|s| s.id.as_str().starts_with(&args.id))
        .ok_or_else(|| XaftError::Usage(format!("session not found: {}", args.id)))?;

    print_session(&args.format, session).await
}

/// Render one session in the requested format.
async fn print_session(
    format: &OutputFormat,
    session: &AgentSession,
) -> Result<ExitCode, XaftError> {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(session).unwrap_or_default()
            );
        }
        _ => {
            println!("  \x1b[1mSession: {}\x1b[0m", session.id);
            println!("  Status:    {}", session.status.label());
            println!("  Task:      {}", session.task);
            println!(
                "  Started:   {}",
                session.created_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
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

async fn handle_session_cancel(
    args: &SessionCancelArgs,
    runtime: Arc<dyn RuntimeDispatch>,
) -> Result<ExitCode, XaftError> {
    if !args.force {
        eprintln!("  Cancel session {}? This cannot be undone.", args.id);
        eprintln!("  Run with --force to confirm.");
        return Err(XaftError::Usage(
            "use --force to confirm cancellation".into(),
        ));
    }

    // This used to print success without touching the store, so a cancel was
    // silently a no-op. The write now goes through the same runtime handle the
    // other `xaft session` subcommands read from.
    let id = SessionId::from_string(&args.id);
    let mut session = runtime
        .load_session(&id)
        .await?
        .ok_or_else(|| XaftError::Usage(format!("session not found: {}", args.id)))?;

    session.status = SessionStatus::Cancelled;
    session.updated_at = chrono::Utc::now();
    runtime.save_session(&session).await?;

    tracing::info!(session_id = %args.id, "session cancelled");
    println!("  Session {} cancelled.", args.id);

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{
        OutputFormat, SessionListArgs, SessionResumeArgs, SessionShowArgs, SessionStatusFilter,
    };
    use async_trait::async_trait;
    use std::path::Path;
    use xaft_runtime::RuntimeError;
    use xaft_runtime::dispatch::{RunRequest, RunResult};
    use xaft_runtime::session::{AgentSession, SessionId};

    struct EmptySessionRuntime;

    #[async_trait]
    impl RuntimeDispatch for EmptySessionRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session =
                AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![])
        }
        async fn load_session(&self, _: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
            Ok(None)
        }
        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    struct WithSessions;

    #[async_trait]
    impl RuntimeDispatch for WithSessions {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session =
                AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(vec![
                AgentSession::new(
                    "fix the bug",
                    std::env::current_dir().unwrap(),
                    "default".into(),
                    "claude".into(),
                ),
                AgentSession::new(
                    "add tests",
                    std::env::current_dir().unwrap(),
                    "default".into(),
                    "claude".into(),
                ),
            ])
        }
        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
    }

    /// A store-backed runtime for the cancel tests: one seeded session plus
    /// whatever `save_session` last wrote.
    #[derive(Clone, Default)]
    struct CancelRuntime {
        inner: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, AgentSession>>>,
    }

    impl CancelRuntime {
        fn new() -> Self {
            Self::default()
        }
        async fn seed(&self, id: &str) -> String {
            let mut session = AgentSession::new(
                "cancel me",
                std::env::current_dir().unwrap(),
                "default".into(),
                "claude".into(),
            );
            session.id = SessionId::from_string(id);
            self.inner
                .lock()
                .unwrap()
                .insert(id.to_string(), session.clone());
            session.id.as_str().to_string()
        }
        async fn stored(&self, id: &str) -> AgentSession {
            self.inner.lock().unwrap().get(id).cloned().unwrap()
        }
    }

    #[async_trait]
    impl RuntimeDispatch for CancelRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session =
                AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(self.inner.lock().unwrap().values().cloned().collect())
        }
        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
        async fn load_session(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
            Ok(self.inner.lock().unwrap().get(id.as_str()).cloned())
        }
        async fn save_session(&self, session: &AgentSession) -> Result<(), RuntimeError> {
            self.inner
                .lock()
                .unwrap()
                .insert(session.id.as_str().to_string(), session.clone());
            Ok(())
        }
    }

    /// A runtime whose only session lives under a workspace_root that is
    /// definitely not the cwd, reachable only through the unfiltered lookup.
    struct ElsewhereRuntime;

    #[async_trait]
    impl RuntimeDispatch for ElsewhereRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session =
                AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(&self, _: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
            // cwd-filtered listing: correctly finds nothing.
            Ok(vec![])
        }
        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
        async fn load_session(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
            if id.as_str() != "elsewhere-id" {
                return Ok(None);
            }
            let mut session = AgentSession::new(
                "recorded elsewhere",
                std::path::PathBuf::from("/definitely/not/the/cwd"),
                "default".into(),
                "claude".into(),
            );
            session.id = SessionId::from_string("elsewhere-id");
            Ok(Some(session))
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
        let code = handle_session_list(&args, Arc::new(EmptySessionRuntime))
            .await
            .unwrap();
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
        let code = handle_session_list(&args, Arc::new(WithSessions))
            .await
            .unwrap();
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
        let code = handle_session_list(&args, Arc::new(EmptySessionRuntime))
            .await
            .unwrap();
        assert!(code.is_success());
    }

    #[tokio::test]
    async fn session_show_not_found_error() {
        let args = SessionShowArgs {
            id: "nonexistent-id".into(),
            format: OutputFormat::Pretty,
        };
        let err = handle_session_show(&args, Arc::new(EmptySessionRuntime))
            .await
            .unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn session_cancel_without_force_errors() {
        let args = SessionCancelArgs {
            id: "some-id".into(),
            force: false,
        };
        let err = handle_session_cancel(&args, Arc::new(WithSessions))
            .await
            .unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn session_cancel_with_force_writes_cancelled_status() {
        let runtime = CancelRuntime::new();
        let id = runtime.seed("some-id").await;
        let args = SessionCancelArgs {
            id: "some-id".into(),
            force: true,
        };
        let code = handle_session_cancel(&args, Arc::new(runtime.clone()))
            .await
            .unwrap();
        assert!(code.is_success());
        // The store must actually have been written, not just reported success.
        let stored = runtime.stored(&id).await;
        assert!(matches!(stored.status, SessionStatus::Cancelled));
    }

    #[tokio::test]
    async fn session_cancel_reports_a_missing_session_instead_of_pretending() {
        let args = SessionCancelArgs {
            id: "not-there".into(),
            force: true,
        };
        let err = handle_session_cancel(&args, Arc::new(CancelRuntime::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, XaftError::Usage(_)));
    }

    #[tokio::test]
    async fn session_show_finds_a_session_from_another_working_dir() {
        // `show` used to scan `list_sessions(&cwd)` only, so a session recorded
        // under a different workspace_root was invisible while `inspect`, which
        // loads globally by ID, could still find it.
        let runtime = ElsewhereRuntime;
        let args = SessionShowArgs {
            id: "elsewhere-id".into(),
            format: OutputFormat::Pretty,
        };
        let code = handle_session_show(&args, Arc::new(runtime)).await.unwrap();
        assert!(code.is_success());
    }
}
