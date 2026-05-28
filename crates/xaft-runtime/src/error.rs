//! Runtime error types.

use crate::types::ExitCode;

/// Errors from xaft runtime operations.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Provider initialisation failed.
    #[error("provider error: {0}")]
    Provider(String),

    /// Workspace operation failed.
    #[error("workspace error: {0}")]
    Workspace(String),

    /// Git operation failed.
    #[error("git error: {0}")]
    Git(String),

    /// Agent execution failed.
    #[error("agent error: {0}")]
    Agent(String),

    /// Agent stream produced an error event.
    #[error("agent failed: {0}")]
    AgentFailed(String),

    /// Session not found.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// Budget exceeded.
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),

    /// Cancelled by user or timeout.
    #[error("cancelled: {0}")]
    Cancelled(String),

    /// Not implemented (stub).
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<xaft_agents::AgentError> for RuntimeError {
    fn from(e: xaft_agents::AgentError) -> Self {
        RuntimeError::Agent(e.to_string())
    }
}

impl RuntimeError {
    /// Return the appropriate exit code for this error.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Config(_) => ExitCode::CONFIG_ERROR,
            Self::BudgetExceeded(_) => ExitCode::BUDGET_EXCEEDED,
            Self::Cancelled(_) => ExitCode::CANCELLED,
            Self::NotImplemented(_) => ExitCode::TASK_FAILED,
            _ => ExitCode::TASK_FAILED,
        }
    }
}
