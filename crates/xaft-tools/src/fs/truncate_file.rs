//! `TruncateFileTool` — truncate a file to a given byte size.
//!
//! Parity row 23. Destructive by nature (it discards everything past `size`),
//! so unlike `write_file` and `patch_file` it sits behind the approval gate
//! exactly like `delete_file` does.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_u64, require_str, validate_path};

/// Truncate a file to a given byte size.
pub struct TruncateFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl TruncateFileTool {
    /// Create a new `TruncateFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "truncate_file";
}

impl std::fmt::Debug for TruncateFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TruncateFileTool").finish()
    }
}

#[async_trait]
impl Tool for TruncateFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Truncate a file to a given byte size. `size` defaults to 0, which empties the file. \
         Destructive: requires confirmation. Returns { ok, path, new_size }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "size": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Target size in bytes (0 empties the file)."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "truncate_file", skip(self, ctx), fields(path, size))]
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
        validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;
        let size = opt_u64(&input, "size").unwrap_or(0);

        tracing::Span::current().record("path", path);
        tracing::Span::current().record("size", size);

        let full = self.root.join(path);

        let current = match tokio::fs::metadata(&full).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult::error(
                    format!("file not found: {path}"),
                    &ctx.tool_use_id,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Ok(ToolResult::error("permission_denied", &ctx.tool_use_id));
            }
            Err(e) => {
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("stat failed: {e}"),
                });
            }
        };

        if current.is_dir() {
            return Ok(ToolResult::error(
                format!("'{path}' is a directory, not a file"),
                &ctx.tool_use_id,
            ));
        }

        // `set_len` only ever shrinks; asking to grow a file is a mistake
        // that would otherwise look like it worked.
        if size > current.len() {
            return Ok(ToolResult::error(
                format!(
                    "'{path}' is {} byte(s); cannot truncate to {size} (target is larger)",
                    current.len()
                ),
                &ctx.tool_use_id,
            ));
        }

        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&full)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("open failed: {e}"),
            })?;
        file.set_len(size)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("truncate failed: {e}"),
            })?;

        let new_size = file.metadata().await.map(|m| m.len()).unwrap_or(size);

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": true,
                "path": path,
                "new_size": new_size,
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

    fn tool(tmp: &TempDir) -> TruncateFileTool {
        TruncateFileTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn truncates_to_requested_size() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "0123456789")
            .await
            .unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt", "size": 4}),
                &ToolContext::new("tf1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["new_size"], 4);
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).await.unwrap(),
            "0123"
        );
    }

    #[tokio::test]
    async fn default_size_empties_the_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "content")
            .await
            .unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt"}),
                &ToolContext::new("tf2"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).await.unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn refuses_to_grow_a_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "ab").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt", "size": 99}),
                &ToolContext::new("tf3"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        // The refusal must not have modified anything.
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).await.unwrap(),
            "ab"
        );
    }

    #[tokio::test]
    async fn is_gated_behind_confirmation() {
        let tmp = TempDir::new().unwrap();
        assert!(
            tool(&tmp).requires_confirmation(),
            "truncate_file destroys data and must require confirmation"
        );
    }

    #[tokio::test]
    async fn missing_file_reports_not_found() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "gone.txt"}),
                &ToolContext::new("tf4"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let res = tool(&tmp)
            .call(
                serde_json::json!({"path": "../secret.txt"}),
                &ToolContext::new("tf5"),
            )
            .await;
        assert!(res.is_err() || res.unwrap().is_error);
    }
}
