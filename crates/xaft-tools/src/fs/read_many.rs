//! `ReadManyTool` — read multiple workspace files in a single call.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::validate_path;

/// Read multiple files in one call.
///
/// # Input schema
///
/// ```json
/// {
///   "paths": ["src/a.rs", "src/b.rs"],
///   "max_bytes_per_file": 32768
/// }
/// ```
pub struct ReadManyTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl ReadManyTool {
    /// Create a new `ReadManyTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "read_many";
    const MAX_FILES: usize = 20;
    const DEFAULT_MAX_BYTES: u64 = 32768;
}

impl std::fmt::Debug for ReadManyTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadManyTool").finish()
    }
}

#[async_trait]
impl Tool for ReadManyTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Read multiple workspace files in a single call. \
         Returns a JSON array of { path, content, truncated, error } objects. \
         Maximum 20 files per call. Content is truncated at max_bytes_per_file."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": 20,
                    "description": "List of workspace-relative file paths to read (max 20)."
                },
                "max_bytes_per_file": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 32768,
                    "description": "Maximum bytes to read per file before truncating. Default: 32768."
                }
            },
            "required": ["paths"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "read_many", skip(self, ctx), fields(count))]
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

        let paths = match input.get("paths").and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: "required field 'paths' is missing or not an array".to_string(),
                });
            }
        };

        if paths.is_empty() {
            return Err(AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "'paths' must not be empty".to_string(),
            });
        }

        let max_bytes = input
            .get("max_bytes_per_file")
            .and_then(|v| v.as_u64())
            .unwrap_or(Self::DEFAULT_MAX_BYTES) as usize;

        let paths_to_read: Vec<&str> = paths
            .iter()
            .take(Self::MAX_FILES)
            .filter_map(|v| v.as_str())
            .collect();

        tracing::Span::current().record("count", paths_to_read.len());

        let mut results: Vec<serde_json::Value> = Vec::new();

        for path in &paths_to_read {
            if ctx.cancel_token.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", Self::TOOL_NAME),
                });
            }

            // Validate path
            if let Err(e) = validate_path(Self::TOOL_NAME, path) {
                results.push(serde_json::json!({
                    "path": path,
                    "content": null,
                    "truncated": false,
                    "error": e.to_string()
                }));
                continue;
            }

            match self.workspace.read(path).await {
                Ok(content) => {
                    let bytes = content.len();
                    let (truncated_content, truncated) = if bytes > max_bytes {
                        // Truncate at a UTF-8 boundary
                        let safe_end = floor_char_boundary(&content, max_bytes);
                        (content[..safe_end].to_string(), true)
                    } else {
                        (content, false)
                    };
                    results.push(serde_json::json!({
                        "path": path,
                        "content": truncated_content,
                        "truncated": truncated,
                        "error": null
                    }));
                }
                Err(e) => {
                    results.push(serde_json::json!({
                        "path": path,
                        "content": null,
                        "truncated": false,
                        "error": format!("File not found: '{path}'. {e}")
                    }));
                }
            }
        }

        let json = serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

/// Find the largest byte index <= `index` that is on a UTF-8 char boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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
    async fn reads_multiple_files() {
        let store = store_with(vec![("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")]);
        let tool = ReadManyTool::new(store);
        let ctx = ToolContext::new("rm1");
        let result = tool
            .call(serde_json::json!({"paths": ["a.rs", "b.rs"]}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["path"], "a.rs");
        assert_eq!(items[0]["error"], serde_json::Value::Null);
        assert!(items[0]["content"].as_str().unwrap().contains("fn a"));
    }

    #[tokio::test]
    async fn reports_error_for_missing_file() {
        let store = store_with(vec![("a.rs", "fn a() {}")]);
        let tool = ReadManyTool::new(store);
        let ctx = ToolContext::new("rm2");
        let result = tool
            .call(serde_json::json!({"paths": ["a.rs", "missing.rs"]}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(items[0]["error"], serde_json::Value::Null);
        assert!(items[1]["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn truncates_large_file() {
        let big_content = "x".repeat(1000);
        let store = store_with(vec![("big.rs", &big_content)]);
        let tool = ReadManyTool::new(store);
        let ctx = ToolContext::new("rm3");
        let result = tool
            .call(
                serde_json::json!({"paths": ["big.rs"], "max_bytes_per_file": 100}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(items[0]["truncated"], true);
        assert!(items[0]["content"].as_str().unwrap().len() <= 100);
    }

    #[tokio::test]
    async fn rejects_traversal_path() {
        let store = store_with(vec![]);
        let tool = ReadManyTool::new(store);
        let ctx = ToolContext::new("rm4");
        let result = tool
            .call(serde_json::json!({"paths": ["../secret.txt"]}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error); // returns error record per-file
        let items: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert!(items[0]["error"].as_str().is_some());
    }
}
