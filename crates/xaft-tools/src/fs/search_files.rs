//! `SearchFilesTool` — search for files by filename in the workspace.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, opt_str, require_str};

/// Find files by filename (not content).
///
/// # Input schema
///
/// ```json
/// {
///   "name": "main.rs",
///   "path_prefix": "src/",
///   "case_sensitive": false
/// }
/// ```
pub struct SearchFilesTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl SearchFilesTool {
    /// Create a new `SearchFilesTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "search_files";
}

impl std::fmt::Debug for SearchFilesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchFilesTool").finish()
    }
}

#[async_trait]
impl Tool for SearchFilesTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Search for files by filename in the workspace. Matches against the filename component \
         (not file content — use grep for content search). \
         Returns a JSON array of matching workspace-relative paths."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Filename (or partial filename) to search for (e.g. \"main.rs\", \"config\")."
                },
                "path_prefix": {
                    "type": "string",
                    "description": "Only search within files under this path prefix (e.g. \"src/\"). Optional."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Whether the filename match is case-sensitive. Default: false."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "search_files", skip(self, ctx), fields(name))]
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

        let name = require_str(Self::TOOL_NAME, &input, "name").map_err(AgtrsError::from)?;
        let path_prefix = opt_str(&input, "path_prefix");
        let case_sensitive = opt_bool(&input, "case_sensitive").unwrap_or(false);

        tracing::Span::current().record("name", name);

        let search_name = if case_sensitive {
            name.to_string()
        } else {
            name.to_lowercase()
        };

        let all = self.workspace.list().await;

        let matches: Vec<String> = all
            .into_iter()
            .filter(|path| {
                // Optional prefix filter
                if let Some(px) = path_prefix {
                    if !path.starts_with(px) {
                        return false;
                    }
                }

                // Filename component match
                let filename = std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");

                let haystack = if case_sensitive {
                    filename.to_string()
                } else {
                    filename.to_lowercase()
                };

                haystack.contains(&search_name)
            })
            .collect();

        tracing::debug!(name, count = matches.len(), "search_files");

        let json = serde_json::to_string_pretty(&matches).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;

    fn store_with(files: Vec<&str>) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(
            files
                .into_iter()
                .map(|p| (p.to_string(), String::new()))
                .collect::<Vec<_>>(),
        )) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn finds_file_by_name() {
        let store = store_with(vec!["src/main.rs", "src/lib.rs", "tests/test.rs"]);
        let tool = SearchFilesTool::new(store);
        let ctx = ToolContext::new("sf1");
        let result = tool
            .call(serde_json::json!({"name": "main.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(!paths.iter().any(|p| p.contains("lib")));
    }

    #[tokio::test]
    async fn case_insensitive_match_by_default() {
        let store = store_with(vec!["src/Main.rs", "tests/test.rs"]);
        let tool = SearchFilesTool::new(store);
        let ctx = ToolContext::new("sf2");
        let result = tool
            .call(serde_json::json!({"name": "main.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(!paths.is_empty(), "should find Main.rs case-insensitively");
    }

    #[tokio::test]
    async fn prefix_filter_limits_search() {
        let store = store_with(vec!["src/config.rs", "tests/config_test.rs"]);
        let tool = SearchFilesTool::new(store);
        let ctx = ToolContext::new("sf3");
        let result = tool
            .call(
                serde_json::json!({"name": "config", "path_prefix": "src/"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(paths.contains(&"src/config.rs".to_string()));
        assert!(!paths.iter().any(|p| p.starts_with("tests")));
    }

    #[tokio::test]
    async fn partial_name_matches() {
        let store = store_with(vec!["src/configuration.rs", "src/other.rs"]);
        let tool = SearchFilesTool::new(store);
        let ctx = ToolContext::new("sf4");
        let result = tool
            .call(serde_json::json!({"name": "config"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(paths.contains(&"src/configuration.rs".to_string()));
    }
}
