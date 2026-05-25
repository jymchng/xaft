//! `XaftRuntime` — the real implementation of `RuntimeDispatch`.
//!
//! # Boot sequence
//!
//! ```text
//! XaftRuntime::bootstrap(config)
//!     ├── validate config
//!     ├── create SignalBus
//!     ├── create FsSessionStore (or InMemorySessionStore in tests)
//!     └── → XaftRuntime
//!
//! XaftRuntime::run(request)
//!     ├── resolve agent preset from config
//!     ├── build LLM provider via ProviderFactory
//!     ├── open FsWorkspaceStore for working_dir
//!     ├── optionally open GitRepo + WorktreeGuard
//!     ├── build ToolRegistry (list/read/edit/grep/write/bash/git)
//!     ├── build XaftAgent or PlanModeAgent
//!     ├── build AgentContext
//!     ├── run AgentExecutor::run_stream()
//!     └── consume events via EventLoop → RunResult
//! ```

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument, warn};

use agtrs_git::GitRepo;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::ConversationStore;
use agtrs_runtime::signals::{SignalBus, ToolCallComplete, ToolCallStarted};
use xaft_config::XaftConfig;
use xaft_tools::FsWorkspaceStore;
use xaft_tools::registry::ToolRegistryBuilder;

use crate::dispatch::{RunRequest, RunResult, RuntimeDispatch};
use crate::error::RuntimeError;
use crate::provider::ProviderFactory;
use crate::session::{AgentSession, SessionStatus};
use crate::session_store::{FsSessionStore, InMemorySessionStore, SessionStore};
use crate::types::ExitCode;

// ── XaftRuntime ───────────────────────────────────────────────────────────────

/// The production xaft runtime — composes all agtrs primitives into a
/// cohesive autonomous coding system.
pub struct XaftRuntime {
    config: XaftConfig,
    signals: Arc<SignalBus>,
    session_store: Arc<dyn SessionStore>,
    /// Optional pre-built provider override (for testing without real API keys).
    provider_override: Option<Arc<dyn LlmProvider>>,
    /// Durable conversation store for session resume (None → in-memory ephemeral).
    pub(crate) conversation_store: Option<Arc<dyn ConversationStore>>,
}

impl std::fmt::Debug for XaftRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaftRuntime").finish()
    }
}

impl XaftRuntime {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Bootstrap a production `XaftRuntime` from loaded config.
    ///
    /// Uses `FsSessionStore` by default. To enable SQLite session persistence,
    /// call [`XaftRuntime::with_stores`] after bootstrapping, passing stores
    /// created by `xaft_session::SessionManager`.
    pub async fn bootstrap(config: XaftConfig) -> Result<Self, RuntimeError> {
        let signals = Arc::new(SignalBus::new());
        attach_tool_call_logger(&signals).await;
        let session_store: Arc<dyn SessionStore> =
            Arc::new(FsSessionStore::new(&config.core.data_dir).await?);
        Ok(Self {
            config,
            signals,
            session_store,
            provider_override: None,
            conversation_store: None,
        })
    }

    /// Replace session + conversation stores (called from binary layer after
    /// bootstrapping `xaft_session::SessionManager`).
    ///
    /// This is the preferred way to inject SQLite persistence without creating
    /// a dependency cycle between `xaft-runtime` and `xaft-session`.
    pub fn with_stores(
        mut self,
        session_store: Arc<dyn SessionStore>,
        conversation_store: Arc<dyn ConversationStore>,
    ) -> Self {
        self.session_store = session_store;
        self.conversation_store = Some(conversation_store);
        self
    }

