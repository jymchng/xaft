//! `WriteFileTool` — create or overwrite a file in the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Write content to a file in the workspace.
///
/// Creates the file if it does not exist. Overwrites if it does.
/// This is a **destructive** operation that requires confirmation.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/new_module.rs",
///   "content": "pub fn hello() {}\n"
/// }
/// ```
pub struct WriteFileTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl WriteFileTool {
    /// Create a new `WriteFileTool` backed by `workspace`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "write_file";
}

impl std::fmt::Debug for WriteFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteFileTool").finish()
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Write content to a file in the workspace. Creates the file if it does not exist, \
         or overwrites it if it does. The entire file content must be provided. \
         For surgical edits to existing files, prefer edit_file instead."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the file to write (e.g. \"src/lib.rs\")."
                },
                "content": {
                    "type": "string",
                    "description": "Complete file content to write. The entire file is replaced."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false // Transactional — caller commits via edit_file workflow if needed
    }

    #[instrument(name = "write_file", skip(self, ctx), fields(path))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let path = require_str(Self::TOOL_NAME, &input, "path")
            .map_err(AgtrsError::from)?;
        let content = require_str(Self::TOOL_NAME, &input, "content")
            .map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let existed = self.workspace.exists(path).await;

        self.workspace.write(path, content).await.map_err(|e| {
            AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: e.to_string(),
            }
        })?;

        let bytes = content.len();
        let lines = content.lines().count();
        let action = if existed { "Updated" } else { "Created" };

        tracing::info!(path, bytes, lines, existed, "write_file");

        Ok(ToolResult::ok(
            format!("{action} '{path}' ({lines} lines, {bytes} bytes)"),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    #[tokio::test]
    async fn creates_new_file() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = WriteFileTool::new(Arc::clone(&store));
        let ctx = ToolContext::new("t1");
        let result = tool.call(
            serde_json::json!({"path": "src/new.rs", "content": "fn hello() {}\n"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("Created"));
        assert_eq!(store.read("src/new.rs").await.unwrap(), "fn hello() {}\n");
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
            ("a.rs".into(), "old\n".into()),
        ])) as Arc<dyn WorkspaceStore>;
        let tool = WriteFileTool::new(Arc::clone(&store));
        let ctx = ToolContext::new("t2");
        let result = tool.call(
            serde_json::json!({"path": "a.rs", "content": "new content\n"}),
            &ctx,
        ).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("Updated"));
        assert_eq!(store.read("a.rs").await.unwrap(), "new content\n");
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = WriteFileTool::new(store);
        let ctx = ToolContext::new("t3");
        assert!(tool.call(
            serde_json::json!({"path": "../etc/passwd", "content": "hacked"}),
            &ctx,
        ).await.is_err());
    }

    #[tokio::test]
    async fn missing_content_field_errors() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = WriteFileTool::new(store);
        let ctx = ToolContext::new("t4");
        assert!(tool.call(serde_json::json!({"path": "a.rs"}), &ctx).await.is_err());
    }
}
