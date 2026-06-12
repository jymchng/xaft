//! Error types for xaft-skills.

use thiserror::Error;

/// Errors produced by the skills system.
#[derive(Debug, Error)]
pub enum SkillError {
    /// Skill file was too large to load.
    #[error("skill file too large: {path} ({size} bytes, max {max})")]
    FileTooLarge {
        /// Path to the oversized file.
        path: String,
        /// Actual file size in bytes.
        size: usize,
        /// Maximum allowed size in bytes.
        max: usize,
    },
    /// I/O error reading a skill file.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