    /// Create for testing with an in-memory session store.
    ///
    /// Pass `Some(llm)` to bypass `ProviderFactory` (no API keys needed in
    /// tests).
    pub fn for_testing(config: XaftConfig, llm: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            config,
            signals: Arc::new(SignalBus::new()),
            session_store: Arc::new(InMemorySessionStore::new()),
            provider_override: llm,
            conversation_store: None,
        }
    }

    /// Access the shared signal bus (useful for attaching signal listeners).
    pub fn signals(&self) -> &Arc<SignalBus> {
        &self.signals
    }

    // ── Core run ──────────────────────────────────────────────────────────────

    /// Execute a task and return a result.
    ///
    /// This is the primary entry point for all agent runs.
    #[instrument(name = "xaft_run", skip_all, fields(task = %request.task))]
    async fn run_task(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        let working_dir = &request.working_dir;
        info!(task = %request.task, dir = %working_dir.display(), "xaft: starting run");

        // ── Resolve preset and model ──────────────────────────────────────────
        let preset_name = "default";
        let preset = self.config.agent.get(preset_name).ok_or_else(|| {
            RuntimeError::Config(format!("agent preset '{preset_name}' not found"))
        })?;

        // ── Create session ────────────────────────────────────────────────────
        let mut session = AgentSession::new(
            request.task.clone(),
            working_dir.clone(),
            preset_name.to_string(),
            preset.model.clone(),
        );
        self.session_store.save(&session).await?;

        // ── Dry-run shortcut (before expensive provider/worktree setup) ───────
        if request.dry_run {
            info!("xaft: dry-run mode, no execution");
            session.status = SessionStatus::Completed {
                summary: format!("dry-run: would execute '{}'", request.task),
            };
            self.session_store.save(&session).await?;
            return Ok(RunResult {
                exit_code: ExitCode::SUCCESS,
                session,
                summary: format!("dry-run completed for: {}", request.task),
            });
        }

        // ── Build LLM provider ────────────────────────────────────────────────
        let llm = if let Some(p) = &self.provider_override {
            Arc::clone(p)
        } else {
            ProviderFactory::build(&self.config, Some(preset_name))?
        };

        // ── Workspace store ───────────────────────────────────────────────────
        let _workspace = Arc::new(FsWorkspaceStore::new(working_dir));

        // ── Git worktree (optional) ───────────────────────────────────────────
        let git_guard = if !request.dry_run {
            try_open_git_worktree(working_dir, &session.id.to_string()).await
        } else {
            None
        };

        if let Some(g) = &git_guard {
            let branch = g.branch_name().to_string();
            session.git_branch = Some(branch.clone());
            info!(branch = %branch, "xaft: agent branch created");
            self.session_store.save(&session).await?;
        }

        // ── Tool registries (read-only + full write) ──────────────────────────
        let read_tools = {
            ToolRegistryBuilder::new(working_dir)
                .without_git()
                .build_reader()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all()
        };

        let write_tools = {
            let mut builder = ToolRegistryBuilder::new(working_dir).with_shell();
            if git_guard.is_some() {
                builder = builder.without_git();
            }
            let mut t = builder
                .build_coder()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all();
            if let Some(guard) = &git_guard {
                t.extend(agtrs_git::GitToolSet::new(Arc::clone(guard)).all());
            }
            t
        };

        // read_tools for QA (no writes); write_tools for coder + fixer
        // Include git read-only tools in read_tools for QA inspection
        let read_tools = {
            let mut t = read_tools;
            if let Some(guard) = &git_guard {
                t.extend(agtrs_git::GitToolSet::new(Arc::clone(guard)).read_only());
            }
            t
        };

        // ── Resolve context (for planner tool-call strategy) ──────────────────
        let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
            injectable_runtime::EmptySingletonStore,
        )));

        // ── Run orchestrated workflow: plan → coder → QA ↔ fixer ─────────────
        let (content, exit_code) = match crate::orchestrator::run_workflow(
            &request.task,
            Arc::clone(&llm),
            Arc::clone(&self.signals),
            resolve_ctx,
            read_tools,
            write_tools,
            &mut session,
            self.conversation_store.clone(),
        )
        .await
        {
            Ok(r) => r,
            Err(RuntimeError::Cancelled(reason)) => {
                session.status = SessionStatus::Cancelled;
                self.session_store.save(&session).await?;
                if let Some(guard) = &git_guard {
                    if let Err(e) = guard.restore().await {
                        warn!(error = %e, "xaft: worktree restore failed after cancellation");
                    }
                }
                return Err(RuntimeError::Cancelled(reason));
            }
            Err(e) => {
                session.status = SessionStatus::Failed {
                    error: e.to_string(),
                };
                self.session_store.save(&session).await?;
                if let Some(guard) = &git_guard {
                    if let Err(re) = guard.restore().await {
                        warn!(error = %re, "xaft: worktree restore failed after error");
                    }
                }
                return Err(e);
            }
        };

        // Auto-commit if git worktree was used
        if let Some(guard) = &git_guard {
            match guard.commit(agtrs_git::CommitOptions::default()).await {
                Ok(r) => info!(sha = %r.sha, "xaft: auto-committed changes"),
                Err(agtrs_git::GitError::NothingToCommit) => {}
                Err(e) => warn!(error = %e, "xaft: auto-commit failed"),
            }
        }

        self.session_store.save(&session).await?;

        let summary = if content.is_empty() {
            format!("task completed: {}", request.task)
        } else {
            content.chars().take(500).collect()
        };

        Ok(RunResult {
            exit_code,
            session,
            summary,
        })
    }
}

// ── RuntimeDispatch impl ──────────────────────────────────────────────────────

