//! `DiffFilesTool` — produce a unified diff between two workspace files.

use std::sync::Arc;

use async_trait::async_trait;
use similar::TextDiff;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_u64, require_str, validate_path};

/// Produce a unified diff between two workspace files.
///
/// # Input schema
///
/// ```json
/// {
///   "path_a": "src/old.rs",
///   "path_b": "src/new.rs",
///   "context_lines": 3
/// }
/// ```
pub struct DiffFilesTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl DiffFilesTool {
    /// Create a new `DiffFilesTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "diff_files";
}

impl std::fmt::Debug for DiffFilesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffFilesTool").finish()
    }
}

#[async_trait]
impl Tool for DiffFilesTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Produce a unified diff between two workspace files. \
         Returns standard unified diff text showing additions and removals."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path_a": {
                    "type": "string",
                    "description": "Workspace-relative path of the first (old) file."
                },
                "path_b": {
                    "type": "string",
                    "description": "Workspace-relative path of the second (new) file."
                },
                "context_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 20,
                    "default": 3,
                    "description": "Number of context lines around each change. Default: 3."
                }
            },
            "required": ["path_a", "path_b"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "diff_files", skip(self, ctx), fields(path_a, path_b))]
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

        let path_a = require_str(Self::TOOL_NAME, &input, "path_a").map_err(AgtrsError::from)?;
        let path_b = require_str(Self::TOOL_NAME, &input, "path_b").map_err(AgtrsError::from)?;

        validate_path(Self::TOOL_NAME, path_a).map_err(AgtrsError::from)?;
        validate_path(Self::TOOL_NAME, path_b).map_err(AgtrsError::from)?;

        tracing::Span::current().record("path_a", path_a);
        tracing::Span::current().record("path_b", path_b);

        let context_lines = opt_u64(&input, "context_lines").unwrap_or(3) as usize;

        let content_a = match self.workspace.read(path_a).await {
            Ok(c) => c,
            Err(_) => {
                return Ok(ToolResult::error(
                    format!("File not found: '{path_a}'"),
                    &ctx.tool_use_id,
                ));
            }
        };

        let content_b = match self.workspace.read(path_b).await {
            Ok(c) => c,
            Err(_) => {
                return Ok(ToolResult::error(
                    format!("File not found: '{path_b}'"),
                    &ctx.tool_use_id,
                ));
            }
        };

        let diff = TextDiff::from_lines(&content_a, &content_b);
        let unified = diff
            .unified_diff()
            .context_radius(context_lines)
            .header(path_a, path_b)
            .to_string();

        if unified.is_empty() {
            return Ok(ToolResult::ok(
                format!("Files '{path_a}' and '{path_b}' are identical."),
                &ctx.tool_use_id,
            ));
        }

        tracing::debug!(path_a, path_b, "diff_files");

        Ok(ToolResult::ok(unified, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn store_with(files: Vec<(&str, &str)>) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(
            files
                .into_iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect::<Vec<_>>(),
        )) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn diff_produces_unified_output() {
        let store = store_with(vec![
            ("a.rs", "fn foo() {}\nfn bar() {}\n"),
            ("b.rs", "fn foo() {}\nfn baz() {}\n"),
        ]);
        let tool = DiffFilesTool::new(store);
        let ctx = ToolContext::new("d1");
        let result = tool
            .call(
                serde_json::json!({"path_a": "a.rs", "path_b": "b.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(result.content.contains("-fn bar()") || result.content.contains("bar"));
        assert!(result.content.contains("+fn baz()") || result.content.contains("baz"));
    }

    #[tokio::test]
    async fn diff_identical_files_says_so() {
        let store = store_with(vec![("a.rs", "fn foo() {}\n"), ("b.rs", "fn foo() {}\n")]);
        let tool = DiffFilesTool::new(store);
        let ctx = ToolContext::new("d2");
        let result = tool
            .call(
                serde_json::json!({"path_a": "a.rs", "path_b": "b.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("identical"));
    }

    #[tokio::test]
    async fn diff_missing_file_returns_error_result() {
        let store = store_with(vec![("a.rs", "fn foo() {}\n")]);
        let tool = DiffFilesTool::new(store);
        let ctx = ToolContext::new("d3");
        let result = tool
            .call(
                serde_json::json!({"path_a": "a.rs", "path_b": "missing.rs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn diff_rejects_traversal() {
        let store = store_with(vec![]);
        let tool = DiffFilesTool::new(store);
        let ctx = ToolContext::new("d4");
        let result = tool
            .call(
                serde_json::json!({"path_a": "../a.rs", "path_b": "b.rs"}),
                &ctx,
            )
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
