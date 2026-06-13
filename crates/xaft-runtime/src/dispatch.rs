//! Runtime dispatch trait and stub implementation.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use xaft_config::XaftConfig;

/// Tool-name filter from the active TUI mode (PRD 64).
///
/// Returns `true` if a tool with the given name is permitted in the current mode.
/// Defined here (in xaft-runtime) so both xaft-runtime and xaft-tui can use it
/// without a circular dependency.
pub type ModeToolFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

use crate::agent_registry::WorkflowConfig;
use crate::error::RuntimeError;
use crate::session::AgentSession;
use crate::types::{ExitCode, UserMessage};

/// Request to run a task.
#[derive(Clone)]
pub struct RunRequest {
    /// The natural language task description.
    pub task: String,
    /// Resolved configuration.
    pub config: XaftConfig,
    /// Working directory (defaults to cwd).
    pub working_dir: PathBuf,
    /// Whether to run in headless mode (no TUI).
    pub headless: bool,
    /// Whether to dry-run (plan only, no execution).
    pub dry_run: bool,
    /// Whether to auto-approve all confirmations.
    pub auto_approve: bool,
    /// Skip ALL tool-call approval gates — every tool executes without asking.
    /// In TUI mode the user must confirm a danger warning before this takes effect.
    pub dangerously_skip_permissions: bool,
    /// Session ID to resume, if any.
    pub resume_session_id: Option<String>,
    /// Which orchestration strategy to use.
    ///
    /// Defaults to [`WorkflowConfig::Standard`] — the classic plan→coder→QA↔fixer
    /// pipeline.  Set to [`WorkflowConfig::Dynamic`] to use the dynamic agent
    /// handoff path where any registered agent can hand off to any other.
    pub workflow: WorkflowConfig,
    /// Messages loaded from a prior session for context injection on resume.
    ///
    /// When `resume_session_id` is set, the runtime loads prior conversation
    /// history and places it here so the orchestrator can seed agent context.
    pub prior_messages: Vec<agtrs_runtime::transport::Message>,
    /// F3 @-mention: optional structured message from the TUI input bar.
    ///
    /// When `Some(UserMessage::MultiPart(parts))`, the orchestrator builds
    /// the first user turn as `Message::user_with_parts(parts)` so resolved
    /// `FileRef` blocks flow into the agent's context. When `Some(Text)` or
    /// `None`, the orchestrator falls back to `Message::user(task)`.
    pub user_message: Option<UserMessage>,
    /// System prompt prefix from the active TUI mode (PRD 64).
    ///
    /// When `Some`, prepended to each agent's base system prompt before any
    /// LLM call. Set by `ModeManager::apply_to_run_request()` at submit time.
    pub mode_system_patch: Option<String>,
    /// Tool name filter from the active TUI mode (PRD 64).
    ///
    /// When `Some(f)`, tools for which `f(tool_name)` returns `false` are
    /// excluded from the run's tool registry. `None` = allow all tools.
    pub mode_tool_filter: Option<ModeToolFilter>,
}

impl std::fmt::Debug for RunRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRequest")
            .field("task", &self.task)
            .field("working_dir", &self.working_dir)
            .field("headless", &self.headless)
            .field("dry_run", &self.dry_run)
            .field("auto_approve", &self.auto_approve)
            .field(
                "dangerously_skip_permissions",
                &self.dangerously_skip_permissions,
            )
            .field("resume_session_id", &self.resume_session_id)
            .field("mode_system_patch", &self.mode_system_patch)
            .field("has_mode_tool_filter", &self.mode_tool_filter.is_some())
            .finish()
    }
}

/// Result of a completed run.
#[derive(Debug)]
pub struct RunResult {
    /// Exit code.
    pub exit_code: ExitCode,
    /// Session that was created or resumed.
    pub session: AgentSession,
    /// Human-readable summary.
    pub summary: String,
}

/// Trait for runtime dispatch — abstracts the actual execution backend.
///
/// The real implementation will live in the full `XaftRuntime`. This trait
/// allows `xaft-cli` to remain decoupled from the implementation details.
#[async_trait]
pub trait RuntimeDispatch: Send + Sync {
    /// Execute a task.
    async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError>;

    /// List all sessions.
    async fn list_sessions(
        &self,
        working_dir: &std::path::Path,
    ) -> Result<Vec<AgentSession>, RuntimeError>;

    /// Resume a specific session.
    async fn resume_session(
        &self,
        session_id: &str,
        config: XaftConfig,
    ) -> Result<RunResult, RuntimeError>;
}

/// Stub runtime used before the full XaftRuntime is implemented.
///
/// Returns `RuntimeError::NotImplemented` for all operations.
pub struct StubRuntime;

#[async_trait]
impl RuntimeDispatch for StubRuntime {
    async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        // Stub: print what we would do
        tracing::info!(
            task = %request.task,
            working_dir = %request.working_dir.display(),
            headless = request.headless,
            dry_run = request.dry_run,
            "StubRuntime: would run task (full runtime not yet implemented)"
        );

        eprintln!();
        eprintln!("  🚧  xaft-runtime is not yet implemented.");
        eprintln!();
        eprintln!("  Task: {}", request.task);
        eprintln!(
            "  Model: {}",
            request
                .config
                .agent
                .get("default")
                .map(|a| a.model.as_str())
                .unwrap_or("(unknown)")
        );
        eprintln!(
            "  Provider: {}",
            request
                .config
                .agent
                .get("default")
                .map(|a| a.provider.as_str())
                .unwrap_or("(unknown)")
        );
        eprintln!();
        eprintln!("  The CLI parsed your arguments correctly. The runtime will be");
        eprintln!("  implemented in a future phase (xaft-orchestrator).");
        eprintln!();

        let session = AgentSession::new(
            request.task.clone(),
            request.working_dir,
            "default".to_string(),
            request
                .config
                .agent
                .get("default")
                .map(|a| a.model.clone())
                .unwrap_or_default(),
        );

        Ok(RunResult {
            exit_code: ExitCode::SUCCESS,
            session,
            summary: format!("stub: would run task '{}'", request.task),
        })
    }

    async fn list_sessions(
        &self,
        _working_dir: &std::path::Path,
    ) -> Result<Vec<AgentSession>, RuntimeError> {
        Ok(Vec::new())
    }

    async fn resume_session(
        &self,
        session_id: &str,
        _config: XaftConfig,
    ) -> Result<RunResult, RuntimeError> {
        Err(RuntimeError::NotImplemented(format!(
            "session resume not yet implemented (session_id: {session_id})"
        )))
    }
}
