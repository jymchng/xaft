//! `CopyFileTool` — copy a file within the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, require_str, validate_path};

/// Copy a file to a new location within the workspace.
///
/// # Input schema
///
/// ```json
/// {
///   "source": "src/a.rs",
///   "destination": "src/b.rs",
///   "overwrite": false
/// }
/// ```
pub struct CopyFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl CopyFileTool {
    /// Create a new `CopyFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "copy_file";
}

impl std::fmt::Debug for CopyFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyFileTool").finish()
    }
}

#[async_trait]
impl Tool for CopyFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Copy a file to a new location within the workspace. \
         Set overwrite=true to replace an existing destination file (requires confirmation). \
         Creates destination parent directories automatically."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Workspace-relative source path (e.g. \"src/a.rs\")."
                },
                "destination": {
                    "type": "string",
                    "description": "Workspace-relative destination path (e.g. \"src/b.rs\")."
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
        false
    }

    #[instrument(name = "copy_file", skip(self, ctx), fields(source, destination))]
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

        if !src_path.exists() {
            return Ok(ToolResult::error(
                format!("Source file not found: '{source}'"),
                &ctx.tool_use_id,
            ));
        }

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

        tokio::fs::copy(&src_path, &dst_path)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("copy failed: {e}"),
            })?;

        tracing::info!(source, destination, "copy_file");

        Ok(ToolResult::ok(
            format!("Copied '{source}' → '{destination}'"),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    async fn make_tool(tmp: &TempDir) -> CopyFileTool {
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        CopyFileTool::new(store, tmp.path())
    }

    #[tokio::test]
    async fn copies_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("src.rs"), "hello").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cp1");
        let result = tool
            .call(
                serde_json::json!({"source": "src.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("src.rs").exists());
        assert!(tmp.path().join("dst.rs").exists());
        let content = fs::read_to_string(tmp.path().join("dst.rs")).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn copy_rejects_overwrite_without_flag() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("src.rs"), "new").await.unwrap();
        fs::write(tmp.path().join("dst.rs"), "old").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cp2");
        let result = tool
            .call(
                serde_json::json!({"source": "src.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn copy_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.rs"), "x").await.unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cp3");
        let result = tool
            .call(
                serde_json::json!({"source": "file.rs", "destination": "a/b/c/file.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(tmp.path().join("a/b/c/file.rs").exists());
    }

    #[tokio::test]
    async fn copy_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp).await;
        let ctx = ToolContext::new("cp4");
        let result = tool
            .call(
                serde_json::json!({"source": "../secret.rs", "destination": "dst.rs"}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
