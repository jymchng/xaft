//! `AppendToFileTool` — append content to a file in the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Append content to a file in the workspace. Creates the file if it does not exist.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "logs/app.log",
///   "content": "new line\n"
/// }
/// ```
pub struct AppendToFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl AppendToFileTool {
    /// Create a new `AppendToFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "append_to_file";
}

impl std::fmt::Debug for AppendToFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppendToFileTool").finish()
    }
}

#[async_trait]
impl Tool for AppendToFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Append content to a file in the workspace. \
         Creates the file and any parent directories if they do not exist. \
         Useful for incrementally building log files or adding entries."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path to append to (e.g. \"logs/app.log\")."
                },
                "content": {
                    "type": "string",
                    "description": "Content to append to the file."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "append_to_file", skip(self, ctx), fields(path))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let path = require_str(Self::TOOL_NAME, &input, "path").map_err(AgtrsError::from)?;
        let content = require_str(Self::TOOL_NAME, &input, "content").map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        let full_path = self.root.join(path);

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("create_dir_all: {e}"),
                })?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&full_path)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("open failed: {e}"),
            })?;

        let bytes = content.len();
        file.write_all(content.as_bytes())
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("write failed: {e}"),
            })?;
        file.flush().await.map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("flush failed: {e}"),
        })?;

        tracing::info!(path, bytes, "append_to_file");

        let result = serde_json::json!({ "path": path, "bytes_written": bytes });
        Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    async fn make_tool(tmp: &TempDir) -> AppendToFileTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        AppendToFileTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn creates_file_if_missing() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("af1");
        let result = tool
            .call(
                serde_json::json!({"path": "new.log", "content": "hello\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let content = fs::read_to_string(tmp.path().join("new.log"))
            .await
            .unwrap();
        assert_eq!(content, "hello\n");
    }

    #[tokio::test]
    async fn appends_to_existing_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("log.txt"), "line1\n")
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("af2");
        let result = tool
            .call(
                serde_json::json!({"path": "log.txt", "content": "line2\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let content = fs::read_to_string(tmp.path().join("log.txt"))
            .await
            .unwrap();
        assert_eq!(content, "line1\nline2\n");
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("af3");
        let result = tool
            .call(
                serde_json::json!({"path": "nested/dir/file.log", "content": "data"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("nested/dir/file.log").exists());
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("af4");
        let result = tool
            .call(
                serde_json::json!({"path": "../outside.log", "content": "data"}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
