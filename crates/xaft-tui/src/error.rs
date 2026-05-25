//! Error types for `xaft-tui`.

use thiserror::Error;

/// Errors from the xaft TUI.
#[derive(Debug, Error)]
pub enum TuiError {
    /// Terminal I/O error (crossterm).
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    /// Ratatui drawing error.
    #[error("render error: {0}")]
    Render(String),

    /// Runtime error forwarded from xaft-runtime.
    #[error("runtime error: {0}")]
    Runtime(#[from] xaft_runtime::RuntimeError),

    /// Channel closed unexpectedly.
    #[error("event channel closed")]
    ChannelClosed,

    /// Approval gate error.
    #[error("approval error: {0}")]
    Approval(String),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for TuiError {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        TuiError::Render(e.to_string())
    }
}
