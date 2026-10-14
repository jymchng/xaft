//! Handler for `xaft session` subcommands.

use std::sync::Arc;

use xaft_config::{CliOverrides, ConfigLoader};
use xaft_runtime::ExitCode;
use xaft_runtime::SessionBackend;
use xaft_runtime::dispatch::RuntimeDispatch;
use xaft_runtime::session::{AgentSession, SessionId, SessionStatus};

use crate::args::{
    OutputFormat, SessionCancelArgs, SessionControlArgs, SessionCreateArgs, SessionEventsArgs,
    SessionListArgs, SessionResumeArgs, SessionSendArgs, SessionShowArgs, SessionSubcommand,
};
use crate::error::XaftError;

/// Execute `xaft session <subcommand>`.
///
/// `sessions` is the injected [`SessionBackend`] — literally the
/// `xaft_session::SessionManager` whose `session_store()` the runtime was given
/// in `main.rs`. `list`/`show`/`resume`/`cancel` go through `runtime`;
/// `export`/`inspect` need message history, which `RuntimeDispatch` cannot
/// expose (see `xaft_runtime::session_backend`), so they go through this
/// handle. Both are the same SQLite file, by construction rather than by two
/// `core.data_dir` resolutions happening to agree.
///
/// It is `Option` because `main.rs` degrades to a filesystem `SessionStore` when
/// SQLite cannot be opened. That fallback keeps no message history, so
/// `export`/`inspect` report that plainly rather than silently opening a
/// different database to answer the question.
pub async fn handle_session(
    subcommand: &SessionSubcommand,
    runtime: Arc<dyn RuntimeDispatch>,
    sessions: Option<Arc<dyn SessionBackend>>,
) -> Result<ExitCode, XaftError> {
    match subcommand {
        SessionSubcommand::List(args) => handle_session_list(args, runtime).await,
        SessionSubcommand::Show(args) => handle_session_show(args, runtime).await,
        SessionSubcommand::Resume(args) => handle_session_resume(args, runtime).await,
        SessionSubcommand::Cancel(args) => handle_session_cancel(args, runtime).await,
        SessionSubcommand::Export(args) => {
            // `export --service` answers from the session service's own store,
            // so it must not require the `SessionBackend` the local-log family
            // needs. Checked here for that reason alone.
            if args.service {
                return crate::commands::sessions::export_service(args).await;
            }
            let backend = require_backend(sessions.as_deref())?;
            crate::commands::sessions::export(args, backend).await
        }
        SessionSubcommand::Create(args) => crate::commands::session_service::create(args).await,
        SessionSubcommand::Events(args) => crate::commands::session_service::events(args).await,
        SessionSubcommand::Send(args) => crate::commands::session_service::send(args).await,
        SessionSubcommand::Control(args) => crate::commands::session_service::control(args).await,
        SessionSubcommand::Inspect(args) => {
            let backend = require_backend(sessions.as_deref())?;
            crate::commands::sessions::inspect(args, backend).await
        }
    }
}

