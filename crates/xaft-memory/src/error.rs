//! Error types for `xaft-memory`.

/// All errors from `xaft-memory` operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// The underlying agtrs-memory error.
    #[error("memory store error: {0}")]
    Store(String),

    /// Configuration error.
    #[error("memory config error: {0}")]
    Config(String),

    /// Invalid tool input.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Git context unavailable.
    #[error("git context error: {0}")]
    GitContext(String),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// The memory system is disabled.
    #[error("memory system is disabled")]
    Disabled,
}

impl From<agtrs_memory::MemoryError> for MemoryError {
    fn from(e: agtrs_memory::MemoryError) -> Self {
        MemoryError::Store(e.to_string())
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(e: serde_json::Error) -> Self {
        MemoryError::Serialization(e.to_string())
    }
}

impl From<MemoryError> for agtrs_runtime::error::AgtrsError {
    fn from(e: MemoryError) -> Self {
        agtrs_runtime::error::AgtrsError::MemoryError(e.to_string())
    }
}

/// Convenience alias.
pub type MemoryResult<T> = Result<T, MemoryError>;
