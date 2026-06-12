//! `CreateDirectoryTool` — create a directory (and all parents) in the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Create a directory (recursively) in the workspace.
///
/// # Input schema
///
/// ```json
/// { "path": "src/new_module" }
/// ```
pub struct CreateDirectoryTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl CreateDirectoryTool {
    /// Create a new `CreateDirectoryTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "create_directory";
}

impl std::fmt::Debug for CreateDirectoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateDirectoryTool").finish()
    }
}

#[async_trait]
impl Tool for CreateDirectoryTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a directory (and all intermediate parent directories) in the workspace. \
         Succeeds silently if the directory already exists."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative directory path to create (e.g. \"src/new_module\")."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "create_directory", skip(self, ctx), fields(path))]
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
        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        let full_path = self.root.join(path);

        tokio::fs::create_dir_all(&full_path)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("create_dir_all failed: {e}"),
            })?;

        tracing::info!(path, "create_directory");

        let result = serde_json::json!({ "created": true, "path": path });
        Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn make_tool(tmp: &TempDir) -> CreateDirectoryTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        CreateDirectoryTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn creates_directory() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cd1");
        let result = tool
            .call(serde_json::json!({"path": "new_dir"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("new_dir").is_dir());
        let json: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(json["created"], true);
    }

    #[tokio::test]
    async fn creates_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cd2");
        let result = tool
            .call(serde_json::json!({"path": "a/b/c/d"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("a/b/c/d").is_dir());
    }

    #[tokio::test]
    async fn creates_already_existing_dir_is_ok() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::create_dir_all(tmp.path().join("existing"))
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cd3");
        let result = tool
            .call(serde_json::json!({"path": "existing"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cd4");
        let result = tool
            .call(serde_json::json!({"path": "../outside"}), &ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
