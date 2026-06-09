//! Session types for xaft runtime.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique session identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Create a new random session ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create from an existing string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Return the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Session lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Session is currently active.
    Active,
    /// Session was paused by the user.
    Suspended,
    /// Session completed successfully.
    Completed {
        /// Summary of what was accomplished.
        summary: String,
    },
    /// Session failed.
    Failed {
        /// Error description.
        error: String,
    },
    /// Session was cancelled.
    Cancelled,
}

impl SessionStatus {
    /// Return a short label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// An xaft agent session.
///
/// Represents a single logical interaction from initial prompt to final result.
/// Sessions persist across crashes via `ConversationStore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    /// Unique session identifier.
    pub id: SessionId,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last updated.
    pub updated_at: DateTime<Utc>,
    /// Original task prompt.
    pub task: String,
    /// Working directory for this session.
    pub workspace_root: PathBuf,
    /// Git branch created for this session (if any).
    pub git_branch: Option<String>,
    /// Cumulative LLM cost in USD.
    pub total_cost_usd: f64,
    /// Total tokens consumed.
    pub total_tokens: u64,
    /// Number of agent turns executed.
    pub turn_count: u32,
    /// Current session status.
    pub status: SessionStatus,
    /// Agent preset name used.
    pub agent_preset: String,
    /// Model used.
    pub model: String,
}

impl AgentSession {
    /// Create a new session.
    pub fn new(
        task: impl Into<String>,
        workspace_root: PathBuf,
        agent_preset: String,
        model: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            created_at: now,
            updated_at: now,
            task: task.into(),
            workspace_root,
            git_branch: None,
            total_cost_usd: 0.0,
            total_tokens: 0,
            turn_count: 0,
            status: SessionStatus::Active,
            agent_preset,
            model,
        }
    }

    /// Return `true` if the session can be resumed.
    ///
    /// Matches the runtime's actual resume policy: Active, Suspended, and
    /// Completed sessions are all resumable (TUI multi-turn continues from a
    /// Completed session when the user sends a second task). Failed and
    /// Cancelled sessions cannot be resumed.
    pub fn is_resumable(&self) -> bool {
        matches!(
            self.status,
            SessionStatus::Active | SessionStatus::Suspended | SessionStatus::Completed { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make(status: SessionStatus) -> AgentSession {
        let mut s = AgentSession::new("t", PathBuf::from("."), "p".into(), "m".into());
        s.status = status;
        s
    }

    #[test]
    fn is_resumable_active() {
        assert!(make(SessionStatus::Active).is_resumable());
    }

    #[test]
    fn is_resumable_suspended() {
        assert!(make(SessionStatus::Suspended).is_resumable());
    }

    #[test]
    fn is_resumable_completed() {
        assert!(make(SessionStatus::Completed { summary: "ok".into() }).is_resumable());
    }

    #[test]
    fn not_resumable_failed() {
        assert!(!make(SessionStatus::Failed { error: "err".into() }).is_resumable());
    }

    #[test]
    fn not_resumable_cancelled() {
        assert!(!make(SessionStatus::Cancelled).is_resumable());
    }
}
