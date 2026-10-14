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

/// Visibility rule shared by the on-disk and in-memory renderers so the two
/// cannot drift: dot-prefixed entries are hidden unless `show_hidden`, and the
/// noisy directories in [`SKIP_DIRS`] are skipped unconditionally.
fn skip_entry(name: &str, show_hidden: bool) -> bool {
    (!show_hidden && name.starts_with('.')) || SKIP_DIRS.contains(&name)
}

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
        if skip_entry(&name, show_hidden) {
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

/// One directory level of the tree built from an in-memory file list.
///
/// The `BTreeMap`/`BTreeSet` keep children sorted by name, so the rendered
/// order matches the on-disk renderer's "directories first, then files, both
/// alphabetical" sort.
#[derive(Default)]
struct MemDir {
    /// Sub-directories, keyed by directory name.
    dirs: std::collections::BTreeMap<String, MemDir>,
    /// Plain files at this level.
    files: std::collections::BTreeSet<String>,
}

impl MemDir {
    /// Insert a `/`-separated path relative to this node, creating the
    /// intermediate directories it implies. Empty components are ignored.
    fn insert(&mut self, rel: &str) {
        let mut node = self;
        let mut parts = rel.split('/').peekable();
        while let Some(part) = parts.next() {
            if part.is_empty() {
                continue;
            }
            if parts.peek().is_none() {
                node.files.insert(part.to_string());
            } else {
                node = node.dirs.entry(part.to_string()).or_default();
            }
        }
    }
}

/// Render one level of an in-memory tree, using the same connectors, `depth`
/// and `max_entries` semantics as [`render_tree`].
fn render_mem_dir(
    dir: &MemDir,
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

    // Directories before files, each already alphabetical from the BTree
    // containers.
    let mut entries: Vec<(String, bool)> = Vec::new();
    for name in dir.dirs.keys() {
        if !skip_entry(name, show_hidden) {
            entries.push((name.clone(), true));
        }
    }
    for name in dir.files.iter() {
        if !skip_entry(name, show_hidden) {
            entries.push((name.clone(), false));
        }
    }

    let total = entries.len();
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        if *count >= max_entries {
            lines.push(format!("{prefix}... (truncated)"));
            break;
        }
        let is_last = i == total - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let display = if *is_dir {
            format!("{name}/")
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
            render_mem_dir(
                &dir.dirs[name],
                &child_prefix,
                depth - 1,
                show_hidden,
                lines,
                count,
                max_entries,
            );
        }
    }
}

