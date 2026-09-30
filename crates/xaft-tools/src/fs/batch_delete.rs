//! `BatchDeleteTool` — delete multiple files in a single call.
//!
//! Parity row 4. Destructive, so it sits behind the approval gate like
//! `delete_file`. A missing file counts as a *failure* of that entry rather
//! than silently succeeding, because "deleted 3 of 4" is the fact the caller
//! needs.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::validate_path;

/// Delete multiple files in a single call.
pub struct BatchDeleteTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl BatchDeleteTool {
    /// Create a new `BatchDeleteTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "batch_delete";
}

impl std::fmt::Debug for BatchDeleteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchDeleteTool").finish()
    }
}

#[async_trait]
impl Tool for BatchDeleteTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delete multiple files in a single call. `paths` is a list of workspace-relative paths. \
         Every path is validated before anything is deleted. \
         Destructive: requires confirmation. Returns { ok, results, total, succeeded, failed }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Workspace-relative file paths to delete."
                }
            },
            "required": ["paths"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "batch_delete", skip(self, ctx), fields(total))]
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
        let total = paths.len();
        tracing::Span::current().record("total", total);

        let mut entries: Vec<String> = Vec::with_capacity(total);
        for p in &paths {
            match p.as_str() {
                Some(s) => entries.push(s.to_string()),
                None => {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: "every entry in 'paths' must be a string".to_string(),
                    });
                }
            }
        }

        for path in &entries {
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
        for path in &entries {
            if ctx.cancel_token.is_cancelled() {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", Self::TOOL_NAME),
                });
            }
            let full = self.root.join(path);
            match tokio::fs::remove_file(&full).await {
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

    fn tool(tmp: &TempDir) -> BatchDeleteTool {
        BatchDeleteTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn deletes_every_file() {
        let tmp = TempDir::new().unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            fs::write(tmp.path().join(n), "x").await.unwrap();
        }
        let out = tool(&tmp)
            .call(
                serde_json::json!({"paths": ["a.txt", "b.txt", "c.txt"]}),
                &ToolContext::new("bd1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["succeeded"], 3);
        assert_eq!(v["failed"], 0);
        assert!(!tmp.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn partial_failure_is_reported_not_swallowed() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "x").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"paths": ["a.txt", "missing.txt"]}),
                &ToolContext::new("bd2"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["succeeded"], 1);
        assert_eq!(v["failed"], 1);
    }

    #[tokio::test]
    async fn is_gated_behind_confirmation() {
        let tmp = TempDir::new().unwrap();
        assert!(tool(&tmp).requires_confirmation());
    }

    #[tokio::test]
    async fn one_unsafe_path_refuses_the_whole_batch() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "x").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"paths": ["a.txt", "../escape.txt"]}),
                &ToolContext::new("bd3"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            tmp.path().join("a.txt").exists(),
            "a traversal entry must not allow deleting the other paths"
        );
    }
}
