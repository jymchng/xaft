//! Tool-level error types for xaft-tools.

use agtrs_runtime::error::AgtrsError;

/// Errors from xaft tool operations.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// A required input field was missing or had wrong type.
    #[error("invalid input for tool '{tool}': {reason}")]
    InvalidInput {
        /// Tool name.
        tool: String,
        /// What was wrong.
        reason: String,
    },

    /// Workspace file operation failed.
    #[error("workspace error in '{tool}': {reason}")]
    Workspace {
        /// Tool name.
        tool: String,
        /// Underlying reason.
        reason: String,
    },

    /// Git operation failed.
    #[error("git error in '{tool}': {reason}")]
    Git {
        /// Tool name.
        tool: String,
        /// Underlying reason.
        reason: String,
    },

    /// Shell execution failed.
    #[error("shell error in '{tool}': {reason}")]
    Shell {
        /// Tool name.
        tool: String,
        /// Underlying reason.
        reason: String,
    },

    /// Cancellation was requested.
    #[error("tool '{tool}' was cancelled")]
    Cancelled {
        /// Tool name.
        tool: String,
    },

    /// Path traversal / security violation.
    #[error("path security violation in '{tool}': {reason}")]
    PathSecurity {
        /// Tool name.
        tool: String,
        /// Violation details.
        reason: String,
    },

    /// File not found.
    #[error("file not found in '{tool}': {path}")]
    FileNotFound {
        /// Tool name.
        tool: String,
        /// The path that was not found.
        path: String,
    },

    /// I/O error.
    #[error("I/O error in '{tool}': {reason}")]
    Io {
        /// Tool name.
        tool: String,
        /// Reason.
        reason: String,
    },
}

impl From<ToolError> for AgtrsError {
    fn from(e: ToolError) -> Self {
        AgtrsError::ToolCallFailed {
            tool_name: tool_name_from_error(&e).to_string(),
            reason: e.to_string(),
        }
    }
}

fn tool_name_from_error(e: &ToolError) -> &str {
    match e {
        ToolError::InvalidInput { tool, .. }
        | ToolError::Workspace { tool, .. }
        | ToolError::Git { tool, .. }
        | ToolError::Shell { tool, .. }
        | ToolError::Cancelled { tool }
        | ToolError::PathSecurity { tool, .. }
        | ToolError::FileNotFound { tool, .. }
        | ToolError::Io { tool, .. } => tool.as_str(),
    }
}

/// Validate that a path does not contain directory traversal.
pub fn validate_path(tool: &str, path: &str) -> Result<(), ToolError> {
    if path.contains("..") {
        return Err(ToolError::PathSecurity {
            tool: tool.to_string(),
            reason: format!("path '{path}' contains '..': directory traversal is not allowed"),
        });
    }
    if path.trim().is_empty() {
        return Err(ToolError::InvalidInput {
            tool: tool.to_string(),
            reason: "path must not be empty".to_string(),
        });
    }
    Ok(())
}

/// Extract a required string field from a JSON input.
pub fn require_str<'a>(tool: &str, input: &'a serde_json::Value, field: &str) -> Result<&'a str, ToolError> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput {
            tool: tool.to_string(),
            reason: format!("required field '{field}' is missing or not a string"),
        })
}

/// Extract an optional string field.
pub fn opt_str<'a>(input: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(|v| v.as_str())
}

/// Extract an optional u64 field.
pub fn opt_u64(input: &serde_json::Value, field: &str) -> Option<u64> {
    input.get(field).and_then(|v| v.as_u64())
}

/// Extract an optional bool field.
pub fn opt_bool(input: &serde_json::Value, field: &str) -> Option<bool> {
    input.get(field).and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_path_blocks_traversal() {
        assert!(validate_path("test", "../secret").is_err());
        assert!(validate_path("test", "src/../../etc").is_err());
    }

    #[test]
    fn validate_path_allows_normal() {
        assert!(validate_path("test", "src/main.rs").is_ok());
        assert!(validate_path("test", "README.md").is_ok());
    }

    #[test]
    fn validate_path_rejects_empty() {
        assert!(validate_path("test", "").is_err());
        assert!(validate_path("test", "  ").is_err());
    }

    #[test]
    fn require_str_extracts_field() {
        let input = serde_json::json!({"path": "src/lib.rs"});
        assert_eq!(require_str("test", &input, "path").unwrap(), "src/lib.rs");
    }

    #[test]
    fn require_str_fails_on_missing() {
        let input = serde_json::json!({});
        assert!(require_str("test", &input, "path").is_err());
    }

    #[test]
    fn tool_error_converts_to_agtrs_error() {
        let e = ToolError::InvalidInput {
            tool: "read_file".into(),
            reason: "bad path".into(),
        };
        let ae: AgtrsError = e.into();
        assert!(matches!(ae, AgtrsError::ToolCallFailed { tool_name, .. } if tool_name == "read_file"));
    }
}
