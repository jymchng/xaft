//! `GitBlameTool` — show per-line blame annotations via `agtrs-git::GitRepo`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::{opt_u64, require_str};

/// Show blame annotations for a file, optionally filtered to a line range.
pub struct GitBlameTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitBlameTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_blame";
}

impl std::fmt::Debug for GitBlameTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitBlameTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitBlameTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Show per-line blame annotations for a file. Returns a JSON array of \
         {line_number, sha, author, date, content} entries. Optionally filter by \
         start_line and end_line."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to repo root."
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line to include (1-indexed, inclusive)."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to include (1-indexed, inclusive)."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_blame", skip_all)]
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

        let path = match require_str(Self::TOOL_NAME, &input, "path") {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        };

        let start_line = opt_u64(&input, "start_line").map(|v| v as usize);
        let end_line = opt_u64(&input, "end_line").map(|v| v as usize);

        match self.repo.blame(path).await {
            Ok(entries) => {
                let filtered: Vec<_> = entries
                    .into_iter()
                    .filter(|e| {
                        let in_start = start_line.map_or(true, |s| e.line_number >= s);
                        let in_end = end_line.map_or(true, |en| e.line_number <= en);
                        in_start && in_end
                    })
                    .map(|e| {
                        serde_json::json!({
                            "line_number": e.line_number,
                            "sha": e.sha,
                            "author": e.author,
                            "date": e.summary,
                            "content": e.content,
                        })
                    })
                    .collect();

                let json = serde_json::to_string_pretty(&filtered).unwrap_or_else(|_| "[]".into());
                Ok(ToolResult::ok(json, &ctx.tool_use_id))
            }
            Err(e) => Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo_with_file(dir: &TempDir) {
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
        std::fs::write(dir.path().join("README.md"), "# Hello\nWorld\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
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
    async fn blame_returns_entries() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_file(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitBlameTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"path": "README.md"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let arr = parsed.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0].get("line_number").is_some());
        assert!(arr[0].get("sha").is_some());
        assert!(arr[0].get("author").is_some());
    }

    #[tokio::test]
    async fn blame_filters_by_line_range() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_file(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitBlameTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(
                serde_json::json!({"path": "README.md", "start_line": 1, "end_line": 1}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line_number"], 1);
    }

    #[tokio::test]
    async fn blame_missing_path_returns_error() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_file(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitBlameTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t3");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
    }
}
