//! `GitDiffTool` — show unified diff via `agtrs-git::GitRepo`.
//!
//! Supports four targets: `"head"` (default), `"staged"`, `"unstaged"`, or a
//! from/to ref pair.  An optional `path` filter narrows the diff to one file.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Return the unified diff of changes.
pub struct GitDiffTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitDiffTool {
    /// Create from a shared `GitRepo` and optional repo root path.
    ///
    /// When `repo_path` is `None`, `git diff HEAD` is used via the `GitRepo`
    /// API (backward-compatible path).
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_diff";
}

impl std::fmt::Debug for GitDiffTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitDiffTool").finish()
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Show a unified diff. target can be 'head' (default), 'staged', 'unstaged', \
         or specify from_ref/to_ref for a ref-to-ref diff. Optional 'path' filter."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "enum": ["head", "staged", "unstaged"],
                    "description": "What to diff: 'head' (working tree vs HEAD, default), \
                                    'staged' (index vs HEAD), 'unstaged' (working tree vs index)."
                },
                "path": {
                    "type": "string",
                    "description": "Limit diff to this file path."
                },
                "from_ref": {
                    "type": "string",
                    "description": "Start ref for a ref-to-ref diff (overrides target)."
                },
                "to_ref": {
                    "type": "string",
                    "description": "End ref for a ref-to-ref diff (used with from_ref)."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_diff", skip_all)]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before start", Self::TOOL_NAME),
            });
        }

        let target = input
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("head");
        let path = input.get("path").and_then(|v| v.as_str());
        let from_ref = input.get("from_ref").and_then(|v| v.as_str());
        let to_ref = input.get("to_ref").and_then(|v| v.as_str());

        // If from_ref is provided, do a ref-to-ref diff via git command
        if from_ref.is_some() || to_ref.is_some() {
            return self.ref_diff(from_ref, to_ref, path, ctx).await;
        }

        match target {
            "staged" => {
                // git diff --cached
                let mut args = vec!["diff", "--cached"];
                let path_arg;
                if let Some(p) = path {
                    args.push("--");
                    path_arg = p.to_string();
                    args.push(&path_arg);
                }
                let output = tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&self.repo_path)
                    .output()
                    .await
                    .map_err(|e| AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: e.to_string(),
                    })?;
                let content = String::from_utf8_lossy(&output.stdout).into_owned();
                let text = if content.is_empty() {
                    "No staged changes.".to_string()
                } else {
                    content
                };
                Ok(ToolResult::ok(text, &ctx.tool_use_id))
            }
            "unstaged" => {
                // git diff (working tree vs index)
                let mut args = vec!["diff"];
                let path_arg;
                if let Some(p) = path {
                    args.push("--");
                    path_arg = p.to_string();
                    args.push(&path_arg);
                }
                let output = tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&self.repo_path)
                    .output()
                    .await
                    .map_err(|e| AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: e.to_string(),
                    })?;
                let content = String::from_utf8_lossy(&output.stdout).into_owned();
                let text = if content.is_empty() {
                    "No unstaged changes.".to_string()
                } else {
                    content
                };
                Ok(ToolResult::ok(text, &ctx.tool_use_id))
            }
            _ => {
                // "head" — use repo API for simple case, or git command if path filter
                if let Some(p) = path {
                    let mut args = vec!["diff", "HEAD", "--"];
                    let path_arg = p.to_string();
                    args.push(&path_arg);
                    let output = tokio::process::Command::new("git")
                        .args(&args)
                        .current_dir(&self.repo_path)
                        .output()
                        .await
                        .map_err(|e| AgtrsError::ToolCallFailed {
                            tool_name: Self::TOOL_NAME.to_string(),
                            reason: e.to_string(),
                        })?;
                    let content = String::from_utf8_lossy(&output.stdout).into_owned();
                    let text = if content.is_empty() {
                        format!("No changes since HEAD for '{p}'.")
                    } else {
                        content
                    };
                    Ok(ToolResult::ok(text, &ctx.tool_use_id))
                } else {
                    match self.repo.diff_head().await {
                        Ok(diff) => {
                            let content = if diff.unified.is_empty() {
                                "No changes since HEAD.".to_string()
                            } else {
                                diff.unified
                            };
                            Ok(ToolResult::ok(content, &ctx.tool_use_id))
                        }
                        Err(e) => Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
                    }
                }
            }
        }
    }
}

