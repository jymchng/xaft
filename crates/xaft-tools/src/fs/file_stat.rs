//! `FileStatTool` — return metadata about a file or directory.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};

/// Return filesystem metadata for a path without reading its content.
///
/// # Input schema
///
/// ```json
/// { "path": "src/main.rs" }
/// ```
pub struct FileStatTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FileStatTool {
    /// Create a new `FileStatTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "file_stat";
}

impl std::fmt::Debug for FileStatTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStatTool").finish()
    }
}

#[async_trait]
impl Tool for FileStatTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Return filesystem metadata for a workspace path. \
         Reports size, file/dir/symlink flags, existence, and last-modified timestamp. \
         Never returns file content."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path to stat (e.g. \"src/main.rs\" or \"src/\")."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "file_stat", skip(self, ctx), fields(path))]
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

        // We need a real path to stat. Try to resolve via FsWorkspaceStore.
        // For in-memory stores we can only check existence.
        let stat = self.stat_path(path).await;

        let json = serde_json::to_string_pretty(&stat).unwrap_or_else(|_| "{}".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

impl FileStatTool {
    async fn stat_path(&self, path: &str) -> serde_json::Value {
        // Best effort: use workspace.exists() for basic check, then try to get
        // real metadata if the workspace exposes a root.
        let exists = self.workspace.exists(path).await;

        // Try real filesystem metadata via type-erased approach
        // We rely on the root being encoded in the workspace's display string
        let root_display = self.workspace.root_display();
        if !root_display.is_empty() && root_display != "<in-memory>" {
            let full = std::path::Path::new(&root_display).join(path);
            return stat_real_path(path, &full).await;
        }

        // In-memory fallback
        serde_json::json!({
            "path": path,
            "exists": exists,
            "is_file": exists,
            "is_dir": false,
            "is_symlink": false,
            "size_bytes": null,
            "modified_secs": null
        })
    }
}

/// Stat a real filesystem path and return a JSON value.
async fn stat_real_path(rel: &str, full: &std::path::Path) -> serde_json::Value {
    match tokio::fs::symlink_metadata(full).await {
        Ok(meta) => {
            let modified_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());

            serde_json::json!({
                "path": rel,
                "exists": true,
                "is_file": meta.is_file(),
                "is_dir": meta.is_dir(),
                "is_symlink": meta.file_type().is_symlink(),
                "size_bytes": meta.len(),
                "modified_secs": modified_secs
            })
        }
        Err(_) => {
            serde_json::json!({
                "path": rel,
                "exists": false,
                "is_file": false,
                "is_dir": false,
                "is_symlink": false,
                "size_bytes": null,
                "modified_secs": null
            })
        }
    }
}

/// A `FileStatTool` that knows the workspace root for accurate metadata.
pub struct FileStatToolFs {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl FileStatToolFs {
    /// Create a `FileStatTool` backed by a real filesystem root.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "file_stat";
}

impl std::fmt::Debug for FileStatToolFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStatToolFs")
            .field("root", &self.root)
            .finish()
    }
}

#[async_trait]
impl Tool for FileStatToolFs {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Return filesystem metadata for a workspace path. \
         Reports size, file/dir/symlink flags, existence, and last-modified timestamp. \
         Never returns file content."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative path to stat (e.g. \"src/main.rs\" or \"src/\")."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "file_stat", skip(self, ctx), fields(path))]
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
        let stat = stat_real_path(path, &full).await;

        let json = serde_json::to_string_pretty(&stat).unwrap_or_else(|_| "{}".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    #[tokio::test]
    async fn stat_existing_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("hello.txt"), "hello world")
            .await
            .unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = FileStatToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("fs1");
        let result = tool
            .call(serde_json::json!({"path": "hello.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(stat["exists"], true);
        assert_eq!(stat["is_file"], true);
        assert_eq!(stat["is_dir"], false);
        assert!(stat["size_bytes"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn stat_missing_file_returns_exists_false() {
        let tmp = TempDir::new().unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = FileStatToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("fs2");
        let result = tool
            .call(serde_json::json!({"path": "missing.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(stat["exists"], false);
    }

    #[tokio::test]
    async fn stat_directory() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("subdir")).await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = FileStatToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("fs3");
        let result = tool
            .call(serde_json::json!({"path": "subdir"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(stat["exists"], true);
        assert_eq!(stat["is_dir"], true);
        assert_eq!(stat["is_file"], false);
    }

    #[tokio::test]
    async fn stat_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = FileStatToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("fs4");
        let result = tool
            .call(serde_json::json!({"path": "../etc/passwd"}), &ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