/// Render a tree from an in-memory file list.
///
/// This is the `TreeTool` path taken when the workspace has no on-disk root
/// (for example `InMemoryWorkspaceStore`, whose `root_display()` is
/// `"<in-memory>"`). It produces the same shape of output as [`render_tree`]:
/// directories before files, `├── `/`└── ` connectors, indentation for
/// nesting, and the same `depth`, `max_entries` and `show_hidden` semantics.
fn render_in_memory(
    files: Vec<String>,
    base: &str,
    depth: usize,
    show_hidden: bool,
    max_entries: usize,
) -> String {
    // Strip the base prefix so the rendered tree is rooted at `base`.
    let prefix = if base == "." {
        String::new()
    } else {
        format!("{base}/")
    };

    let mut root = MemDir::default();
    for f in files {
        let rel = if prefix.is_empty() {
            f
        } else if let Some(rest) = f.strip_prefix(prefix.as_str()) {
            rest.to_string()
        } else {
            // Outside the requested base (this also covers `f == base` itself,
            // which names the tree root rather than one of its entries).
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        root.insert(&rel);
    }

    let mut lines = Vec::new();
    let mut count = 0usize;
    render_mem_dir(
        &root,
        "",
        depth,
        show_hidden,
        &mut lines,
        &mut count,
        max_entries,
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;
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

    // ── TreeTool (in-memory workspace) ────────────────────────────────────────
    //
    // `TreeTool` takes the in-memory branch whenever `root_display()` is empty
    // or `"<in-memory>"`, which is hardcoded for `InMemoryWorkspaceStore`. It is
    // `pub`, so constructing it directly is a supported use and it must render a
    // real tree, not a flat path list.

    /// The fixture shared by the in-memory tests: two files under `src/`, two
    /// top-level files, plus a hidden file and a `.git/` entry.
    fn in_memory_fixture() -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files([
            ("src/main.rs".to_string(), String::new()),
            ("src/lib.rs".to_string(), String::new()),
            ("Cargo.toml".to_string(), String::new()),
            ("README.md".to_string(), String::new()),
            (".hidden".to_string(), String::new()),
            (".git/config".to_string(), String::new()),
        ])) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn in_memory_tree_nests_files_under_directories() {
        let tool = TreeTool::new(in_memory_fixture());
        let ctx = ToolContext::new("m1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error, "error: {}", result.content);

        // Exact expected tree: directories first, then files, each alphabetical,
        // `src/main.rs` indented underneath `src/`.
        assert_eq!(
            result.content,
            "\
├── src/
│   ├── lib.rs
│   └── main.rs
├── Cargo.toml
└── README.md",
            "content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn in_memory_tree_respects_depth() {
        let store = Arc::new(InMemoryWorkspaceStore::with_files([
            ("src/main.rs".to_string(), String::new()),
            ("a/b/c/deep.rs".to_string(), String::new()),
        ])) as Arc<dyn WorkspaceStore>;
        let tool = TreeTool::new(store);

        // depth 2 shows two levels of entries: `a/` and its child `b/`, but not
        // `a/b/c/`. This matches `render_tree`, which recurses with `depth - 1`
        // and only descends while `depth > 1`.
        let ctx = ToolContext::new("m2");
        let out = tool
            .call(serde_json::json!({"depth": 2}), &ctx)
            .await
            .unwrap()
            .content;
        assert_eq!(
            out,
            "\
├── a/
│   └── b/
└── src/
    └── main.rs",
            "content: {out}"
        );

        // depth 3 reaches `a/b/c/` but still not `a/b/c/deep.rs`.
        let ctx = ToolContext::new("m2b");
        let out = tool
            .call(serde_json::json!({"depth": 3}), &ctx)
            .await
            .unwrap()
            .content;
        assert_eq!(
            out,
            "\
├── a/
│   └── b/
│       └── c/
└── src/
    └── main.rs",
            "content: {out}"
        );
    }

    #[tokio::test]
    async fn in_memory_tree_respects_max_entries() {
        let tool = TreeTool::new(in_memory_fixture());
        let ctx = ToolContext::new("m3");
        let out = tool
            .call(serde_json::json!({"max_entries": 2}), &ctx)
            .await
            .unwrap()
            .content;
        // Only `max_entries` real entries are rendered; the rest is reported as
        // truncation. (Each level that ran out of budget appends its own marker,
        // exactly as `render_tree` does.)
        let entries: Vec<&str> = out.lines().filter(|l| l.contains("── ")).collect();
        assert_eq!(entries.len(), 2, "content: {out}");
        assert_eq!(entries[0], "├── src/");
        assert_eq!(entries[1], "│   ├── lib.rs");
        assert!(
            out.lines().any(|l| l.ends_with("... (truncated)")),
            "expected a truncation marker: {out}"
        );
    }

    #[tokio::test]
    async fn in_memory_tree_matches_the_on_disk_renderer() {
        // Same fixture through both tools: the in-memory renderer must honour
        // depth, max_entries and show_hidden exactly as `render_tree` does.
        let tmp = TempDir::new().unwrap();
        for f in [
            "src/main.rs",
            "src/lib.rs",
            "Cargo.toml",
            "README.md",
            ".hidden",
        ] {
            let p = tmp.path().join(f);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).await.unwrap();
            }
            fs::write(&p, "").await.unwrap();
        }
        fs::create_dir_all(tmp.path().join(".git")).await.unwrap();
        fs::write(tmp.path().join(".git/config"), "").await.unwrap();

        let fs_store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let on_disk = TreeToolFs::new(fs_store, tmp.path());

        let mem = TreeTool::new(in_memory_fixture());

        for input in [
            serde_json::json!({}),
            serde_json::json!({"depth": 1}),
            serde_json::json!({"depth": 2}),
            serde_json::json!({"show_hidden": true}),
            serde_json::json!({"max_entries": 2}),
            serde_json::json!({"max_entries": 3, "depth": 2}),
        ] {
            let ctx = ToolContext::new("mp");
            let a = on_disk.call(input.clone(), &ctx).await.unwrap().content;
            let b = mem.call(input.clone(), &ctx).await.unwrap().content;
            assert_eq!(a, b, "renderers disagree for {input}");
        }
    }

    #[tokio::test]
    async fn in_memory_tree_honours_show_hidden() {
        let tool = TreeTool::new(in_memory_fixture());

        // Default: dot-prefixed entries are hidden.
        let ctx = ToolContext::new("m4");
        let out = tool
            .call(serde_json::json!({}), &ctx)
            .await
            .unwrap()
            .content;
        assert!(!out.contains(".hidden"), "content: {out}");
        // `.git/` is in SKIP_DIRS and stays skipped even when asked for.
        assert!(!out.contains(".git"), "content: {out}");

        // show_hidden: true surfaces the hidden file (but not the skipped dir).
        let ctx = ToolContext::new("m4b");
        let out = tool
            .call(serde_json::json!({"show_hidden": true}), &ctx)
            .await
            .unwrap()
            .content;
        assert!(out.contains(".hidden"), "content: {out}");
        assert!(!out.contains(".git"), "SKIP_DIRS entry leaked: {out}");
    }

    #[tokio::test]
    async fn in_memory_tree_honours_a_base_path() {
        let tool = TreeTool::new(in_memory_fixture());
        let ctx = ToolContext::new("m5");
        let out = tool
            .call(serde_json::json!({"path": "src"}), &ctx)
            .await
            .unwrap()
            .content;
        assert_eq!(out, "├── lib.rs\n└── main.rs", "content: {out}");
    }

    #[tokio::test]
    async fn in_memory_tree_rejects_traversal_path() {
        let tool = TreeTool::new(in_memory_fixture());
        let ctx = ToolContext::new("m6");
        let result = tool
            .call(serde_json::json!({"path": "../other"}), &ctx)
            .await;
        assert!(result.is_err() || result.unwrap().is_error);
    }

    // ── Tool trait surface: name / description / schema ──────────────────

    #[test]
    fn the_tool_is_named_tree() {
        assert_eq!(TreeTool::new(in_memory_fixture()).name(), "tree");
    }

    #[test]
    fn the_description_names_what_it_skips() {
        let tool = TreeTool::new(in_memory_fixture());
        let d = tool.description();
        assert!(!d.is_empty());
        assert!(d.contains("depth"), "depth must be documented: {d}");
        // Which directories are named is covered by
        // `skip_dirs_are_documented_apart_from_the_known_gaps`.
    }

    /// The description drifts from [`SKIP_DIRS`] in two known ways.
    ///
    /// Pinned rather than asserted away, because both are real defects a reader
    /// of this file should see:
    ///  1. `__pycache__` is misspelled as `__pycache/` in the description, so
    ///     the agent is shown a directory name that does not exist.
    ///  2. `.cache` is filtered but not mentioned at all.
    ///
    /// Fix either and this test fails, which is the point: it should be deleted
    /// only when both are corrected.
    #[test]
    fn the_description_drift_from_skip_dirs_is_pinned() {
        let tool = TreeTool::new(in_memory_fixture());
        let d = tool.description();

        // (1) The typo, spelled out exactly as it appears in the source.
        assert!(
            d.contains("__pycache/"),
            "the `__pycache/` typo was corrected; fold this into the \
             description test above"
        );
        assert!(
            !d.contains("__pycache__"),
            "`__pycache__` is now spelled correctly; fold this into the \
             description test above"
        );

        // (2) `.cache` is filtered but never mentioned.
        assert!(
            !d.contains(".cache"),
            "`.cache` is now documented; fold this into the description \
             test above"
        );
    }

    /// Every `SKIP_DIRS` entry except the two known gaps is documented.
    #[test]
    fn skip_dirs_are_documented_apart_from_the_known_gaps() {
        let tool = TreeTool::new(in_memory_fixture());
        let d = tool.description();
        for skipped in SKIP_DIRS {
            if *skipped == ".cache" {
                continue; // pinned as a gap above
            }
            if *skipped == "__pycache__" {
                continue; // misspelled; pinned as a gap above
            }
            let needle = format!("{skipped}/");
            assert!(
                d.contains(needle.as_str()),
                "{skipped} is filtered but the description never names it: {d}"
            );
        }
    }

    #[test]
    fn the_tool_is_read_only_and_needs_no_confirmation() {
        assert!(!TreeTool::new(in_memory_fixture()).requires_confirmation());
    }

    /// The schema is the contract the model writes against, so pin its shape
    /// rather than just that it exists.
    #[test]
    fn the_schema_declares_its_four_options_with_bounds() {
        let schema = TreeTool::new(in_memory_fixture()).schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["additionalProperties"], false,
            "unknown keys must be rejected, not silently ignored"
        );

        let props = schema["properties"].clone();
        let props = &props;
        let expected: &[(&str, &str)] = &[
            ("path", "string"),
            ("depth", "integer"),
            ("show_hidden", "boolean"),
            ("max_entries", "integer"),
        ];
        for (name, ty) in expected {
            assert_eq!(
                props[name]["type"], *ty,
                "{name} must be declared as a {ty}"
            );
            assert!(
                !props[name]["description"].as_str().unwrap_or("").is_empty(),
                "{name} needs a description for the model to use it"
            );
        }
        assert_eq!(
            props.as_object().map(|o| o.len()).unwrap_or(0),
            4,
            "every option must be listed, and only the documented ones"
        );
    }

    /// `depth` and `max_entries` are bounded in the schema so a runaway value
    /// cannot be sent; both defaults must match what `call` actually uses.
    #[test]
    fn the_numeric_options_are_bounded_with_matching_defaults() {
        let props = TreeTool::new(in_memory_fixture()).schema()["properties"].clone();

        let depth = &props["depth"];
        assert_eq!(depth["minimum"], 1);
        assert_eq!(depth["maximum"], 10);
        assert_eq!(depth["default"], 3);

        let max_entries = &props["max_entries"];
        assert_eq!(max_entries["minimum"], 1);
        assert_eq!(max_entries["maximum"], 2000);
        assert_eq!(max_entries["default"], 200);
    }

    #[tokio::test]
    async fn the_schema_default_depth_matches_the_rendered_output() {
        let tmp = TempDir::new().unwrap();
        // Nest deeper than the documented default depth of 3.
        let mut deep = tmp.path().to_path_buf();
        for i in 0..6 {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).await.unwrap();
        fs::write(deep.join("leaf.txt"), "").await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let result = TreeToolFs::new(store, tmp.path())
            .call(serde_json::json!({}), &ToolContext::new("sd"))
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(
            !result.content.contains("leaf.txt"),
            "a depth-6 path must be cut off at the documented default of 3: {}",
            result.content
        );

        // Asking for more depth must reveal it.
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let deeper = TreeToolFs::new(store, tmp.path())
            .call(serde_json::json!({"depth": 10}), &ToolContext::new("sd2"))
            .await
            .unwrap();
        assert!(
            deeper.content.contains("leaf.txt"),
            "depth 10 must reach the leaf: {}",
            deeper.content
        );
    }
}
