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
    use agtrs_workspace::InMemoryWorkspaceStore;
    use tempfile::TempDir;
    use tokio::fs;

    fn mem_store(files: Vec<(&str, &str)>) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(
            files
                .into_iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect::<Vec<_>>(),
        )) as Arc<dyn WorkspaceStore>
    }

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

    // ── FileStatTool (the root-derived variant) ─────────────────────────────────────
    //
    // `FileStatToolFs` takes the root explicitly; `FileStatTool` has to recover it
    // from `WorkspaceStore::root_display()`. That type-dispatch is the only
    // difference between the two, so these tests pin both branches of it — plus
    // the fact that the two variants advertise an identical surface to the agent,
    // since only one of them is what `registry.rs` actually registers.

    /// A store whose `root_display()` is `<in-memory>` (the hardcoded value on
    /// `InMemoryWorkspaceStore`) takes the in-memory fallback: existence comes
    /// from the store and every metadata field is null.
    #[tokio::test]
    async fn in_memory_store_reports_existence_without_metadata() {
        let store = mem_store(vec![("present.rs", "twelve bytes")]);
        assert_eq!(store.root_display(), "<in-memory>");
        let tool = FileStatTool::new(store);
        let ctx = ToolContext::new("fs5");

        let result = tool
            .call(serde_json::json!({"path": "present.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let stat: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(stat["path"], "present.rs");
        assert_eq!(stat["exists"], true);
        // The in-memory branch cannot tell a file from a directory, so it
        // reports `is_file == exists` and leaves the rest at their defaults.
        assert_eq!(stat["is_file"], true);
        assert_eq!(stat["is_dir"], false);
        assert_eq!(stat["is_symlink"], false);
        assert_eq!(stat["size_bytes"], serde_json::Value::Null);
        assert_eq!(stat["modified_secs"], serde_json::Value::Null);

        let missing = tool
            .call(serde_json::json!({"path": "absent.rs"}), &ctx)
            .await
            .unwrap();
        let stat: serde_json::Value = serde_json::from_str(&missing.content).unwrap();
        assert_eq!(stat["exists"], false);
        assert_eq!(stat["is_file"], false);
    }

    /// A store with a real root takes the other branch: `root_display()` is
    /// joined with the relative path and `stat_real_path` supplies the answer.
    /// Both variants must agree on a file that really exists on disk, because
    /// `registry.rs` registers `FileStatToolFs` while `FileStatTool` is a
    /// supported public type a caller may construct directly.
    #[tokio::test]
    async fn fs_backed_store_agrees_with_the_explicit_root_variant() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("shared.txt"), "hello world")
            .await
            .unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let derived = FileStatTool::new(Arc::clone(&store));
        let explicit = FileStatToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("fs6");

        let a = derived
            .call(serde_json::json!({"path": "shared.txt"}), &ctx)
            .await
            .unwrap();
        let b = explicit
            .call(serde_json::json!({"path": "shared.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!a.is_error && !b.is_error);
        let (a, b): (serde_json::Value, serde_json::Value) = (
            serde_json::from_str(&a.content).unwrap(),
            serde_json::from_str(&b.content).unwrap(),
        );
        assert_eq!(a, b, "both variants must stat a real file identically");
        assert_eq!(a["exists"], true);
        assert_eq!(a["is_file"], true);
        assert_eq!(a["size_bytes"], 11);

        // A missing path falls into `stat_real_path`'s Err arm, which is a
        // different shape from the in-memory fallback even though both say
        // `exists: false`.
        let missing = derived
            .call(serde_json::json!({"path": "nope.txt"}), &ctx)
            .await
            .unwrap();
        let stat: serde_json::Value = serde_json::from_str(&missing.content).unwrap();
        assert_eq!(stat["exists"], false);
        assert_eq!(stat["is_file"], false);
        assert_eq!(stat["size_bytes"], serde_json::Value::Null);
    }

    /// The two variants differ only in where the root comes from. Everything the
    /// agent can see — name, description, schema, confirmation policy — must be
    /// identical, because an agent calling one of them must not get a different
    /// tool contract than it would from the registered variant.
    #[test]
    fn both_variants_advertise_the_same_surface() {
        let store = mem_store(vec![]);
        let plain = FileStatTool::new(Arc::clone(&store));
        let fs = FileStatToolFs::new(store, "/tmp");

        assert_eq!(plain.name(), fs.name());
        assert_eq!(plain.name(), "file_stat");
        assert_eq!(plain.description(), fs.description());
        assert_eq!(plain.schema(), fs.schema());
        assert_eq!(plain.requires_confirmation(), fs.requires_confirmation());
        assert!(!plain.requires_confirmation(), "stat only reads metadata");
    }

    /// `Debug` is how a misconfigured store gets diagnosed in a log line, and the
    /// two impls print different fields. Pin both shapes.
    #[test]
    fn debug_names_the_variant_and_the_fs_root() {
        let store = mem_store(vec![]);
        let plain = format!("{:?}", FileStatTool::new(Arc::clone(&store)));
        let fs = format!("{:?}", FileStatToolFs::new(store, "/tmp/x"));
        assert!(plain.contains("FileStatTool"), "{plain}");
        assert!(
            !plain.contains("/tmp/x"),
            "the plain variant has no root: {plain}"
        );
        assert!(fs.contains("FileStatToolFs"), "{fs}");
        assert!(fs.contains("/tmp/x"), "{fs}");
    }

    /// Assert that a `file_stat` variant rejects `input` with a message
    /// containing `needle`, so both variants can be driven through the same
    /// expectations without naming their concrete types.
    async fn expect_rejected(
        tool: &(dyn Tool<Inputs = serde_json::Value, Output = ToolResult> + Sync),
        ctx: &ToolContext,
        label: &str,
        input: serde_json::Value,
        needle: &str,
    ) {
        match tool.call(input, ctx).await {
            Ok(r) => panic!("{label} accepted input it should reject: {r:?}"),
            Err(err) => assert!(
                format!("{err}").contains(needle),
                "{label} rejected the wrong way, wanted {needle:?} in: {err}"
            ),
        }
    }

    /// Both variants validate their input before touching the filesystem, and
    /// `FileStatTool` — the one without an explicit root — is the variant that
    /// had no traversal test at all, which is exactly the case a caller reaches
    /// by constructing it directly.
    #[tokio::test]
    async fn both_variants_reject_bad_input_before_reading() {
        let store = mem_store(vec![("a.rs", "x")]);
        let plain = FileStatTool::new(Arc::clone(&store));
        let fs = FileStatToolFs::new(store, "/tmp");
        let ctx = ToolContext::new("fs7");

        let variants: [(
            &str,
            &(dyn Tool<Inputs = serde_json::Value, Output = ToolResult> + Sync),
        ); 2] = [("FileStatTool", &plain), ("FileStatToolFs", &fs)];
        for (label, tool) in variants {
            // `path` absent entirely.
            expect_rejected(
                tool,
                &ctx,
                label,
                serde_json::json!({}),
                "required field 'path'",
            )
            .await;
            // `path` present but not a string.
            expect_rejected(
                tool,
                &ctx,
                label,
                serde_json::json!({"path": 42}),
                "required field 'path'",
            )
            .await;
            // Directory traversal.
            expect_rejected(
                tool,
                &ctx,
                label,
                serde_json::json!({"path": "../etc/passwd"}),
                "directory traversal is not allowed",
            )
            .await;
            // An empty / whitespace-only path.
            expect_rejected(
                tool,
                &ctx,
                label,
                serde_json::json!({"path": "   "}),
                "must not be empty",
            )
            .await;
        }
    }

    /// A cancelled token must abort before any read, and it must be a hard error
    /// rather than an empty success — `exists: false` would tell the agent the
    /// file is missing when the truth is that it was interrupted.
    #[tokio::test]
    async fn both_variants_refuse_to_run_once_cancelled() {
        let store = mem_store(vec![("a.rs", "x")]);
        let plain = FileStatTool::new(Arc::clone(&store));
        let fs = FileStatToolFs::new(store, "/tmp");

        let mut ctx = ToolContext::new("fs8");
        ctx.cancel_token.cancel();
        let variants: [(
            &str,
            &(dyn Tool<Inputs = serde_json::Value, Output = ToolResult> + Sync),
        ); 2] = [("FileStatTool", &plain), ("FileStatToolFs", &fs)];
        for (label, tool) in variants {
            let err = tool
                .call(serde_json::json!({"path": "a.rs"}), &ctx)
                .await
                .expect_err(&format!("{label} must not return a result once cancelled"));
            assert!(format!("{err}").contains("cancelled"), "{label}: {err}");
            assert!(
                matches!(err, AgtrsError::Cancelled { .. }),
                "{label}: expected AgtrsError::Cancelled, got {err:?}"
            );
        }
    }
}
