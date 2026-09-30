//! `GlobTool` — find files in the workspace matching a glob pattern.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_workspace::WorkspaceStore;

use crate::error::{require_str, validate_path};
use crate::fs_store::FsWorkspaceStore;

/// Find files matching a glob pattern in the workspace.
///
/// # Input schema
///
/// ```json
/// {
///   "pattern": "src/**/*.rs",
///   "max_results": 200
/// }
/// ```
pub struct GlobTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl GlobTool {
    /// Create a new `GlobTool` backed by `workspace`.
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace }
    }

    const TOOL_NAME: &'static str = "glob";
    const DEFAULT_MAX_RESULTS: usize = 200;
}

impl std::fmt::Debug for GlobTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobTool").finish()
    }
}

#[async_trait]
impl Tool for GlobTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Find files in the workspace matching a glob pattern (e.g. \"src/**/*.rs\"). \
         Returns a sorted JSON array of matching workspace-relative paths. \
         Patterns must be relative — absolute paths and '..' are rejected."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g. \"src/**/*.rs\", \"**/*.toml\"). Must be relative."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2000,
                    "default": 200,
                    "description": "Maximum number of results to return. Default: 200."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "glob", skip(self, ctx), fields(pattern))]
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

        let pattern = require_str(Self::TOOL_NAME, &input, "pattern").map_err(AgtrsError::from)?;

        // Reject patterns that start with "/" or contain ".."
        if pattern.starts_with('/') {
            return Ok(ToolResult::error(
                "glob pattern must be relative (must not start with '/')".to_string(),
                &ctx.tool_use_id,
            ));
        }
        validate_path(Self::TOOL_NAME, pattern).map_err(AgtrsError::from)?;

        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(Self::DEFAULT_MAX_RESULTS as u64) as usize;

        tracing::Span::current().record("pattern", pattern);

        // Get workspace root for FsWorkspaceStore, or fall back to listing all files
        let matches = self.glob_files(pattern, max_results).await;

        tracing::debug!(pattern, count = matches.len(), "glob");

        let json = serde_json::to_string_pretty(&matches).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

impl GlobTool {
    async fn glob_files(&self, pattern: &str, max_results: usize) -> Vec<String> {
        // Try to get the real root via downcasting to FsWorkspaceStore
        // If the workspace is in-memory, fall back to filtering the list
        if let Some(fs_store) = self.as_fs_store() {
            self.glob_on_disk(fs_store.root(), pattern, max_results)
        } else {
            // In-memory fallback: filter workspace.list() against the pattern
            self.glob_in_memory(pattern, max_results).await
        }
    }

    fn as_fs_store(&self) -> Option<&FsWorkspaceStore> {
        // Use Arc::downcast-equivalent via Any — but WorkspaceStore doesn't require Any.
        // Instead we expose root via a concrete type stored in GlobTool for fs stores.
        // Since WorkspaceStore doesn't expose root(), we need another approach.
        // We store an Option<PathBuf> root at construction time for real fs stores.
        // For now, use a workaround: try to glob on disk using root from a separate field.
        None // handled via GlobTool::root field — see GlobToolWithRoot
    }

    async fn glob_in_memory(&self, pattern: &str, max_results: usize) -> Vec<String> {
        let all = self.workspace.list().await;
        let matcher = match glob::Pattern::new(pattern) {
            Ok(m) => m,
            Err(_) => return vec![format!("error: invalid glob pattern '{pattern}'")],
        };

        let mut matches: Vec<String> = all
            .into_iter()
            .filter(|p| matcher.matches(p))
            .take(max_results)
            .collect();
        matches.sort();
        matches
    }

    fn glob_on_disk(&self, root: &Path, pattern: &str, max_results: usize) -> Vec<String> {
        let full_pattern = root.join(pattern);
        let full_pattern_str = full_pattern.to_string_lossy();

        let mut matches: Vec<String> = Vec::new();
        if let Ok(paths) = glob::glob(&full_pattern_str) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    if let Ok(rel) = entry.strip_prefix(root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        matches.push(rel_str);
                    }
                }
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        matches.sort();
        matches
    }
}

/// A `GlobTool` that holds the real filesystem root for accurate glob matching.
///
/// Use this instead of `GlobTool::new` when you have an `FsWorkspaceStore`.
pub struct GlobToolFs {
    #[allow(dead_code)]
    inner: Arc<dyn WorkspaceStore>,
    root: std::path::PathBuf,
}

