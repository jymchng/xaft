//! `ListFilesTool` — list files in the workspace matching an optional pattern.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::opt_str;

/// List files in the workspace, with optional prefix/glob filtering.
pub struct ListFilesTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl ListFilesTool {
    /// Create a new `ListFilesTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "list_files";
}

impl std::fmt::Debug for ListFilesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListFilesTool").finish()
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "List files in the workspace. Optionally filter by path prefix or suffix. \
         Returns sorted file paths, one per line."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prefix": {
                    "type": "string",
                    "description": "Only include files whose path starts with this prefix (e.g. \"src/\")."
                },
                "suffix": {
                    "type": "string",
                    "description": "Only include files whose path ends with this suffix (e.g. \".rs\")."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10000,
                    "default": 200,
                    "description": "Maximum number of file paths to return."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "list_files", skip(self, ctx))]
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

        let prefix = opt_str(&input, "prefix");
        let suffix = opt_str(&input, "suffix");
        let max = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;

        let all = self.workspace.list().await;

        let filtered: Vec<&str> = all
            .iter()
            .filter(|p| {
                let matches_prefix = prefix.map(|px| p.starts_with(px)).unwrap_or(true);
                let matches_suffix = suffix.map(|sx| p.ends_with(sx)).unwrap_or(true);
                matches_prefix && matches_suffix
            })
            .take(max)
            .map(|s| s.as_str())
            .collect();

        if filtered.is_empty() {
            return Ok(ToolResult::ok("(no files found)", &ctx.tool_use_id));
        }

        let mut output = format!("{} file(s):\n", filtered.len());
        for path in &filtered {
            output.push_str(path);
            output.push('\n');
        }

        if filtered.len() == max {
            output.push_str(&format!(
                "\n(truncated at {max} results — use prefix/suffix to narrow down)"
            ));
        }

        tracing::debug!(total = all.len(), shown = filtered.len(), "list_files");

        Ok(ToolResult::ok(
            output.trim_end().to_string(),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn make_store() -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(vec![
            ("src/main.rs".into(), "".into()),
            ("src/lib.rs".into(), "".into()),
            ("tests/test.rs".into(), "".into()),
            ("Cargo.toml".into(), "".into()),
        ])) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn lists_all_files() {
        let tool = ListFilesTool::new(make_store());
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("src/main.rs"));
        assert!(result.content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn filters_by_prefix() {
        let tool = ListFilesTool::new(make_store());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"prefix": "src/"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("src/main.rs"));
        assert!(result.content.contains("src/lib.rs"));
        assert!(!result.content.contains("Cargo.toml"));
        assert!(!result.content.contains("tests/"));
    }

    #[tokio::test]
    async fn filters_by_suffix() {
        let tool = ListFilesTool::new(make_store());
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(serde_json::json!({"suffix": ".toml"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("Cargo.toml"));
        assert!(!result.content.contains(".rs"));
    }

    #[tokio::test]
    async fn empty_workspace_returns_no_files() {
        let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
        let tool = ListFilesTool::new(store);
        let ctx = ToolContext::new("t4");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("no files"));
    }
}
