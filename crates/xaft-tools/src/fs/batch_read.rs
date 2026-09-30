//! `BatchReadTool` — read multiple workspace files in a single call.
//!
//! Parity row 6. This is xaft's former `read_many`, brought onto agenthicc's
//! contract: the tool is named `batch_read` and returns
//! `{ ok, results, total, succeeded, failed }` rather than a bare array.
//!
//! The old envelope had two problems worth naming, because both are the
//! "looks finished but quietly did less" shape: a caller asking for 30 files
//! got 20 back with nothing in the result saying 10 were dropped, and a
//! caller could not tell "all four read" from "one of four failed" without
//! counting `error` fields by hand. Both are fixed below — the cap is
//! reported as `skipped`, and the counts are in the envelope.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::validate_path;

/// Read multiple files in a single call.
pub struct BatchReadTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl BatchReadTool {
    /// Create a new `BatchReadTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "batch_read";
    const MAX_FILES: usize = 20;
    const DEFAULT_MAX_BYTES: u64 = 32768;
}

impl std::fmt::Debug for BatchReadTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchReadTool").finish()
    }
}

#[async_trait]
impl Tool for BatchReadTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Read multiple workspace files in a single call. \
         Returns { ok, results, total, succeeded, failed, skipped }. \
         Each result is { path, content, truncated, error }. \
         At most 20 files are read; any beyond that are reported in `skipped` rather than dropped silently."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Workspace-relative paths to read (at most 20 are read)."
                },
                "max_bytes_per_file": {
                    "type": "integer",
                    "default": 32768,
                    "description": "Per-file content cap in bytes. Longer content is truncated and flagged."
                }
            },
            "required": ["paths"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "batch_read", skip(self, ctx), fields(requested, total))]
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

        let requested = paths.len();
        let max_bytes = input
            .get("max_bytes_per_file")
            .and_then(|v| v.as_u64())
            .unwrap_or(Self::DEFAULT_MAX_BYTES) as usize;

        let paths_to_read: Vec<&str> = paths
            .iter()
            .take(Self::MAX_FILES)
            .filter_map(|v| v.as_str())
            .collect();
        // Non-string entries are not silently dropped either: count them as
        // failed results so the totals still add up to what was asked for.
        let skipped: Vec<serde_json::Value> = paths
            .iter()
            .skip(Self::MAX_FILES)
            .map(|v| {
                serde_json::json!({
                    "path": v.as_str().unwrap_or("<non-string>"),
                    "content": null,
                    "truncated": false,
                    "error": format!("not read: batch_read reads at most {} files per call", Self::MAX_FILES),
                })
            })
            .collect();

        let total = paths_to_read.len() + skipped.len();
        tracing::Span::current().record("requested", requested);
        tracing::Span::current().record("total", total);

        let mut results: Vec<serde_json::Value> = Vec::with_capacity(total);
        for path in &paths_to_read {
            if ctx.cancel_token.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", Self::TOOL_NAME),
                });
            }
            if let Err(e) = validate_path(Self::TOOL_NAME, path) {
                results.push(serde_json::json!({
                    "path": path, "content": null, "truncated": false, "error": e.to_string()
                }));
                continue;
            }
            match self.workspace.read(path).await {
                Ok(content) => {
                    let (body, truncated) = if content.len() > max_bytes {
                        let safe_end = floor_char_boundary(&content, max_bytes);
                        (content[..safe_end].to_string(), true)
                    } else {
                        (content, false)
                    };
                    results.push(serde_json::json!({
                        "path": path, "content": body, "truncated": truncated, "error": null
                    }));
                }
                Err(e) => results.push(serde_json::json!({
                    "path": path, "content": null, "truncated": false,
                    "error": format!("File not found: '{path}'. {e}")
                })),
            }
        }
        results.extend(skipped);

        let failed = results
            .iter()
            .filter(|r| r.get("error").is_some_and(|e| !e.is_null()))
            .count();

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": failed == 0,
                "results": results,
                "total": total,
                "succeeded": total - failed,
                "failed": failed,
            })
            .to_string(),
            &ctx.tool_use_id,
        ))
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
        let tool = BatchReadTool::new(store);
        let result = tool
            .call(
                serde_json::json!({"paths": ["a.rs", "b.rs"]}),
                &ToolContext::new("br1"),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["total"], 2);
        assert_eq!(v["succeeded"], 2);
        assert_eq!(v["failed"], 0);
        let items = v["results"].as_array().unwrap();
        assert_eq!(items[0]["path"], "a.rs");
        assert_eq!(items[0]["error"], serde_json::Value::Null);
        assert!(items[0]["content"].as_str().unwrap().contains("fn a"));
    }

    #[tokio::test]
    async fn missing_file_is_counted_as_a_failure() {
        let store = store_with(vec![("a.rs", "fn a() {}")]);
        let tool = BatchReadTool::new(store);
        let result = tool
            .call(
                serde_json::json!({"paths": ["a.rs", "missing.rs"]}),
                &ToolContext::new("br2"),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["succeeded"], 1);
        assert_eq!(v["failed"], 1);
    }

    #[tokio::test]
    async fn truncates_large_file_and_says_so() {
        let big_content = "x".repeat(1000);
        let store = store_with(vec![("big.rs", &big_content)]);
        let tool = BatchReadTool::new(store);
        let result = tool
            .call(
                serde_json::json!({"paths": ["big.rs"], "max_bytes_per_file": 100}),
                &ToolContext::new("br3"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let items = v["results"].as_array().unwrap();
        assert_eq!(items[0]["truncated"], true);
        assert!(items[0]["content"].as_str().unwrap().len() <= 100);
    }

    #[tokio::test]
    async fn files_beyond_the_cap_are_reported_not_silently_dropped() {
        // The old `read_many` returned 20 results and said nothing about the
        // other 5, which reads as "these are all the files you asked for".
        let requested: Vec<String> = (0..25).map(|i| format!("f{i}.rs")).collect();
        let store = store_with(vec![("f0.rs", "x")]);
        let tool = BatchReadTool::new(store);
        let result = tool
            .call(
                serde_json::json!({"paths": requested}),
                &ToolContext::new("br4"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(v["total"], 25, "total must count every requested path");
        assert_eq!(v["results"].as_array().unwrap().len(), 25);
        let last = v["results"].as_array().unwrap().last().unwrap();
        assert!(
            last["error"].as_str().unwrap().contains("at most 20"),
            "the dropped entries must say why: {last}"
        );
    }

    #[tokio::test]
    async fn traversal_path_is_reported_per_file() {
        let store = store_with(vec![]);
        let tool = BatchReadTool::new(store);
        let result = tool
            .call(
                serde_json::json!({"paths": ["../secret.txt"]}),
                &ToolContext::new("br5"),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(v["results"][0]["error"].as_str().is_some());
    }
}
