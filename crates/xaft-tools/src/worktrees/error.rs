//! Errors raised by the parallel-orchestration subsystem.
//!
//! agenthicc raises a family of Python exceptions and the CLI catches
//! `(KeyError, OSError, RuntimeError, ValueError)`. One Rust enum covers the
//! same ground: every variant's `Display` is the message agenthicc's `str(exc)`
//! produces, so `Unable to integrate task: {exc}` reads identically.
//!
//! # One deliberate difference
//!
//! agenthicc's `ManifestNotFound` subclasses `KeyError`, and Python's
//! `str(KeyError("abc"))` is `"'abc'"` — the repr, quotes included. So
//! `agenthicc worktrees recover missing` prints
//! `Unable to recover orchestration: 'missing'` while the same failure through
//! any other exception type prints the bare message. That is an artifact of
//! `KeyError.__str__`, not a contract, so [`WorktreeError::NotFound`] displays
//! the bare id and xaft prints `Unable to recover orchestration: missing`.
//! `worktrees show` is unaffected: it formats `Orchestration not found: {id}`
//! itself and has no quotes in agenthicc either.

/// Everything the orchestration subsystem can fail with.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// No durable manifest exists for this orchestration. Displays as the id.
    #[error("{0}")]
    NotFound(String),

    /// A manifest or argument is malformed. This is the `ValueError` case.
    #[error("{0}")]
    Invalid(String),

    /// The filesystem refused. This is the `OSError` case.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// The path is not a usable Git repository. This is `RepositoryError`.
    #[error("not a Git repository: {0}")]
    Repository(String),

    /// An operation would risk losing uncommitted changes. This is
    /// `WorktreeDirtyError`.
    #[error("Git worktree is dirty: {0}")]
    Dirty(String),

    /// A Git invocation exited non-zero. This is `WorktreeError` itself.
    #[error("git {command} failed: {output}")]
    Git {
        /// The argument list, joined for display.
        command: String,
        /// Combined stdout and stderr, truncated by the caller.
        output: String,
    },

    /// The coordinator refuses the operation. This is `CoordinatorError`.
    #[error("{0}")]
    Coordinator(String),
}

impl WorktreeError {
    /// Whether this error names a specific missing orchestration.
    ///
    /// `worktrees show` uses this to print its own message rather than the
    /// generic "unable to" text, which is why it exists alongside the
    /// `Display` impl rather than inside it.
    pub fn not_found_id(&self) -> Option<&str> {
        match self {
            WorktreeError::NotFound(id) => Some(id),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for WorktreeError {
    fn from(e: serde_json::Error) -> Self {
        WorktreeError::Invalid(e.to_string())
    }
}

/// Truncate Git output the way agenthicc does, so a runaway `git` message
/// cannot flood a terminal or an error string.
pub(crate) fn truncate(output: &str, limit: usize) -> String {
    let trimmed = output.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    trimmed.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_displays_the_bare_id_not_a_python_repr() {
        let e = WorktreeError::NotFound("abc".into());
        assert_eq!(e.to_string(), "abc");
        assert_eq!(e.not_found_id(), Some("abc"));
        assert_eq!(WorktreeError::Invalid("x".into()).not_found_id(), None);
    }

    #[test]
    fn git_errors_name_the_command_and_its_output() {
        let e = WorktreeError::Git {
            command: "worktree add".into(),
            output: "boom".into(),
        };
        assert_eq!(e.to_string(), "git worktree add failed: boom");
    }

    #[test]
    fn truncate_keeps_short_output_and_cuts_long_output() {
        assert_eq!(truncate("  hello  ", 10), "hello");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("héllo wörld", 5), "héllo");
    }
}
