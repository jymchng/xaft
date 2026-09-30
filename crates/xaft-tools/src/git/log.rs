//! `GitLogTool` — recent commit history via `agtrs-git::GitRepo`.
//!
//! Extended with optional filters: path, author, since, grep.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_u64;

/// Return recent commit history as a JSON array.
///
/// Each entry: `{ "sha", "author", "date", "subject" }`.
pub struct GitLogTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitLogTool {
    /// Create from a shared `GitRepo` and repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_log";
}

impl std::fmt::Debug for GitLogTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitLogTool").finish()
    }
}

#[async_trait]
impl Tool for GitLogTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Show recent commit history as a JSON array of \
         {sha, author, date, subject} entries. Supports optional filters: \
         path, author, since, grep."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum number of commits to return (default: 10)."
                },
                "path": {
                    "type": "string",
                    "description": "Limit history to commits that touched this path."
                },
                "author": {
                    "type": "string",
                    "description": "Filter commits by author name or email pattern."
                },
                "since": {
                    "type": "string",
                    "description": "Show commits more recent than this date (e.g. '2024-01-01', '1 week ago')."
                },
                "grep": {
                    "type": "string",
                    "description": "Filter commits whose message matches this pattern."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_log", skip_all)]
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

        let max = opt_u64(&input, "max").unwrap_or(10).clamp(1, 100) as usize;
        let path = input.get("path").and_then(|v| v.as_str());
        let author = input.get("author").and_then(|v| v.as_str());
        let since = input.get("since").and_then(|v| v.as_str());
        let grep = input.get("grep").and_then(|v| v.as_str());

        // Use git command directly when extra filters are needed
        if path.is_some() || author.is_some() || since.is_some() || grep.is_some() {
            return self.log_filtered(max, path, author, since, grep, ctx).await;
        }

        match self.repo.log(max).await {
            Ok(entries) => {
                let json = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into());
                Ok(ToolResult::ok(json, &ctx.tool_use_id))
            }
            Err(e) => Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        }
    }
}

impl GitLogTool {
    async fn log_filtered(
        &self,
        max: usize,
        path: Option<&str>,
        author: Option<&str>,
        since: Option<&str>,
        grep: Option<&str>,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let mut args: Vec<String> = vec![
            "log".to_string(),
            format!("-{}", max),
            "--format=%h|%an|%ai|%s".to_string(),
        ];

        if let Some(a) = author {
            args.push(format!("--author={}", a));
        }
        if let Some(s) = since {
            args.push(format!("--since={}", s));
        }
        if let Some(g) = grep {
            args.push(format!("--grep={}", g));
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
                format!("git log failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let entries: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(4, '|').collect();
                if parts.len() == 4 {
                    Some(serde_json::json!({
                        "sha": parts[0],
                        "author": parts[1],
                        "date": parts[2],
                        "subject": parts[3],
                    }))
                } else {
                    None
                }
            })
            .collect();

        let json = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
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
    }

    fn commit_empty(dir: &TempDir, msg: &str) {
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", msg])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn log_returns_commits() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        commit_empty(&tmp, "initial");
        commit_empty(&tmp, "second commit");
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"max": 5}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert!(arr.len() >= 2);
    }

    #[tokio::test]
    async fn log_with_path_filter() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        commit_empty(&tmp, "add a.txt");
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();
        Command::new("git")
            .args(["add", "b.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        commit_empty(&tmp, "add b.txt");

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"path": "a.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        // Only the commit that touched a.txt should appear
        assert_eq!(arr.len(), 1);
        assert!(
            arr[0]["subject"]
                .as_str()
                .unwrap_or("")
                .contains("add a.txt")
        );
    }

    #[tokio::test]
    async fn log_with_grep_filter() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        commit_empty(&tmp, "fix: resolve bug");
        commit_empty(&tmp, "feat: add feature");

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(serde_json::json!({"grep": "fix:"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["subject"].as_str().unwrap_or("").contains("fix:"));
    }

    #[tokio::test]
    async fn log_with_author_filter_selects_only_that_author() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        commit_empty(&tmp, "by the test user");
        Command::new("git")
            .args(["config", "user.email", "other@other.com"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Other"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        commit_empty(&tmp, "by somebody else");

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t4");
        let result = tool
            .call(serde_json::json!({"author": "other@other.com"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1, "only the other author's commit should match");
        assert_eq!(arr[0]["author"].as_str(), Some("Other"));
        assert!(arr[0]["sha"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(arr[0]["date"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[tokio::test]
    async fn log_with_since_filter_excludes_older_commits() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        Command::new("git")
            .args([
                "commit",
                "--allow-empty",
                "-m",
                "ancient",
                "--date=2001-02-03T04:05:06",
            ])
            .env("GIT_AUTHOR_DATE", "2001-02-03T04:05:06")
            .env("GIT_COMMITTER_DATE", "2001-02-03T04:05:06")
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "recent"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t5");
        let result = tool
            .call(serde_json::json!({"since": "2020-01-01"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1, "the 2001 commit is older than the cutoff");
        assert_eq!(arr[0]["subject"].as_str(), Some("recent"));
    }

    #[tokio::test]
    async fn log_filter_outside_a_repository_reports_git_failure() {
        let tmp = TempDir::new().unwrap();
        let not_a_repo = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), not_a_repo.path());
        let ctx = ToolContext::new("t6");
        let result = tool
            .call(serde_json::json!({"grep": "anything"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.starts_with("git log failed:"),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn log_max_is_clamped_to_the_documented_range() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        commit_empty(&tmp, "only commit");
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t7");

        // 0 clamps up to 1 rather than asking git for zero commits.
        let result = tool
            .call(serde_json::json!({"max": 0, "grep": "only"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1);

        // 5000 clamps down to 100.
        let result = tool
            .call(serde_json::json!({"max": 5000, "grep": "only"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[tokio::test]
    async fn log_is_cancelled_before_it_starts() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitLogTool::new(Arc::clone(&repo), tmp.path());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::new("t8").with_cancel_token(token);
        let err = tool
            .call(serde_json::json!({}), &ctx)
            .await
            .expect_err("a cancelled context must abort the call");
        assert!(
            err.to_string().contains("git_log cancelled before start"),
            "unexpected error: {err}"
        );
    }
}
