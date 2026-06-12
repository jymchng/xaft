//! `TreeTool` — render an ASCII tree of the workspace directory structure.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{opt_bool, opt_u64, validate_path};

/// Skip these directories — they are noisy and almost never relevant.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".cache"];

/// Render an ASCII directory tree.
///
/// # Input schema
///
/// ```json
/// {
///   "path": ".",
///   "depth": 3,
///   "show_hidden": false,
///   "max_entries": 200
/// }
/// ```
pub struct TreeTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl TreeTool {
    /// Create a new `TreeTool`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "tree";
}

impl std::fmt::Debug for TreeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeTool").finish()
    }
}

#[async_trait]
impl Tool for TreeTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Render an ASCII tree of the workspace directory structure. \
         Skips .git/, target/, node_modules/, and __pycache/. \
         Use depth to limit recursion depth."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative directory to tree (default: \".\")."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "default": 3,
                    "description": "Maximum recursion depth. Default: 3."
                },
                "show_hidden": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include hidden files/dirs (starting with '.'). Default: false."
                },
                "max_entries": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2000,
                    "default": 200,
                    "description": "Maximum number of entries to show. Default: 200."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "tree", skip(self, ctx), fields(path))]
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

        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        // Allow "." but validate non-root paths
        if path != "." {
            validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;
        }

        let depth = opt_u64(&input, "depth").unwrap_or(3) as usize;
        let show_hidden = opt_bool(&input, "show_hidden").unwrap_or(false);
        let max_entries = opt_u64(&input, "max_entries").unwrap_or(200) as usize;

        tracing::Span::current().record("path", path);

        // Get workspace root via root_display heuristic
        let root_display = self.workspace.root_display();
        let output = if !root_display.is_empty() && root_display != "<in-memory>" {
            let root = std::path::Path::new(&root_display);
            let start = if path == "." {
                root.to_path_buf()
            } else {
                root.join(path)
            };
            let mut lines = Vec::new();
            let mut count = 0usize;
            render_tree(
                &start,
                "",
                depth,
                show_hidden,
                &mut lines,
                &mut count,
                max_entries,
            )
            .await;
            lines.join("\n")
        } else {
            // In-memory: simulate from workspace.list()
            render_in_memory(
                self.workspace.list().await,
                path,
                depth,
                show_hidden,
                max_entries,
            )
        };

        tracing::debug!(path, "tree");
        Ok(ToolResult::ok(output, &ctx.tool_use_id))
    }
}

/// A `TreeTool` backed by a real filesystem root.
pub struct TreeToolFs {
    #[allow(dead_code)]
    workspace: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl TreeToolFs {
    /// Create a `TreeTool` backed by a real filesystem root.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "tree";
}

impl std::fmt::Debug for TreeToolFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeToolFs").finish()
    }
}

#[async_trait]
impl Tool for TreeToolFs {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Render an ASCII tree of the workspace directory structure. \
         Skips .git/, target/, node_modules/, and __pycache/. \
         Use depth to limit recursion depth."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative directory (default: \".\")." },
                "depth": { "type": "integer", "minimum": 1, "maximum": 10, "default": 3 },
                "show_hidden": { "type": "boolean", "default": false },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 200 }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "tree", skip(self, ctx), fields(path))]
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

        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        if path != "." {
            validate_path(Self::TOOL_NAME, path).map_err(AgtrsError::from)?;
        }

        let depth = opt_u64(&input, "depth").unwrap_or(3) as usize;
        let show_hidden = opt_bool(&input, "show_hidden").unwrap_or(false);
        let max_entries = opt_u64(&input, "max_entries").unwrap_or(200) as usize;

        tracing::Span::current().record("path", path);

        let start = if path == "." {
            self.root.clone()
        } else {
            self.root.join(path)
        };

        let mut lines = Vec::new();
        let mut count = 0usize;
        render_tree(
            &start,
            "",
            depth,
            show_hidden,
            &mut lines,
            &mut count,
            max_entries,
        )
        .await;

        Ok(ToolResult::ok(lines.join("\n"), &ctx.tool_use_id))
    }
}

