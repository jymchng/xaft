//! `ReadFileTool` — read file contents with optional line range.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_u64, require_str, validate_path};

/// Read a file from the workspace, with optional line-range support.
///
/// Returns the file content with 1-based line numbers prepended. Optionally
/// restricts output to a range of lines for large files.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/main.rs",
///   "start_line": 1,        // optional, 1-indexed inclusive
///   "end_line": 50,         // optional, 1-indexed inclusive
///   "with_line_numbers": true  // optional, default true
/// }
/// ```
pub struct ReadFileTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl ReadFileTool {
    /// Create a new `ReadFileTool` backed by `workspace`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "read_file";
    const MAX_LINES_DEFAULT: usize = 500;
}

impl std::fmt::Debug for ReadFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadFileTool").finish()
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Read the contents of a file in the workspace. Returns lines with 1-based line numbers. \
         Use start_line/end_line to read a specific range (useful for large files). \
         If the file is too large and no range is given, returns the first 500 lines."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path to read (e.g. \"src/main.rs\")."
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line to include (1-indexed, inclusive). Omit to start from the beginning."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to include (1-indexed, inclusive). Omit to read to the end (or cap)."
                },
                "with_line_numbers": {
                    "type": "boolean",
                    "default": true,
                    "description": "Prepend line numbers to each line. Default: true."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[instrument(name = "read_file", skip(self, ctx), fields(path))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let path = require_str(Self::TOOL_NAME, &input, "path").map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        // Cancellation check
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let content = match self.workspace.read(path).await {
            Ok(c) => c,
            Err(_) => {
                // List available files to help the model use the correct path
                let available = self.workspace.list().await;
                let hint = if available.is_empty() {
                    "Workspace is empty.".to_string()
                } else {
                    let sample: Vec<_> = available.iter().take(10).map(|s| s.as_str()).collect();
                    format!("Available files: {}", sample.join(", "))
                };
                return Ok(ToolResult::error(
                    format!(
                        "File not found: '{path}'. Use workspace-relative paths like 'main.go' not '/main.go'. {hint}"
                    ),
                    &ctx.tool_use_id,
                ));
            }
        };

        let with_line_numbers = input
            .get("with_line_numbers")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let start_line = opt_u64(&input, "start_line").map(|n| n.saturating_sub(1) as usize);
        let end_line = opt_u64(&input, "end_line").map(|n| n as usize);

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let from = start_line.unwrap_or(0);
        let to = end_line
            .map(|e| e.min(total))
            .unwrap_or_else(|| (from + Self::MAX_LINES_DEFAULT).min(total));
        let to = to.max(from); // ensure from <= to

        let selected = &lines[from.min(total)..to.min(total)];

        let mut output = String::new();

        // Header: show range info
        if total > Self::MAX_LINES_DEFAULT && start_line.is_none() && end_line.is_none() {
            output.push_str(&format!(
                "// File has {total} lines. Showing lines 1–{}. Use start_line/end_line to read more.\n\n",
                Self::MAX_LINES_DEFAULT
            ));
        } else if start_line.is_some() || end_line.is_some() {
            output.push_str(&format!(
                "// Showing lines {}-{} of {total}\n\n",
                from + 1,
                to
            ));
        }

        if with_line_numbers {
            let width = total.to_string().len().max(3);
            for (i, line) in selected.iter().enumerate() {
                output.push_str(&format!("{:>width$} | {}\n", from + i + 1, line));
            }
        } else {
            for line in selected {
                output.push_str(line);
                output.push('\n');
            }
        }

        tracing::debug!(path, lines = total, shown = selected.len(), "read_file");

        Ok(ToolResult::ok(
            output.trim_end().to_string(),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn make_store_with(path: &str, content: &str) -> Arc<dyn WorkspaceStore> {
        let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![(
            path.to_string(),
            content.to_string(),
        )]));
        store as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn reads_file_with_line_numbers() {
        let store = make_store_with("src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n");
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"path": "src/main.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("  1 | fn main()"));
        assert!(result.content.contains("  2 |     println!"));
    }

    #[tokio::test]
    async fn reads_file_without_line_numbers() {
        let store = make_store_with("a.rs", "hello\nworld\n");
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(
                serde_json::json!({"path": "a.rs", "with_line_numbers": false}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
        assert!(!result.content.contains("  1 |"));
    }

    #[tokio::test]
    async fn reads_line_range() {
        let content = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let store = make_store_with("big.rs", &content);
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(
                serde_json::json!({"path": "big.rs", "start_line": 3, "end_line": 5}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("line3"));
        assert!(result.content.contains("line5"));
        assert!(!result.content.contains("line1"));
        assert!(!result.content.contains("line6"));
    }

    #[tokio::test]
    async fn missing_file_returns_error_result() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(serde_json::json!({"path": "missing.rs"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t5");
        let result = tool
            .call(serde_json::json!({"path": "../etc/passwd"}), &ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = ReadFileTool::new(store);
        let ctx = ToolContext::new("t6");
        let result = tool.call(serde_json::json!({}), &ctx).await;
        // Should be Err (AgtrsError) or error ToolResult
        assert!(result.is_err());
    }
}
