//! `PatchFileTool` — apply a unified diff patch to a workspace file.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Apply a unified diff patch to a workspace file.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/main.rs",
///   "patch": "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"hello\");\n+    println!(\"world\");\n }\n"
/// }
/// ```
pub struct PatchFileTool {
    workspace: Arc<dyn WorkspaceStore>,
    #[allow(dead_code)]
    root: std::path::PathBuf,
}

impl PatchFileTool {
    /// Create a new `PatchFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "patch_file";
}

impl std::fmt::Debug for PatchFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatchFileTool").finish()
    }
}

#[async_trait]
impl Tool for PatchFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Apply a unified diff patch to a workspace file. \
         The patch must be in standard unified diff format \
         (produced by diff -u or git diff). \
         Returns an error if any hunk fails to apply."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the file to patch (e.g. \"src/main.rs\")."
                },
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch text to apply."
                }
            },
            "required": ["path", "patch"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "patch_file", skip(self, ctx), fields(path))]
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
        let patch = require_str(Self::TOOL_NAME, &input, "patch").map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        // Read current file content
        let original = match self.workspace.read(path).await {
            Ok(c) => c,
            Err(_) => {
                return Ok(ToolResult::error(
                    format!("File not found: '{path}'"),
                    &ctx.tool_use_id,
                ));
            }
        };

        // Apply patch using diffy
        let patch_parsed = match diffy::Patch::from_str(patch) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult::error(
                    format!("Failed to parse patch: {e}"),
                    &ctx.tool_use_id,
                ));
            }
        };

        let patched = match diffy::apply(&original, &patch_parsed) {
            Ok(result) => result,
            Err(e) => {
                return Ok(ToolResult::error(
                    format!("Patch failed to apply: {e}"),
                    &ctx.tool_use_id,
                ));
            }
        };

        // Guard: reject if result would be empty
        if patched.trim().is_empty() && !original.trim().is_empty() {
            return Ok(ToolResult::error(
                "Patch would produce an empty file — aborting.".to_string(),
                &ctx.tool_use_id,
            ));
        }

        // Write patched content back
        self.workspace
            .write(path, &patched)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("write failed: {e}"),
            })?;

        tracing::info!(path, "patch_file applied");

        let result = serde_json::json!({ "applied": true, "path": path });
        Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn mem_store_with(path: &str, content: &str) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(vec![(
            path.to_string(),
            content.to_string(),
        )])) as Arc<dyn WorkspaceStore>
    }

    fn make_tool(store: Arc<dyn WorkspaceStore>) -> PatchFileTool {
        PatchFileTool::new(store, std::path::PathBuf::from("/tmp/workspace"))
    }

    #[tokio::test]
    async fn applies_valid_patch() {
        let original = "fn main() {\n    println!(\"hello\");\n}\n";
        let store = mem_store_with("src/main.rs", original);
        let tool = make_tool(store.clone());
        let ctx = ToolContext::new("pf1");

        // Build a real patch using diffy
        let new_content = "fn main() {\n    println!(\"world\");\n}\n";
        let patch = diffy::create_patch(original, new_content);
        let patch_str = patch.to_string();

        let result = tool
            .call(
                serde_json::json!({"path": "src/main.rs", "patch": patch_str}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let updated = store.read("src/main.rs").await.unwrap();
        assert!(updated.contains("world"));
        assert!(!updated.contains("hello"));
    }

    #[tokio::test]
    async fn returns_error_for_inapplicable_patch() {
        let store = mem_store_with("a.rs", "fn foo() {}\n");
        let tool = make_tool(store);
        let ctx = ToolContext::new("pf2");
        // A patch that refers to content not in the file
        let bad_patch = "--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n fn nonexistent() {}\n-    old line\n+    new line\n";
        let result = tool
            .call(
                serde_json::json!({"path": "a.rs", "patch": bad_patch}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            result.is_error,
            "expected error but got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn returns_error_for_missing_file() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = make_tool(store);
        let ctx = ToolContext::new("pf3");
        let result = tool
            .call(
                serde_json::json!({"path": "missing.rs", "patch": "--- a\n+++ b\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = make_tool(store);
        let ctx = ToolContext::new("pf4");
        let result = tool
            .call(
                serde_json::json!({"path": "../etc/passwd", "patch": "--- a\n+++ b\n"}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