#[async_trait]
impl RuntimeDispatch for XaftRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        self.run_task(request).await
    }

    async fn list_sessions(&self, working_dir: &Path) -> Result<Vec<AgentSession>, RuntimeError> {
        self.session_store.list(Some(working_dir)).await
    }

    async fn resume_session(
        &self,
        session_id: &str,
        config: XaftConfig,
    ) -> Result<RunResult, RuntimeError> {
        let id = crate::session::SessionId::from_string(session_id);
        let session = self
            .session_store
            .load(&id)
            .await?
            .ok_or_else(|| RuntimeError::Config(format!("session '{session_id}' not found")))?;

        if !session.is_resumable() {
            return Err(RuntimeError::Config(format!(
                "session '{}' is not resumable (status: {})",
                session_id,
                session.status.label()
            )));
        }

        let request = RunRequest {
            task: session.task.clone(),
            config,
            working_dir: session.workspace_root.clone(),
            headless: false,
            dry_run: false,
            auto_approve: false,
            resume_session_id: Some(session_id.to_string()),
        };

        // For resume, propagate conversation_store so history is restored
        let resume_runtime = Self {
            config: request.config.clone(),
            signals: Arc::clone(&self.signals),
            session_store: Arc::clone(&self.session_store),
            provider_override: self.provider_override.clone(),
            conversation_store: self.conversation_store.clone(),
        };

        resume_runtime.run_task(request).await
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Try to open a git worktree for the working directory.
///
/// Register a `ToolCallStarted` + `ToolCallComplete` listener on `signals`.
async fn attach_tool_call_logger(signals: &agtrs_runtime::signals::SignalBus) {
    use agtrs_runtime::signals::{ToolCallComplete, ToolCallStarted};
    signals
        .on::<ToolCallStarted>(|ev| {
            tracing::info!(
                tool = %ev.tool_name,
                id = %ev.tool_use_id,
                input = %ev.input,
                "xaft: tool call input"
            );
        })
        .await;
    signals
        .on::<ToolCallComplete>(|ev| {
            if ev.success {
                tracing::info!(
                    tool = %ev.tool_name,
                    id = %ev.tool_use_id,
                    duration_ms = %ev.duration_ms,
                    "xaft: tool call done"
                );
            } else {
                tracing::warn!(
                    tool = %ev.tool_name,
                    id = %ev.tool_use_id,
                    error = %ev.error.as_deref().unwrap_or("(none)"),
                    "xaft: tool call failed"
                );
            }
        })
        .await;
}

/// Returns `None` if the directory is not a git repo (silently ignores the
/// error — xaft works fine without git).
async fn try_open_git_worktree(
    working_dir: &Path,
    run_id: &str,
) -> Option<Arc<agtrs_git::WorktreeGuard>> {
    let repo = match GitRepo::open(working_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(dir = %working_dir.display(), error = %e, "xaft: not a git repo, running without worktree");
            return None;
        }
    };

    match repo.begin_worktree(Some(run_id), None).await {
        Ok(guard) => Some(Arc::new(guard)),
        Err(e) => {
            warn!(error = %e, "xaft: could not create git worktree, running without branch isolation");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
    use tempfile::TempDir;
    use xaft_config::XaftConfig;

    fn mock_config() -> XaftConfig {
        XaftConfig::default()
    }

    fn make_request(task: &str, dir: &TempDir) -> RunRequest {
        RunRequest {
            task: task.to_string(),
            config: mock_config(),
            working_dir: dir.path().to_path_buf(),
            headless: true,
            dry_run: false,
            auto_approve: true,
            resume_session_id: None,
        }
    }

    #[tokio::test]
    async fn dry_run_returns_success_without_llm() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let mut request = make_request("list files", &tmp);
        request.dry_run = true;
        let result = runtime.run(request).await.unwrap();
        assert!(result.exit_code.is_success());
        assert!(result.summary.contains("dry-run"));
    }

    #[tokio::test]
    async fn run_with_mock_llm_succeeds() {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        // The orchestrated flow (planner × 3 strategies + coder + QA) consumes
        // many LLM calls. Queue enough garbage for planning to exhaust its
        // strategies, then the coder returns a response, then QA says APPROVED.
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport.queue_text("{\"files_changed\":[],\"description\":\"done\",\"tests_passed\":false,\"notes\":\"\"}").await;
        transport.queue_text("APPROVED").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

        let runtime = XaftRuntime::for_testing(mock_config(), Some(llm));
        let request = make_request("do something", &tmp);
        let result = runtime.run(request).await.unwrap();
        assert!(result.exit_code.is_success());
    }

    #[tokio::test]
    async fn list_sessions_returns_empty_for_new_runtime() {
        let tmp = TempDir::new().unwrap();
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn session_persisted_after_run() {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        for _ in 0..12 {
            transport.queue_text("not a plan").await;
        }
        transport.queue_text("{\"files_changed\":[],\"description\":\"done\",\"tests_passed\":false,\"notes\":\"\"}").await;
        transport.queue_text("APPROVED").await;
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

        let runtime = XaftRuntime::for_testing(mock_config(), Some(Arc::clone(&llm)));
        let request = make_request("my task", &tmp);
        runtime.run(request).await.unwrap();

        let sessions = runtime.list_sessions(tmp.path()).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].task, "my task");
    }

    #[tokio::test]
    async fn resume_nonexistent_session_returns_error() {
        let runtime = XaftRuntime::for_testing(mock_config(), None);
        let result = runtime
            .resume_session("nonexistent-id", mock_config())
            .await;
        assert!(matches!(result, Err(RuntimeError::Config(_))));
    }
}
