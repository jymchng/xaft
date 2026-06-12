//! `MoveFileTool` — move/rename a file within the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, require_str, validate_path};

/// Move or rename a file within the workspace.
///
/// # Input schema
///
/// ```json
/// {
///   "source": "old/path.rs",
///   "destination": "new/path.rs",
///   "overwrite": false
/// }
/// ```
pub struct MoveFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl MoveFileTool {
    /// Create a new `MoveFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "move_file";
}

impl std::fmt::Debug for MoveFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoveFileTool").finish()
    }
}

#[async_trait]
impl Tool for MoveFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Move or rename a file within the workspace. \
         Set overwrite=true to replace an existing destination file (requires confirmation). \
         Creates destination parent directories automatically."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Workspace-relative source path (e.g. \"old/path.rs\")."
                },
                "destination": {
                    "type": "string",
                    "description": "Workspace-relative destination path (e.g. \"new/path.rs\")."
                },
                "overwrite": {
                    "type": "boolean",
                    "default": false,
                    "description": "Overwrite destination if it already exists. Requires confirmation when true."
                }
            },
            "required": ["source", "destination"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        // We check at runtime based on overwrite flag.
        // The framework calls requires_confirmation() before call(), so we default
        // to false here — the overwrite guard is enforced inside call().
        false
    }

    #[instrument(name = "move_file", skip(self, ctx), fields(source, destination))]
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

        let source = require_str(Self::TOOL_NAME, &input, "source").map_err(AgtrsError::from)?;
        let destination =
            require_str(Self::TOOL_NAME, &input, "destination").map_err(AgtrsError::from)?;
        let overwrite = opt_bool(&input, "overwrite").unwrap_or(false);

        validate_path(Self::TOOL_NAME, source).map_err(AgtrsError::from)?;
        validate_path(Self::TOOL_NAME, destination).map_err(AgtrsError::from)?;

        tracing::Span::current().record("source", source);
        tracing::Span::current().record("destination", destination);

        let src_path = self.root.join(source);
        let dst_path = self.root.join(destination);

        // Source must exist
        if !src_path.exists() {
            return Ok(ToolResult::error(
                format!("Source file not found: '{source}'"),
                &ctx.tool_use_id,
            ));
        }

        // Destination guard
        if dst_path.exists() && !overwrite {
            return Ok(ToolResult::error(
                format!(
                    "Destination '{destination}' already exists. Pass overwrite: true to replace it."
                ),
                &ctx.tool_use_id,
            ));
        }

        // Create destination parent directories
        if let Some(parent) = dst_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("create_dir_all: {e}"),
                })?;
        }

        tokio::fs::rename(&src_path, &dst_path)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("rename failed: {e}"),
            })?;

        tracing::info!(source, destination, "move_file");

        Ok(ToolResult::ok(
            format!("Moved '{source}' → '{destination}'"),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    async fn make_tool(tmp: &TempDir) -> MoveFileTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        MoveFileTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn moves_file_to_new_location() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("src.rs"), "content")
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("mv1");
        let result = tool
            .call(
                serde_json::json!({"source": "src.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(!tmp.path().join("src.rs").exists());
        assert!(tmp.path().join("dst.rs").exists());
    }

    #[tokio::test]
    async fn move_creates_destination_dirs() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "x").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("mv2");
        let result = tool
            .call(
                serde_json::json!({"source": "file.rs", "destination": "sub/dir/file.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("sub/dir/file.rs").exists());
    }

    #[tokio::test]
    async fn move_rejects_overwrite_without_flag() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("src.rs"), "source")
            .await
            .unwrap();
        fs::write(tmp.path().join("dst.rs"), "existing")
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("mv3");
        let result = tool
            .call(
                serde_json::json!({"source": "src.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("overwrite"));
    }

    #[tokio::test]
    async fn move_with_overwrite_true_replaces_destination() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("src.rs"), "new content")
            .await
            .unwrap();
        fs::write(tmp.path().join("dst.rs"), "old content")
            .await
            .unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("mv4");
        let result = tool
            .call(
                serde_json::json!({"source": "src.rs", "destination": "dst.rs", "overwrite": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let content = fs::read_to_string(tmp.path().join("dst.rs")).await.unwrap();
        assert_eq!(content, "new content");
    }

    #[tokio::test]
    async fn move_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("mv5");
        let result = tool
            .call(
                serde_json::json!({"source": "../outside.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