impl GitDiffTool {
    async fn ref_diff(
        &self,
        from_ref: Option<&str>,
        to_ref: Option<&str>,
        path: Option<&str>,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let mut args: Vec<String> = vec!["diff".to_string()];

        match (from_ref, to_ref) {
            (Some(f), Some(t)) => {
                args.push(format!("{}..{}", f, t));
            }
            (Some(f), None) => {
                args.push(f.to_string());
            }
            (None, Some(t)) => {
                args.push(format!("HEAD..{}", t));
            }
            (None, None) => {}
        }

        if let Some(p) = path {
            args.push("--".to_string());
            args.push(p.to_string());
        }

        let output = tokio::process::Command::new("git")
            .args(&args)
            .current_dir(&self.repo_path)
            .output()
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: e.to_string(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(ToolResult::error(
                format!("git diff failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let content = String::from_utf8_lossy(&output.stdout).into_owned();
        let text = if content.is_empty() {
            "No differences between refs.".to_string()
        } else {
            content
        };
        Ok(ToolResult::ok(text, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(dir: &TempDir) {
        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("foo.txt"), "original").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn diff_head_no_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("No changes"));
    }

    #[tokio::test]
    async fn diff_staged_shows_staged_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("new.txt"), "new content").unwrap();
        Command::new("git")
            .args(["add", "new.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"target": "staged"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("new.txt"));
    }

    #[tokio::test]
    async fn diff_unstaged_shows_unstaged_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("foo.txt"), "modified").unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(serde_json::json!({"target": "unstaged"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("foo.txt"));
    }

    #[tokio::test]
    async fn diff_with_path_filter() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("foo.txt"), "changed foo").unwrap();
        std::fs::write(tmp.path().join("bar.txt"), "changed bar").unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(serde_json::json!({"path": "foo.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        // Should include foo.txt but not bar.txt (bar isn't tracked yet)
        assert!(!result.content.contains("bar.txt"));
    }

    fn second_commit(dir: &TempDir) {
        std::fs::write(dir.path().join("foo.txt"), "second").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn diff_staged_with_no_changes_reports_none() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t5");
        let result = tool
            .call(serde_json::json!({"target": "staged"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "No staged changes.");
    }

    #[tokio::test]
    async fn diff_unstaged_with_no_changes_reports_none() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t6");
        let result = tool
            .call(serde_json::json!({"target": "unstaged"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "No unstaged changes.");
    }

    #[tokio::test]
    async fn diff_head_with_path_filter_reports_none_when_unchanged() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t7");
        let result = tool
            .call(serde_json::json!({"path": "foo.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content, "No changes since HEAD for 'foo.txt'.");
    }

    #[tokio::test]
    async fn diff_ref_to_ref_with_both_refs() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        second_commit(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t8");
        let result = tool
            .call(
                serde_json::json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(result.content.contains("-original"));
        assert!(result.content.contains("+second"));
    }

    #[tokio::test]
    async fn diff_from_ref_only_diffs_that_ref_against_the_worktree() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        second_commit(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t9");
        let result = tool
            .call(serde_json::json!({"from_ref": "HEAD~1"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert!(result.content.contains("+second"));
    }

    #[tokio::test]
    async fn diff_to_ref_only_compares_head_to_the_given_ref() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        second_commit(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t10");
        let result = tool
            .call(serde_json::json!({"to_ref": "HEAD"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        assert_eq!(result.content, "No differences between refs.");
    }

    #[tokio::test]
    async fn diff_ref_to_ref_with_path_filter_narrows_the_diff() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("bar.txt"), "tracked").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add bar"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        // A second commit touching BOTH files, so the ref-to-ref diff
        // genuinely has content for each and the filter has something to
        // exclude. (`git diff A..B` compares two trees, so uncommitted
        // working-tree edits would not appear at all.)
        std::fs::write(tmp.path().join("foo.txt"), "touched").unwrap();
        std::fs::write(tmp.path().join("bar.txt"), "also touched").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "touch both"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t11");

        let both = tool
            .call(
                serde_json::json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!both.is_error, "error: {}", both.content);
        assert!(both.content.contains("foo.txt"), "{}", both.content);
        assert!(both.content.contains("bar.txt"), "{}", both.content);

        let only_foo = tool
            .call(
                serde_json::json!({"from_ref": "HEAD~1", "to_ref": "HEAD", "path": "foo.txt"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!only_foo.is_error, "error: {}", only_foo.content);
        assert!(only_foo.content.contains("foo.txt"));
        assert!(
            !only_foo.content.contains("bar.txt"),
            "the path filter must exclude bar.txt: {}",
            only_foo.content
        );
    }

    #[tokio::test]
    async fn diff_ref_to_ref_outside_a_repository_reports_git_failure() {
        let tmp = TempDir::new().unwrap();
        let not_a_repo = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        // Same GitRepo handle, but the command runs in a directory that is not
        // a work tree, so `git diff` exits non-zero.
        let tool = GitDiffTool::new(Arc::clone(&repo), not_a_repo.path());
        let ctx = ToolContext::new("t12");
        let result = tool
            .call(
                serde_json::json!({"from_ref": "HEAD~1", "to_ref": "HEAD"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.starts_with("git diff failed:"),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn diff_is_cancelled_before_it_starts() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitDiffTool::new(Arc::clone(&repo), tmp.path());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::new("t13").with_cancel_token(token);
        let err = tool
            .call(serde_json::json!({}), &ctx)
            .await
            .expect_err("a cancelled context must abort the call");
        assert!(
            err.to_string().contains("git_diff cancelled before start"),
            "unexpected error: {err}"
        );
    }
}