impl GlobToolFs {
    /// Create a `GlobTool` backed by a real filesystem root.
    pub fn new(workspace: Arc<dyn WorkspaceStore>, root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            inner: workspace,
            root: root.into(),
        }
    }

    const TOOL_NAME: &'static str = "glob";
}

impl std::fmt::Debug for GlobToolFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobToolFs")
            .field("root", &self.root)
            .finish()
    }
}

#[async_trait]
impl Tool for GlobToolFs {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Find files in the workspace matching a glob pattern (e.g. \"src/**/*.rs\"). \
         Returns a sorted JSON array of matching workspace-relative paths. \
         Patterns must be relative — absolute paths and '..' are rejected."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g. \"src/**/*.rs\", \"**/*.toml\"). Must be relative."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2000,
                    "default": 200,
                    "description": "Maximum number of results to return. Default: 200."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "glob", skip(self, ctx), fields(pattern))]
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

        let pattern = require_str(Self::TOOL_NAME, &input, "pattern").map_err(AgtrsError::from)?;

        if pattern.starts_with('/') {
            return Ok(ToolResult::error(
                "glob pattern must be relative (must not start with '/')".to_string(),
                &ctx.tool_use_id,
            ));
        }
        validate_path(Self::TOOL_NAME, pattern).map_err(AgtrsError::from)?;

        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;

        tracing::Span::current().record("pattern", pattern);

        let full_pattern = self.root.join(pattern);
        let full_pattern_str = full_pattern.to_string_lossy();

        let mut matches: Vec<String> = Vec::new();
        if let Ok(paths) = glob::glob(&full_pattern_str) {
            for entry in paths.flatten() {
                if entry.is_file() {
                    if let Ok(rel) = entry.strip_prefix(&self.root) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        matches.push(rel_str);
                    }
                }
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        matches.sort();

        tracing::debug!(pattern, count = matches.len(), "glob");

        let json = serde_json::to_string_pretty(&matches).unwrap_or_else(|_| "[]".to_string());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_workspace::InMemoryWorkspaceStore;
    use tempfile::TempDir;
    use tokio::fs;

    fn mem_store_with_files(files: Vec<(&str, &str)>) -> Arc<dyn WorkspaceStore> {
        Arc::new(InMemoryWorkspaceStore::with_files(
            files
                .into_iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect::<Vec<_>>(),
        )) as Arc<dyn WorkspaceStore>
    }

    #[tokio::test]
    async fn glob_matches_rs_files_in_memory() {
        let store = mem_store_with_files(vec![
            ("src/main.rs", ""),
            ("src/lib.rs", ""),
            ("README.md", ""),
            ("Cargo.toml", ""),
        ]);
        let tool = GlobTool::new(store);
        let ctx = ToolContext::new("g1");
        let result = tool
            .call(serde_json::json!({"pattern": "src/*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"src/lib.rs".to_string()));
        assert!(!paths.iter().any(|p| p.ends_with(".md")));
    }

    #[tokio::test]
    async fn glob_rejects_absolute_pattern() {
        let store = mem_store_with_files(vec![]);
        let tool = GlobTool::new(store);
        let ctx = ToolContext::new("g2");
        let result = tool
            .call(serde_json::json!({"pattern": "/etc/passwd"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn glob_rejects_traversal_pattern() {
        let store = mem_store_with_files(vec![]);
        let tool = GlobTool::new(store);
        let ctx = ToolContext::new("g3");
        let result = tool
            .call(serde_json::json!({"pattern": "../secret/*.rs"}), &ctx)
            .await;
        // Should be an Err or error ToolResult
        assert!(result.is_err() || result.unwrap().is_error);
    }

    #[tokio::test]
    async fn glob_fs_finds_files_on_disk() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).await.unwrap();
        fs::write(root.join("src/main.rs"), "").await.unwrap();
        fs::write(root.join("src/lib.rs"), "").await.unwrap();
        fs::write(root.join("Cargo.toml"), "").await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(root)) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, root);
        let ctx = ToolContext::new("gfs1");
        let result = tool
            .call(serde_json::json!({"pattern": "src/*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"src/lib.rs".to_string()));
        assert!(!paths.iter().any(|p| p.ends_with(".toml")));
    }

    #[tokio::test]
    async fn glob_returns_sorted_results() {
        let store = mem_store_with_files(vec![("c.rs", ""), ("a.rs", ""), ("b.rs", "")]);
        let tool = GlobTool::new(store);
        let ctx = ToolContext::new("g4");
        let result = tool
            .call(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let paths: Vec<String> = serde_json::from_str(&result.content).unwrap();
        let sorted = {
            let mut s = paths.clone();
            s.sort();
            s
        };
        assert_eq!(paths, sorted);
    }

    // ── Both glob tools must present the identical contract ──────────────

    /// `GlobTool` and `GlobToolFs` are two implementations of one tool. If
    /// their advertised name, schema or confirmation flag drift apart, an
    /// agent sees a different contract depending on which store is wired,
    /// so pin all three to the same values here.
    #[test]
    fn both_glob_tools_advertise_the_same_contract() {
        let store = mem_store_with_files(vec![]);
        let mem = GlobTool::new(Arc::clone(&store));
        let fs = GlobToolFs::new(store, "/tmp");
        assert_eq!(mem.name(), fs.name());
        assert_eq!(mem.name(), "glob");
        assert_eq!(mem.description(), fs.description());
        assert_eq!(mem.schema(), fs.schema());
        assert_eq!(mem.requires_confirmation(), fs.requires_confirmation());
        assert!(
            !mem.requires_confirmation(),
            "glob is read-only and must never be gated behind a confirmation prompt"
        );
    }

    #[test]
    fn the_glob_schema_requires_only_a_pattern_and_forbids_extras() {
        let tool = GlobTool::new(mem_store_with_files(vec![]));
        let s = tool.schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["required"][0], "pattern");
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["max_results"]["default"], 200);
    }

    #[test]
    fn the_fs_glob_debug_output_names_its_root() {
        let tool = GlobToolFs::new(mem_store_with_files(vec![]), "/tmp/x");
        let dbg = format!("{tool:?}");
        assert!(
            dbg.contains("GlobToolFs") && dbg.contains("/tmp/x"),
            "the root must be visible in Debug so a misconfigured store is diagnosable: {dbg}"
        );
    }

    // ── Cancellation ─────────────────────────────────────────────────────

    /// Both variants must honour the cancel token before doing any work, and
    /// report it as a hard error rather than an empty success — an empty
    /// result set would tell the agent "nothing matched" when the truth is
    /// "you were interrupted".
    #[tokio::test]
    async fn both_glob_tools_refuse_to_run_once_cancelled() {
        let store = mem_store_with_files(vec![("a.rs", "")]);

        let mem = GlobTool::new(Arc::clone(&store));
        let mut ctx = ToolContext::new("gc1");
        ctx.cancel_token.cancel();
        let err = mem
            .call(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .expect_err("a cancelled glob must not return a result");
        assert!(format!("{err}").contains("cancelled"), "{err}");

        let fs = GlobToolFs::new(store, "/tmp");
        let mut ctx = ToolContext::new("gc2");
        ctx.cancel_token.cancel();
        let err = fs
            .call(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .expect_err("a cancelled glob must not return a result");
        assert!(format!("{err}").contains("cancelled"), "{err}");
    }

    // ── max_results truncation ───────────────────────────────────────────

    /// `max_results` is the agent's only defence against a `**` pattern
    /// returning thousands of paths, so it must cap both backends, not just
    /// the in-memory one.
    #[tokio::test]
    async fn max_results_caps_both_backends() {
        let many: Vec<(String, String)> = (0..10)
            .map(|i| (format!("f{i}.rs"), String::new()))
            .collect();
        let store: Arc<dyn WorkspaceStore> =
            Arc::new(InMemoryWorkspaceStore::with_files(many.clone()));

        let mem = GlobTool::new(Arc::clone(&store));
        let out = mem
            .call(
                serde_json::json!({"pattern": "*.rs", "max_results": 3}),
                &ToolContext::new("gm1"),
            )
            .await
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert_eq!(paths.len(), 3, "in-memory glob ignored max_results");

        let tmp = TempDir::new().unwrap();
        for (name, _) in &many {
            fs::write(tmp.path().join(name), "").await.unwrap();
        }
        let fs_store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let fs_tool = GlobToolFs::new(fs_store, tmp.path());
        let out = fs_tool
            .call(
                serde_json::json!({"pattern": "*.rs", "max_results": 3}),
                &ToolContext::new("gm2"),
            )
            .await
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert_eq!(paths.len(), 3, "on-disk glob ignored max_results");
    }

    /// A non-numeric `max_results` must not silently become zero matches.
    #[tokio::test]
    async fn a_non_numeric_max_results_falls_back_to_the_default() {
        let store = mem_store_with_files(vec![("a.rs", ""), ("b.rs", "")]);
        let tool = GlobTool::new(store);
        let out = tool
            .call(
                serde_json::json!({"pattern": "*.rs", "max_results": "many"}),
                &ToolContext::new("gm3"),
            )
            .await
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert_eq!(
            paths.len(),
            2,
            "a bad max_results must not truncate to zero"
        );
    }

    // ── Error paths ──────────────────────────────────────────────────────

    /// The on-disk variant refuses an absolute pattern with the same message
    /// as the in-memory one, so the user sees one rule rather than two.
    #[tokio::test]
    async fn the_fs_glob_refuses_absolute_patterns_the_same_way() {
        let tmp = TempDir::new().unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, tmp.path());
        let out = tool
            .call(
                serde_json::json!({"pattern": "/etc/passwd"}),
                &ToolContext::new("ge1"),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("must not start with '/'"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn the_fs_glob_refuses_traversal_patterns() {
        let tmp = TempDir::new().unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, tmp.path());
        let res = tool
            .call(
                serde_json::json!({"pattern": "../outside/*.rs"}),
                &ToolContext::new("ge2"),
            )
            .await;
        assert!(
            res.is_err() || res.unwrap().is_error,
            "'..' must be refused"
        );
    }

    /// A missing `pattern` key is a caller error, not a match-everything
    /// request.
    #[tokio::test]
    async fn a_missing_pattern_is_rejected_by_both_tools() {
        let store = mem_store_with_files(vec![("a.rs", "")]);

        let res = GlobTool::new(Arc::clone(&store))
            .call(serde_json::json!({}), &ToolContext::new("gp1"))
            .await;
        assert!(res.is_err(), "{res:?}");

        let tmp = TempDir::new().unwrap();
        let fs_store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let res = GlobToolFs::new(fs_store, tmp.path())
            .call(serde_json::json!({}), &ToolContext::new("gp2"))
            .await;
        assert!(res.is_err(), "{res:?}");
    }

    /// Directories must never appear in results, even when a pattern would
    /// match them, because a path the agent cannot `read_file` is noise.
    #[tokio::test]
    async fn directories_are_never_returned() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("pkg")).await.unwrap();
        fs::write(tmp.path().join("pkg/real.rs"), "").await.unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, tmp.path());
        let out = tool
            .call(
                serde_json::json!({"pattern": "pkg/*"}),
                &ToolContext::new("gd1"),
            )
            .await
            .unwrap();
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert_eq!(paths, vec!["pkg/real.rs".to_string()]);
    }

    /// The recursive pattern an agent is told to use in the tool description
    /// must actually work on disk, and must return workspace-relative paths.
    #[tokio::test]
    async fn the_recursive_pattern_from_the_description_works_on_disk() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src/deep/deeper"))
            .await
            .unwrap();
        fs::write(tmp.path().join("src/main.rs"), "").await.unwrap();
        fs::write(tmp.path().join("src/deep/mod.rs"), "")
            .await
            .unwrap();
        fs::write(tmp.path().join("src/deep/deeper/leaf.rs"), "")
            .await
            .unwrap();
        fs::write(tmp.path().join("notes.md"), "").await.unwrap();

        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, tmp.path());
        let out = tool
            .call(
                serde_json::json!({"pattern": "src/**/*.rs"}),
                &ToolContext::new("gr1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert!(paths.contains(&"src/main.rs".to_string()), "{paths:?}");
        assert!(paths.contains(&"src/deep/mod.rs".to_string()), "{paths:?}");
        assert!(
            paths.contains(&"src/deep/deeper/leaf.rs".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.iter().all(|p| !p.starts_with('/')),
            "results must be workspace-relative, got {paths:?}"
        );
    }

    /// A pattern matching nothing is a successful empty list, not an error.
    #[tokio::test]
    async fn a_pattern_with_no_matches_is_an_empty_list_not_an_error() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "").await.unwrap();
        let store =
            Arc::new(crate::fs_store::FsWorkspaceStore::new(tmp.path())) as Arc<dyn WorkspaceStore>;
        let tool = GlobToolFs::new(store, tmp.path());
        let out = tool
            .call(
                serde_json::json!({"pattern": "**/*.py"}),
                &ToolContext::new("gn1"),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let paths: Vec<String> = serde_json::from_str(&out.content).unwrap();
        assert!(paths.is_empty(), "{paths:?}");
    }
}
