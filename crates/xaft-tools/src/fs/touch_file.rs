//! `TouchFileTool` — create a file if missing, or bump its mtime if it exists.
//!
//! Parity row 22. agenthicc's contract is `{ ok, path, created }`, and its one
//! non-obvious case is `create=False` against a missing path, which is an
//! *error* result carrying `created: false` rather than a crash.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, require_str, validate_path};

/// Create a file if it does not exist, or update its modification time.
pub struct TouchFileTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl TouchFileTool {
    /// Create a new `TouchFileTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "touch_file";
}

impl std::fmt::Debug for TouchFileTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TouchFileTool").finish()
    }
}

#[async_trait]
impl Tool for TouchFileTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a file if it does not exist, or update its modification time if it does. \
         Pass create: false to touch an existing file only, which errors when it is missing. \
         Returns { ok, path, created }."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative file path."
                },
                "create": {
                    "type": "boolean",
                    "default": true,
                    "description": "Create the file when missing. If false and the file is missing, this is an error."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "touch_file", skip(self, ctx), fields(path, create))]
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
        let create = opt_bool(&input, "create").unwrap_or(true);

        tracing::Span::current().record("path", path);
        tracing::Span::current().record("create", create);

        let full = self.root.join(path);
        let existed = tokio::fs::symlink_metadata(&full).await.is_ok();

        if !existed && !create {
            return Ok(ToolResult::error(
                format!("not found: {path}"),
                &ctx.tool_use_id,
            ));
        }

        if existed {
            // Open in append mode and close immediately: the documented way
            // to bump mtime without writing content.
            if let Err(e) = tokio::fs::OpenOptions::new().append(true).open(&full).await {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    return Ok(ToolResult::error("permission_denied", &ctx.tool_use_id));
                }
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("touch failed: {e}"),
                });
            }
        } else {
            if let Some(parent) = full.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: format!("create_dir_all: {e}"),
                    });
                }
            }
            if let Err(e) = tokio::fs::write(&full, "").await {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    return Ok(ToolResult::error("permission_denied", &ctx.tool_use_id));
                }
                return Err(AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("create failed: {e}"),
                });
            }
        }

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": true,
                "path": path,
                "created": !existed,
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

    fn tool(tmp: &TempDir) -> TouchFileTool {
        TouchFileTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn creates_missing_file_with_parents() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a/b/c.txt"}),
                &ToolContext::new("t1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "error: {}", out.content);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["created"], true);
        assert!(tmp.path().join("a/b/c.txt").exists());
    }

    #[tokio::test]
    async fn touching_existing_file_preserves_content() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "keep me")
            .await
            .unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt"}),
                &ToolContext::new("t2"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["created"], false);
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).await.unwrap(),
            "keep me",
            "touch must not truncate an existing file"
        );
    }

    #[tokio::test]
    async fn create_false_on_missing_path_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "gone.txt", "create": false}),
                &ToolContext::new("t3"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(!tmp.path().join("gone.txt").exists());
    }

    #[tokio::test]
    async fn create_false_on_existing_path_succeeds() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "x").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt", "create": false}),
                &ToolContext::new("t4"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let res = tool(&tmp)
            .call(
                serde_json::json!({"path": "../escape.txt"}),
                &ToolContext::new("t5"),
            )
            .await;
        assert!(res.is_err() || res.unwrap().is_error);
    }
}
