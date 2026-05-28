//! `AgentError` — typed errors for the xaft-agents crate.

/// Errors produced by agent-building and runtime operations in `xaft-agents`.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Named agent not registered in the registry.
    #[error("agent '{name}' not registered in AgentRegistry")]
    NotRegistered {
        /// The unknown agent name.
        name: String,
    },

    /// Agent tool-set resolution failed.
    #[error("tool-set error for agent '{agent}': {reason}")]
    ToolSet {
        /// Agent name.
        agent: String,
        /// Human-readable reason.
        reason: String,
    },

    /// Agent configuration is invalid.
    #[error("invalid agent configuration for '{agent}': {reason}")]
    InvalidConfig {
        /// Agent name.
        agent: String,
        /// Human-readable reason.
        reason: String,
    },

    /// Handoff to a disallowed target was attempted.
    #[error("handoff from '{from}' to '{target}' is not permitted; allowed: {allowed:?}")]
    DisallowedHandoff {
        /// Source agent.
        from: String,
        /// Attempted target.
        target: String,
        /// Allowed targets.
        allowed: Vec<String>,
    },
}
