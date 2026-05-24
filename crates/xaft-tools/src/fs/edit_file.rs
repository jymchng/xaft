//! `EditFileTool` — surgically replace a block of text using fuzzy anchor matching.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::{FileEditor, Occurrence, WorkspaceStore};

use crate::error::{opt_str, require_str, validate_path};

/// Replace a block of text in a file using fuzzy anchor matching.
///
/// Uses [`FileEditor::replace_block`] for whitespace-tolerant matching.
/// Changes are **immediately committed** to the workspace (no separate commit step
/// — xaft-tools handles the staging transparently via `commit()`).
///
/// Prefer this over `write_file` for targeted edits to existing files.
///
/// # Input schema
///
/// ```json
/// {
///   "path": "src/lib.rs",
///   "old_content": "fn old_name() {\n    ...\n}",
///   "new_content": "fn new_name() {\n    ...\n}",
///   "occurrence": "first"
/// }
/// ```
pub struct EditFileTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl EditFileTool {
    /// Create a new `EditFileTool` backed by `workspace`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "edit_file";
}

impl std::fmt::Debug for EditFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditFileTool").finish()
    }
}

#[async_trait]
impl Tool for EditFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Replace a specific block of text in a file using fuzzy anchor matching. \
         Provide the exact lines you want to replace as old_content, and the replacement \
         as new_content. Trailing whitespace differences are tolerated. \
         Use occurrence to control which match to replace when the block appears multiple times."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path of the file to edit."
                },
                "old_content": {
                    "type": "string",
                    "description": "The exact block of text to replace (fuzzy matched — trailing whitespace ignored)."
                },
                "new_content": {
                    "type": "string",
                    "description": "The replacement text."
                },
                "occurrence": {
                    "type": "string",
                    "enum": ["first", "all"],
                    "default": "first",
                    "description": "Which occurrence to replace: 'first' (default) or 'all'."
                }
            },
            "required": ["path", "old_content", "new_content"],
            "additionalProperties": false
        })
    }

    #[instrument(name = "edit_file", skip(self, ctx), fields(path))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let path = require_str(Self::TOOL_NAME, &input, "path").map_err(AgtrsError::from)?;
        let old_content =
            require_str(Self::TOOL_NAME, &input, "old_content").map_err(AgtrsError::from)?;
        let new_content =
            require_str(Self::TOOL_NAME, &input, "new_content").map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path", path);

        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            });
        }

        let occurrence = match opt_str(&input, "occurrence").unwrap_or("first") {
            "all" => Occurrence::All,
            _ => Occurrence::First,
        };

        // Create a FileEditor, apply the replacement, and commit atomically
        let editor = FileEditor::new(Arc::clone(&self.workspace));

        let receipt = match editor
            .replace_block(path, old_content, new_content, occurrence)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id));
            }
        };

        // Commit the staged change to the workspace
        editor
            .commit()
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("commit failed: {e}"),
            })?;

        let diff_summary = receipt.to_tool_result_content();

        tracing::info!(path, lines_delta = receipt.lines_delta, "edit_file");

        Ok(ToolResult::ok(diff_summary, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn make_store_with(path: &str, content: &str) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(vec![(
            path.to_string(),
            content.to_string(),
        )])) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn replaces_block_successfully() {
        let store = make_store_with("src/main.rs", "fn main() {\n    old_fn();\n}\n");
        let tool = EditFileTool::new(Arc::clone(&store));
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(
                serde_json::json!({
                    "path": "src/main.rs",
                    "old_content": "    old_fn();",
                    "new_content": "    new_fn();"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "error: {}", result.content);
        let content = store.read("src/main.rs").await.unwrap();
        assert!(content.contains("new_fn()"));
        assert!(!content.contains("old_fn()"));
    }

    #[tokio::test]
    async fn pattern_not_found_returns_error() {
        let store = make_store_with("a.rs", "fn foo() {}\n");
        let tool = EditFileTool::new(store);
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(
                serde_json::json!({
                    "path": "a.rs",
                    "old_content": "fn bar() {}",
                    "new_content": "fn baz() {}"
                }),
                &ctx,
            )
            .await;
        // Should return an error (either Err or error ToolResult)
        assert!(result.is_err() || result.unwrap().is_error);
    }

    #[tokio::test]
    async fn replaces_all_occurrences() {
        let store = make_store_with("b.rs", "x = 1;\nx = 1;\nx = 2;\n");
        let tool = EditFileTool::new(Arc::clone(&store));
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(
                serde_json::json!({
                    "path": "b.rs",
                    "old_content": "x = 1;",
                    "new_content": "x = 99;",
                    "occurrence": "all"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let content = store.read("b.rs").await.unwrap();
        assert_eq!(content.matches("x = 99;").count(), 2);
        assert_eq!(content.matches("x = 1;").count(), 0);
    }

    #[tokio::test]
    async fn result_contains_diff() {
        let store = make_store_with("c.rs", "fn old() {}\n");
        let tool = EditFileTool::new(store);
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(
                serde_json::json!({
                    "path": "c.rs",
                    "old_content": "fn old() {}",
                    "new_content": "fn new() {}"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        // EditReceipt renders diff info
        assert!(result.content.contains("OK:") || result.content.contains("c.rs"));
    }
}
