//! `BatchWriteTool` — write multiple files in a single call.
//!
//! Parity row 7. agenthicc validates the *shape* of every entry before it
//! touches anything (`each entry must have "path" and "content" keys`, with
//! `succeeded: 0` and `failed: total`), and reports
//! `{ ok, results, total, succeeded, failed }`.
//!
//! Path safety is decided for the whole batch up front, matching agenthicc's
//! `_authorize_tool`: one bad path fails the call rather than silently
//! writing the other N-1 files.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, validate_path};

/// Write multiple files in a single call.
pub struct BatchWriteTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl BatchWriteTool {
    /// Create a new `BatchWriteTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "batch_write";
}

impl std::fmt::Debug for BatchWriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchWriteTool").finish()
    }
}

#[async_trait]
impl Tool for BatchWriteTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Write multiple files in a single call. `files` is a list of { path, content } entries. \
         Every path is validated before anything is written: if one is unsafe, nothing is written. \
         Returns { ok, results, total, succeeded, failed }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Workspace-relative path." },
                            "content": { "type": "string", "description": "Text to write." }
                        },
                        "required": ["path", "content"],
                        "additionalProperties": false
                    },
                    "description": "Files to write."
                },
                "create_parents": {
                    "type": "boolean",
                    "default": true,
                    "description": "Create missing parent directories for each file."
                }
            },
            "required": ["files"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "batch_write", skip(self, ctx), fields(total))]
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

        let files = match input.get("files").and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: "required field 'files' is missing or not an array".to_string(),
                });
            }
        };
        let create_parents = opt_bool(&input, "create_parents").unwrap_or(true);
        let total = files.len();

        tracing::Span::current().record("total", total);

        // Shape check first, exactly as agenthicc does, so a malformed batch
        // never half-writes.
        let mut entries: Vec<(String, String)> = Vec::with_capacity(total);
        for item in &files {
            let (Some(path), Some(content)) = (
                item.get("path").and_then(|v| v.as_str()),
                item.get("content").and_then(|v| v.as_str()),
            ) else {
                return Ok(ToolResult::error(
                    serde_json::json!({
                        "ok": false,
                        "error": "each entry must have \"path\" and \"content\" keys",
                        "results": [],
                        "total": total,
                        "succeeded": 0,
                        "failed": total,
                    })
                    .to_string(),
                    &ctx.tool_use_id,
                ));
            };
            entries.push((path.to_string(), content.to_string()));
        }

        // Authorize the whole batch up front: a partial write of the safe
        // subset is a worse outcome than an outright refusal.
        for (path, _) in &entries {
            if let Err(e) = validate_path(Self::TOOL_NAME, path) {
                return Ok(ToolResult::error(
                    serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                        "results": [],
                        "total": total,
                        "succeeded": 0,
                        "failed": total,
                    })
                    .to_string(),
                    &ctx.tool_use_id,
                ));
            }
        }

        let mut results = Vec::with_capacity(total);
        let mut succeeded = 0usize;
        for (path, content) in &entries {
            if ctx.cancel_token.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", Self::TOOL_NAME),
                });
            }
            let full = self.root.join(path);
            let entry = if create_parents {
                if let Some(parent) = full.parent() {
                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                        results.push(serde_json::json!({
                            "path": path, "ok": false,
                            "error": format!("create_dir_all failed: {e}")
                        }));
                        continue;
                    }
                }
                tokio::fs::write(&full, content).await
            } else {
                tokio::fs::write(&full, content).await
            };
            match entry {
                Ok(()) => {
                    succeeded += 1;
                    results.push(serde_json::json!({"path": path, "ok": true, "error": null}));
                }
                Err(e) => results.push(serde_json::json!({
                    "path": path, "ok": false, "error": e.to_string()
                })),
            }
        }

        let failed = total - succeeded;
        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": failed == 0,
                "results": results,
                "total": total,
                "succeeded": succeeded,
                "failed": failed,
            })
            .to_string(),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_store::FsWorkspaceStore;
    use tempfile::TempDir;
    use tokio::fs;

    fn tool(tmp: &TempDir) -> BatchWriteTool {
        BatchWriteTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn writes_every_file() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"files": [
                    {"path": "a.txt", "content": "A"},
                    {"path": "nested/b.txt", "content": "B"}
                ]}),
                &ToolContext::new("bw1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["total"], 2);
        assert_eq!(v["succeeded"], 2);
        assert_eq!(v["failed"], 0);
        assert_eq!(
            fs::read_to_string(tmp.path().join("nested/b.txt"))
                .await
                .unwrap(),
            "B"
        );
    }

    #[tokio::test]
    async fn malformed_entry_fails_the_whole_batch_and_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"files": [
                    {"path": "good.txt", "content": "ok"},
                    {"path": "bad.txt"}
                ]}),
                &ToolContext::new("bw2"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(
            v["error"],
            "each entry must have \"path\" and \"content\" keys"
        );
        assert_eq!(v["succeeded"], 0);
        assert_eq!(v["failed"], 2);
        assert!(
            !tmp.path().join("good.txt").exists(),
            "a malformed batch must not write the valid entries"
        );
    }

    #[tokio::test]
    async fn one_unsafe_path_refuses_the_whole_batch() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"files": [
                    {"path": "good.txt", "content": "ok"},
                    {"path": "../escape.txt", "content": "nope"}
                ]}),
                &ToolContext::new("bw3"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            !tmp.path().join("good.txt").exists(),
            "traversal in one entry must not allow a partial write"
        );
    }

    #[tokio::test]
    async fn create_parents_false_does_not_make_directories() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"files": [{"path": "x/y.txt", "content": "z"}],
                                   "create_parents": false}),
                &ToolContext::new("bw4"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["succeeded"], 0);
        assert_eq!(v["failed"], 1);
    }

    #[tokio::test]
    async fn empty_batch_is_ok() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(serde_json::json!({"files": []}), &ToolContext::new("bw5"))
            .await
            .unwrap();
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["total"], 0);
        assert_eq!(v["ok"], true);
    }
}
