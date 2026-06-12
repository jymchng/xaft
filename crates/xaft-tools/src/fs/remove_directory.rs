//! `RemoveDirectoryTool` — remove a directory from the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, require_str, validate_path};

/// Remove a directory from the workspace. Always requires confirmation.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/old_module",
///   "recursive": false,
///   "confirm": true
/// }
/// ```
pub struct RemoveDirectoryTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl RemoveDirectoryTool {
    /// Create a new `RemoveDirectoryTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "remove_directory";
}

impl std::fmt::Debug for RemoveDirectoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoveDirectoryTool").finish()
    }
}

#[async_trait]
impl Tool for RemoveDirectoryTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Remove a directory from the workspace. \
         You MUST pass confirm: true. \
         Set recursive: true to remove non-empty directories. \
         Without recursive, the directory must be empty. \
         Cannot remove the workspace root."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative directory path to remove (e.g. \"src/old_module\")."
                },
                "recursive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Remove directory and all contents recursively. Default: false (empty dir only)."
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Must be true to confirm removal."
                }
            },
            "required": ["path", "confirm"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "remove_directory", skip(self, ctx), fields(path))]
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
        let recursive = opt_bool(&input, "recursive").unwrap_or(false);
        let confirm = opt_bool(&input, "confirm").unwrap_or(false);

        if !confirm {
            return Ok(ToolResult::error(
                "Pass confirm: true to remove the directory. This is a destructive operation."
                    .to_string(),
                &ctx.tool_use_id,
            ));
        }

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        let full_path = self.root.join(path);

        // Refuse to remove workspace root
        if full_path == self.root {
            return Ok(ToolResult::error(
                "Cannot remove the workspace root directory.".to_string(),
                &ctx.tool_use_id,
            ));
        }

        if !full_path.exists() {
            return Ok(ToolResult::error(
                format!("Directory not found: '{path}'"),
                &ctx.tool_use_id,
            ));
        }

        if !full_path.is_dir() {
            return Ok(ToolResult::error(
                format!("'{path}' is not a directory. Use delete_file to remove files."),
                &ctx.tool_use_id,
            ));
        }

        if recursive {
            tokio::fs::remove_dir_all(&full_path).await.map_err(|e| {
                AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("remove_dir_all failed: {e}"),
                }
            })?;
        } else {
            tokio::fs::remove_dir(&full_path)
                .await
                .map_err(|e| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!(
                        "remove_dir failed (directory may not be empty, use recursive: true): {e}"
                    ),
                })?;
        }

        tracing::info!(path, recursive, "remove_directory");

        let result = serde_json::json!({ "removed": true, "path": path });
        Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    async fn make_tool(tmp: &TempDir) -> RemoveDirectoryTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        RemoveDirectoryTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn removes_empty_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("empty_dir"))
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("rd1");
        let result = tool
            .call(
                serde_json::json!({"path": "empty_dir", "confirm": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(!tmp.path().join("empty_dir").exists());
    }

    #[tokio::test]
    async fn refuses_without_confirm() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("dir")).await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("rd2");
        let result = tool
            .call(serde_json::json!({"path": "dir", "confirm": false}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(tmp.path().join("dir").exists());
    }

    #[tokio::test]
    async fn removes_non_empty_with_recursive() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("dir")).await.unwrap();
        fs::write(tmp.path().join("dir/file.rs"), "x")
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("rd3");
        let result = tool
            .call(
                serde_json::json!({"path": "dir", "recursive": true, "confirm": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(!tmp.path().join("dir").exists());
    }

    #[tokio::test]
    async fn refuses_to_remove_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("rd4");
        let result = tool
            .call(serde_json::json!({"path": ".", "confirm": true}), &ctx)
            .await;
        // "." normalizes to root — should error
        // Either validate_path or the root guard catches it
        match result {
            Ok(tr) => assert!(tr.is_error),
            Err(_) => {} // also acceptable
        }
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("rd5");
        let result = tool
            .call(
                serde_json::json!({"path": "../other", "confirm": true}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
