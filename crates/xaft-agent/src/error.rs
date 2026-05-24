//! Error types for xaft-agent.

use thiserror::Error;

/// Errors produced by `XaftAgent` and `PlanModeAgent`.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The underlying agtrs executor returned an error.
    #[error("agent execution failed: {0}")]
    Execution(#[from] agtrs_runtime::error::AgtrsError),

    /// Planning failed (plan mode only).
    #[error("planning failed: {0}")]
    Planning(String),

    /// Git commit failed during `on_finish`.
    #[error("git auto-commit failed: {0}")]
    GitCommit(String),

    /// Configuration is invalid.
    #[error("invalid agent config: {0}")]
    Config(String),
}

impl AgentError {
    /// True if the error was caused by cancellation.
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Execution(e) => e.is_cancelled(),
            _ => false,
        }
    }
}
