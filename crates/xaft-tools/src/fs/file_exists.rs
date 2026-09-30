//! `FileExistsTool` \u2014 cheap existence probe for a workspace path.
//!
//! Parity row 11. agenthicc's `file_exists` is a *read* tool whose whole
//! answer is a boolean. xaft already had `file_stat`, which reports
//! `exists` as one field among several, and which is the wrong shape for the
//! common case: an agent asking "is there a config file here?" should not
//! have to reason about `size_bytes: null` meaning "no".

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Check whether a file or directory exists.
pub struct FileExistsTool {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl FileExistsTool {
    /// Create a new `FileExistsTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "file_exists";
}

impl std::fmt::Debug for FileExistsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileExistsTool").finish()
    }
}

#[async_trait]
impl Tool for FileExistsTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Check whether a file or directory exists. \
         Returns { ok, path, exists, is_dir }. Never fails for a missing path — \
         a missing path is a normal answer, not an error."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path to check."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "file_exists", skip(self, ctx), fields(path))]
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

        tracing::Span::current().record("path", path);

        let full = self.root.join(path);
        // `symlink_metadata` so a broken symlink still reports as present.
        let meta = tokio::fs::symlink_metadata(&full).await.ok();

        Ok(ToolResult::ok(
            serde_json::json!({
                "ok": true,
                "path": path,
                "exists": meta.is_some(),
                "is_dir": meta.as_ref().is_some_and(|m| m.is_dir()),
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

    fn tool(tmp: &TempDir) -> FileExistsTool {
        FileExistsTool::new(
            Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>,
            tmp.path(),
        )
    }

    #[tokio::test]
    async fn reports_existing_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "x").await.unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "a.txt"}),
                &ToolContext::new("fe1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["exists"], true);
        assert_eq!(v["is_dir"], false);
    }

    #[tokio::test]
    async fn missing_path_is_false_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let out = tool(&tmp)
            .call(
                serde_json::json!({"path": "nope.txt"}),
                &ToolContext::new("fe2"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "a missing path must be a normal answer");
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["exists"], false);
    }

    #[tokio::test]
    async fn detects_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("sub")).await.unwrap();
        let out = tool(&tmp)
            .call(serde_json::json!({"path": "sub"}), &ToolContext::new("fe3"))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["is_dir"], true);
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let res = tool(&tmp)
            .call(
                serde_json::json!({"path": "../escape"}),
                &ToolContext::new("fe4"),
            )
            .await;
        assert!(res.is_err() || res.unwrap().is_error);
    }
}