/// Recursively render ASCII tree lines.
#[async_recursion::async_recursion]
async fn render_tree(
    dir: &Path,
    prefix: &str,
    depth: usize,
    show_hidden: bool,
    lines: &mut Vec<String>,
    count: &mut usize,
    max_entries: usize,
) {
    if *count >= max_entries {
        return;
    }

    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut entries: Vec<(String, bool)> = Vec::new(); // (name, is_dir)
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        entries.push((name, is_dir));
    }
    entries.sort_by(|a, b| {
        // dirs first, then files, both sorted alphabetically
        match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        }
    });

    let total = entries.len();
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        if *count >= max_entries {
            lines.push(format!("{prefix}... (truncated)"));
            break;
        }
        let is_last = i == total - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let display = if *is_dir {
            format!("{}/", name)
        } else {
            name.clone()
        };
        lines.push(format!("{prefix}{connector}{display}"));
        *count += 1;

        if *is_dir && depth > 1 {
            let child_prefix = if is_last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}│   ")
            };
            render_tree(
                &dir.join(name),
                &child_prefix,
                depth - 1,
                show_hidden,
                lines,
                count,
                max_entries,
            )
            .await;
        }
    }
}

/// Render a tree from an in-memory file list.
fn render_in_memory(
    files: Vec<String>,
    base: &str,
    depth: usize,
    _show_hidden: bool,
    max_entries: usize,
) -> String {
    // Filter by base
    let filtered: Vec<String> = files
        .into_iter()
        .filter(|p| {
            if base == "." {
                true
            } else {
                p.starts_with(&format!("{base}/")) || p == base
            }
        })
        .map(|p| {
            if base == "." {
                p
            } else {
                p.trim_start_matches(&format!("{base}/")).to_string()
            }
        })
        .collect();

    // Collect unique path components at each level
    let mut dirs: std::collections::BTreeSet<String> = Default::default();
    for f in &filtered {
        let parts: Vec<&str> = f.splitn(depth + 1, '/').collect();
        if parts.len() > 1 {
            dirs.insert(parts[0].to_string());
        }
    }

    let mut lines = Vec::new();
    let mut count = 0usize;

    for f in filtered.iter().take(max_entries) {
        if count >= max_entries {
            break;
        }
        lines.push(f.clone());
        count += 1;
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::fs;

    #[tokio::test]
    async fn tree_renders_directory_structure() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).await.unwrap();
        fs::write(tmp.path().join("src/main.rs"), "").await.unwrap();
        fs::write(tmp.path().join("src/lib.rs"), "").await.unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "").await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = TreeToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(
            result.content.contains("src/"),
            "content: {}",
            result.content
        );
        assert!(
            result.content.contains("main.rs"),
            "content: {}",
            result.content
        );
        assert!(
            result.content.contains("Cargo.toml"),
            "content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn tree_skips_git_and_target() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).await.unwrap();
        fs::create_dir_all(tmp.path().join("target")).await.unwrap();
        fs::write(tmp.path().join("src.rs"), "").await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = TreeToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(
            !result.content.contains(".git"),
            "content: {}",
            result.content
        );
        assert!(
            !result.content.contains("target"),
            "content: {}",
            result.content
        );
        assert!(result.content.contains("src.rs"));
    }

    #[tokio::test]
    async fn tree_respects_depth() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).await.unwrap();
        fs::write(tmp.path().join("a/b/c/deep.rs"), "")
            .await
            .unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = TreeToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("t3");
        // depth=2 should not reach a/b/c/
        let result = tool
            .call(serde_json::json!({"depth": 2}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            !result.content.contains("deep.rs"),
            "should not reach depth 3: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn tree_rejects_traversal_path() {
        let tmp = TempDir::new().unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = TreeToolFs::new(store, tmp.path());
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(serde_json::json!({"path": "../other"}), &ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
