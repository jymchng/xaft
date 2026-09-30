//! `BatchMoveTool` — move or rename multiple files in a single call.
//!
//! Parity row 5. Shares its implementation with `batch_copy`; only the verb
//! differs. Returns agenthicc's `{ ok, results, succeeded, failed }` envelope,
//! which has no `total` key.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::validate_path;
use crate::fs::batch_transfer::{ensure_parent, envelope, refusal, run_transfers, validate_pairs};

/// Move or rename multiple files in a single call.
pub struct BatchMoveTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl BatchMoveTool {
    /// Create a new `BatchMoveTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "batch_move";
}

impl std::fmt::Debug for BatchMoveTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchMoveTool").finish()
    }
}

#[async_trait]
impl Tool for BatchMoveTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Move or rename multiple files in a single call. `moves` is a list of \
         { source, destination } entries. Every path is validated before anything moves. \
         Returns { ok, results, succeeded, failed }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "moves": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "source": { "type": "string", "description": "Workspace-relative path." },
                            "destination": { "type": "string", "description": "Workspace-relative path." }
                        },
                        "required": ["source", "destination"],
                        "additionalProperties": false
                    },
                    "description": "Source/destination pairs to move."
                }
            },
            "required": ["moves"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "batch_move", skip(self, ctx), fields(total))]
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

        let raw = match input.get("moves").and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: "required field 'moves' is missing or not an array".to_string(),
                });
            }
        };
        let total = raw.len();
        tracing::Span::current().record("total", total);

        let entries = match validate_pairs(Self::TOOL_NAME, "moves", &raw) {
            Ok(e) => e,
            Err(msg) => return Ok(ToolResult::error(refusal(&msg, total), &ctx.tool_use_id)),
        };

        for (source, destination) in &entries {
            for p in [source, destination] {
                if let Err(e) = validate_path(Self::TOOL_NAME, p) {
                    return Ok(ToolResult::error(
                        refusal(&e.to_string(), total),
                        &ctx.tool_use_id,
                    ));
                }
            }
        }

        let (results, succeeded) = run_transfers(&self.root, &entries, |src, dst| {
            ensure_parent(dst)?;
            std::fs::rename(src, dst)
        });

        Ok(ToolResult::ok(
            envelope(results, succeeded, total).to_string(),
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

    fn tool(tmp: &TempDir) -> BatchMoveTool {
        BatchMoveTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn moves_every_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "A").await.unwrap();
        fs::write(tmp.path().join("b.txt"), "B").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"moves": [
                    {"source": "a.txt", "destination": "moved_a.txt"},
                    {"source": "b.txt", "destination": "moved_b.txt"}
                ]}),
                &ToolContext::new("bm1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["succeeded"], 2);
        assert_eq!(v["failed"], 0);
        assert!(tmp.path().join("moved_a.txt").exists());
        assert!(!tmp.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn creates_destination_parents() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "A").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"moves": [{"source": "a.txt", "destination": "deep/dir/a.txt"}]}),
                &ToolContext::new("bm2"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        assert!(tmp.path().join("deep/dir/a.txt").exists());
    }

    #[tokio::test]
    async fn partial_failure_is_reported() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "A").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"moves": [
                    {"source": "a.txt", "destination": "b.txt"},
                    {"source": "missing.txt", "destination": "c.txt"}
                ]}),
                &ToolContext::new("bm3"),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["succeeded"], 1);
        assert_eq!(v["failed"], 1);
    }

    #[tokio::test]
    async fn malformed_entry_refuses_the_batch() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "A").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"moves": [
                    {"source": "a.txt", "destination": "b.txt"},
                    {"source": "c.txt"}
                ]}),
                &ToolContext::new("bm4"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            !tmp.path().join("b.txt").exists(),
            "a malformed entry must not allow a partial move"
        );
    }

    #[tokio::test]
    async fn unsafe_path_refuses_the_batch() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "A").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"moves": [
                    {"source": "a.txt", "destination": "b.txt"},
                    {"source": "../escape.txt", "destination": "c.txt"}
                ]}),
                &ToolContext::new("bm5"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(!tmp.path().join("b.txt").exists());
    }
}
