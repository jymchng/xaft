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
use agtrs_runtime::agent::Agent;
use agtrs_runtime::executor::AgentExecutor;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::transport::Message;
use xaft_agent::builder::AgentBuilder;
use xaft_agent::config::{AgentRole, CommitPolicy};
use xaft_config::XaftConfig;
use xaft_tools::FsWorkspaceStore;
use xaft_tools::registry::ToolRegistryBuilder;

use crate::dispatch::{RunRequest, RunResult, RuntimeDispatch};
use crate::error::RuntimeError;
use crate::event_loop::EventLoop;
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
    /// Creates a `FsSessionStore` under `config.core.data_dir`.
    pub async fn bootstrap(config: XaftConfig) -> Result<Self, RuntimeError> {
        let signals = Arc::new(SignalBus::new());
        let session_store: Arc<dyn SessionStore> =
            Arc::new(FsSessionStore::new(&config.core.data_dir).await?);

        Ok(Self {
            config,
            signals,
            session_store,
            provider_override: None,
        })
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

        // ── Tool registry ─────────────────────────────────────────────────────
        let tools = {
            let mut builder = ToolRegistryBuilder::new(working_dir).with_shell();
            if git_guard.is_some() {
                // Git tools provided via WorktreeGuard separately
                builder = builder.without_git();
            }
            builder
                .build_coder()
                .map_err(|e| RuntimeError::Workspace(e.to_string()))?
                .all()
        };

        // Add git tools from worktree guard if available
        let tools = if let Some(guard) = &git_guard {
            let git_tools = agtrs_git::GitToolSet::new(Arc::clone(guard));
            [tools, git_tools.all()].concat()
        } else {
            tools
        };

        // ── Build agent ───────────────────────────────────────────────────────
        let commit_policy = if git_guard.is_some() {
            CommitPolicy::OnSuccess
        } else {
            CommitPolicy::Never
        };

        let system_prompt = if preset.system_prompt.is_empty() {
            None
        } else {
            Some(preset.system_prompt.as_str())
        };

        let mut agent_builder = AgentBuilder::new("xaft")
            .role(AgentRole::Coder)
            .max_turns(preset.max_turns as usize)
            .temperature(preset.temperature)
            .commit_policy(commit_policy)
            .tools(tools)
            .signals(Arc::clone(&self.signals))
            .parallel_tools();

        if let Some(prompt) = system_prompt {
            agent_builder = agent_builder.system_prompt_extra(prompt);
        }
        if let Some(guard) = &git_guard {
            agent_builder = agent_builder.with_git_guard(Arc::clone(guard));
        }

        let agent: Arc<dyn Agent> = Arc::new(agent_builder.build());

        // ── Build context ─────────────────────────────────────────────────────
        let resolve_ctx = Arc::new(injectable_runtime::ResolveContext::from_store(Arc::new(
            injectable_runtime::EmptySingletonStore,
        )));

        let ctx = agtrs_runtime::agent::AgentContextBuilder::new(
            "xaft",
            agent.config().clone(),
            Arc::clone(&llm),
            resolve_ctx,
        )
        .with_signals(Arc::clone(&self.signals))
        .build();

        // ── Run the agent ─────────────────────────────────────────────────────
        let input = Message::user(&request.task);
        let stream = AgentExecutor::run_stream(Arc::clone(&agent), input, ctx);

        let event_loop = EventLoop {
            headless: request.headless,
            show_tool_executions: !request.headless,
            cancel: None, // TODO: wire Ctrl-C token
        };

        let (content, exit_code) = match event_loop.consume(Box::pin(stream), &mut session).await {
            Ok(result) => result,
            Err(RuntimeError::Cancelled(reason)) => {
                session.status = SessionStatus::Cancelled;
                self.session_store.save(&session).await?;
                // Restore worktree on cancellation
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
                // Restore worktree on failure
                if let Some(guard) = &git_guard {
                    if let Err(re) = guard.restore().await {
                        warn!(error = %re, "xaft: worktree restore failed after agent error");
                    }
                }
                return Err(e);
            }
        };

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

        // For resume, use a fresh runtime with the current config but same signals
        let resume_runtime = Self {
            config: request.config.clone(),
            signals: Arc::clone(&self.signals),
            session_store: Arc::clone(&self.session_store),
            provider_override: self.provider_override.clone(),
        };

        resume_runtime.run_task(request).await
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Try to open a git worktree for the working directory.
///
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
        transport.queue_text("Task complete!").await;
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
        transport.queue_text("Done.").await;
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
