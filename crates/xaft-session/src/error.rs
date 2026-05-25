//! Error types for `xaft-session`.

use thiserror::Error;

/// Errors from the xaft session layer.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Session not found in the store.
    #[error("session not found: {0}")]
    NotFound(String),

    /// Session exists but cannot be resumed (wrong status).
    #[error("session '{id}' is not resumable (status: {status})")]
    NotResumable { id: String, status: String },

    /// Underlying SQLite / store error.
    #[error("store error: {0}")]
    Store(#[from] agtrs_store::StoreError),

    /// Runtime layer error forwarded upward.
    #[error("runtime error: {0}")]
    Runtime(#[from] xaft_runtime::RuntimeError),

    /// Conversation store error from agtrs-runtime.
    #[error("conversation error: {0}")]
    Conversation(#[from] agtrs_runtime::error::AgtrsError),

    /// I/O error (directory creation, file ops).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization / deserialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// SQLx database error.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// Session integrity violation.
    #[error("integrity error: {0}")]
    Integrity(String),
}