/// The store handle, or an explanation of why there isn't one.
///
/// Only reachable when `xaft` fell back to `FsSessionStore` at bootstrap. The
/// message names the cause, because "found no sessions" would be both wrong and
/// far more confusing to a user who demonstrably has some.
fn require_backend(
    sessions: Option<&dyn SessionBackend>,
) -> Result<&dyn SessionBackend, XaftError> {
    sessions.ok_or_else(|| {
        XaftError::Usage(
            "no durable conversation store is available: xaft fell back to a filesystem \
             session store, which keeps no message history, so this command cannot run"
                .into(),
        )
    })
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
        OutputFormat, SessionCancelArgs, SessionListArgs, SessionShowArgs, SessionSubcommand,
        SessionsExportArgs, SessionsInspectArgs,
    };
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Arc;
    use xaft_runtime::RuntimeError;
    use xaft_runtime::SessionBackend;
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
    async fn one_session_is_visible_to_every_subcommand_of_the_family() {
        // THE INVARIANT TEST for this module, and the end-to-end acceptance
        // check for the whole `xaft session` family.
        //
        // The family reads one store by two routes: `list`/`show`/`resume`/
        // `cancel` go through `RuntimeDispatch`, `export`/`inspect` go through
        // the injected `SessionBackend`. Before the `SessionBackend` existed
        // those were two independent `core.data_dir` resolutions, so nothing
        // stopped `xaft session list` and `xaft session inspect` from
        // disagreeing about which sessions exist.
        //
        // Both sides are built from ONE `SessionManager` here, exactly as
        // `main.rs` now does: `mgr.session_store()` becomes the runtime's
        // store and `mgr` itself becomes the backend. That is what makes the
        // assertion below meaningful rather than tautological — if either side
        // were repointed at a different manager or a different `data_dir`, one
        // of the five subcommands below would go red.
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join("data");
        let cwd = std::env::current_dir().expect("cwd");

        let manager = xaft_session::SessionManager::new(&data_dir)
            .await
            .expect("open store");

        // Seed through the writer the runtime uses, with real history, so
        // `export`/`inspect` have something history-shaped to report.
        let conversation: Vec<xaft_session::Message> = ["hi", "hello"]
            .into_iter()
            .map(xaft_session::Message::user)
            .collect();
        let seeded: Vec<String> = {
            let mut ids = Vec::new();
            for task in ["alpha", "beta", "gamma"] {
                let session = AgentSession::new(
                    task,
                    cwd.clone(),
                    "default".into(),
                    "claude-3-5-sonnet-20241022".into(),
                );
                ids.push(session.id.as_str().to_string());
                manager
                    .save_with_history(&session, &conversation)
                    .await
                    .expect("seed session");
            }
            ids
        };
        assert_eq!(seeded.len(), 3);

        // ── one store, two views ──
        let runtime = Arc::new(StoreBackedRuntime::from_manager(manager.clone()));
        let backend: Arc<dyn SessionBackend> = Arc::new(manager.clone());

        // 1. `list` (dispatch route, cwd-filtered — hence the real cwd above)
        let listed: Vec<String> = runtime
            .list_sessions(&cwd)
            .await
            .expect("list")
            .into_iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        assert_eq!(
            sorted(&listed),
            sorted(&seeded),
            "`xaft session list` must see exactly what the backend serves"
        );

        // 2..5. `show`, `export`, `inspect`, `cancel` all reach the same ids.
        let out = dir.path().join("exported.json");
        for id in &seeded {
            let show = handle_session_show(
                &SessionShowArgs {
                    id: id.clone(),
                    format: OutputFormat::Json,
                },
                runtime.clone(),
            )
            .await
            .expect("show runs");
            assert!(show.is_success(), "`session show` failed for {id}");

            let exported = crate::commands::sessions::export(
                &SessionsExportArgs {
                    service: false,
                    id: id.clone(),
                    output: Some(out.clone()),
                },
                backend.as_ref(),
            )
            .await
            .expect("export runs");
            assert!(exported.is_success(), "`session export` failed for {id}");

            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
            assert_eq!(
                doc["session"]["id"],
                id.as_str(),
                "`session export` wrote a document for a different session"
            );
            assert_eq!(
                doc["message_count"], 2,
                "history must survive the round trip"
            );

            let inspected = crate::commands::sessions::inspect(
                &SessionsInspectArgs {
                    id: id.clone(),
                    json: true,
                },
                backend.as_ref(),
            )
            .await
            .expect("inspect runs");
            assert!(
                inspected.is_success(),
                "`session inspect` reported not-found for seeded id {id}"
            );
        }

        // `cancel` last, because it mutates. It must be visible to the OTHER
        // route afterwards — that is the strongest form of the invariant: a
        // write through `RuntimeDispatch::save_session` has to be readable by
        // the `SessionBackend` that serves `inspect`.
        let victim = &seeded[0];
        let cancelled = handle_session_cancel(
            &SessionCancelArgs {
                id: victim.clone(),
                force: true,
            },
            runtime.clone(),
        )
        .await
        .expect("cancel runs");
        assert!(
            cancelled.is_success(),
            "`session cancel` failed for {victim}"
        );

        let after = backend
            .load_with_history(&SessionId::from_string(victim.clone()))
            .await
            .expect("backend read")
            .expect("cancelled session is still there");
        assert!(
            matches!(after.session.status, SessionStatus::Cancelled),
            "a write through the runtime's store was not visible to the backend; \
             the two handles are not the same store"
        );

        // Negative control, so a test that "passes" by finding everything is
        // rejected: an id that was never seeded must still be absent.
        let missing = crate::commands::sessions::inspect(
            &SessionsInspectArgs {
                id: "definitely-not-a-real-session-id".into(),
                json: true,
            },
            backend.as_ref(),
        )
        .await
        .expect("inspect handles a missing session, not an Err");
        assert!(
            !missing.is_success(),
            "inspect must not claim success for a session that was never stored"
        );
    }

    #[tokio::test]
    async fn export_and_inspect_refuse_rather_than_opening_a_second_store() {
        // The fallback arm: when SQLite could not be opened at bootstrap,
        // `main.rs` passes `None`. `export`/`inspect` must say WHY rather than
        // reporting "not found", which would be both wrong (the user does have
        // sessions) and the exact confusion the injected handle exists to
        // prevent.
        let runtime = Arc::new(EmptySessionRuntime);
        for args in [
            SessionSubcommand::Export(SessionsExportArgs {
                service: false,
                id: "anything".into(),
                output: None,
            }),
            SessionSubcommand::Inspect(SessionsInspectArgs {
                id: "anything".into(),
                json: true,
            }),
        ] {
            let err = handle_session(&args, runtime.clone(), None)
                .await
                .expect_err("a missing history store must be reported");
            let XaftError::Usage(message) = err else {
                panic!("expected a Usage error, got {err:?}");
            };
            assert!(
                message.contains("message history"),
                "the message must explain the cause, got: {message}"
            );
        }
    }

    fn sorted(ids: &[String]) -> Vec<String> {
        let mut v = ids.to_vec();
        v.sort();
        v
    }

    /// A runtime backed by a REAL SQLite store.
    ///
    /// Built from `SessionManager::session_store()` rather than a hash map so
    /// this exercises the actual store type the binary runs on. It is handed
    /// the SAME manager that becomes the `SessionBackend`, mirroring `main.rs`
    /// — the previous version resolved `core.data_dir` a second time through
    /// `ConfigLoader`, which is precisely the duplication being removed.
    struct StoreBackedRuntime {
        store: Arc<dyn xaft_runtime::session_store::SessionStore>,
    }

    impl StoreBackedRuntime {
        fn from_manager(manager: xaft_session::SessionManager) -> Self {
            Self {
                store: manager.session_store(),
            }
        }
    }

    #[async_trait]
    impl RuntimeDispatch for StoreBackedRuntime {
        async fn run(&self, req: RunRequest) -> Result<RunResult, RuntimeError> {
            let session =
                AgentSession::new(req.task, req.working_dir, "default".into(), "m".into());
            Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: "ok".into(),
            })
        }
        async fn list_sessions(
            &self,
            working_dir: &Path,
        ) -> Result<Vec<AgentSession>, RuntimeError> {
            Ok(self.store.list(Some(working_dir)).await?)
        }
        async fn resume_session(
            &self,
            _: &str,
            _: xaft_config::XaftConfig,
        ) -> Result<RunResult, RuntimeError> {
            Err(RuntimeError::NotImplemented("stub".into()))
        }
        async fn load_session(&self, id: &SessionId) -> Result<Option<AgentSession>, RuntimeError> {
            Ok(self.store.load(id).await?)
        }
        async fn save_session(&self, session: &AgentSession) -> Result<(), RuntimeError> {
            self.store.save(session).await
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
