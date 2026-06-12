//! `DeleteFileTool` — delete a file from the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, require_str, validate_path};

/// Delete a file from the workspace. Always requires confirmation.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/old.rs",
///   "confirm": true
/// }
/// ```
pub struct DeleteFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl DeleteFileTool {
    /// Create a new `DeleteFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "delete_file";
}

impl std::fmt::Debug for DeleteFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeleteFileTool").finish()
    }
}

#[async_trait]
impl Tool for DeleteFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delete a file from the workspace. \
         You MUST pass confirm: true to confirm the deletion. \
         This operation always requires approval."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the file to delete (e.g. \"src/old.rs\")."
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Must be true to confirm deletion. Deletion is refused without this flag."
                }
            },
            "required": ["path", "confirm"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "delete_file", skip(self, ctx), fields(path))]
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
        let confirm = opt_bool(&input, "confirm").unwrap_or(false);

        if !confirm {
            return Ok(ToolResult::error(
                "Pass confirm: true to delete the file. This is a destructive operation."
                    .to_string(),
                &ctx.tool_use_id,
            ));
        }

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        let full_path = self.root.join(path);

        if !full_path.exists() {
            return Ok(ToolResult::error(
                format!("File not found: '{path}'"),
                &ctx.tool_use_id,
            ));
        }

        if !full_path.is_file() {
            return Ok(ToolResult::error(
                format!("'{path}' is not a regular file. Use remove_directory for directories."),
                &ctx.tool_use_id,
            ));
        }

        tokio::fs::remove_file(&full_path)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("remove_file failed: {e}"),
            })?;

        tracing::info!(path, "delete_file");

        let result = serde_json::json!({ "deleted": true, "path": path });
        Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    async fn make_tool(tmp: &TempDir) -> DeleteFileTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        DeleteFileTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn deletes_file_when_confirmed() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "x").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("df1");
        let result = tool
            .call(
                serde_json::json!({"path": "file.rs", "confirm": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(!tmp.path().join("file.rs").exists());
        let json: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(json["deleted"], true);
    }

    #[tokio::test]
    async fn refuses_without_confirm_flag() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "x").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("df2");
        let result = tool
            .call(
                serde_json::json!({"path": "file.rs", "confirm": false}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("confirm"));
        // File should still exist
        assert!(tmp.path().join("file.rs").exists());
    }

    #[tokio::test]
    async fn returns_error_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("df3");
        let result = tool
            .call(
                serde_json::json!({"path": "nope.rs", "confirm": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("df4");
        let result = tool
            .call(
                serde_json::json!({"path": "../secret.rs", "confirm": true}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
