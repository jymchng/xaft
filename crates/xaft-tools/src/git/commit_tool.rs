//! `GitCommitStagedTool` — commit staged changes via `git commit`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::{opt_bool, require_str};

/// Commit currently staged changes with an explicit message.
pub struct GitCommitStagedTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitCommitStagedTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_commit_staged";
}

impl std::fmt::Debug for GitCommitStagedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCommitStagedTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitCommitStagedTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Commit currently staged changes. Requires a non-empty commit message. \
         Returns JSON with {sha, subject, files_changed}. \
         Will fail if nothing is staged unless allow_empty=true."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Commit message (required, must not be empty)."
                },
                "allow_empty": {
                    "type": "boolean",
                    "description": "Allow committing even when nothing is staged."
                }
            },
            "required": ["message"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "git_commit_staged", skip_all)]
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

        let message = match require_str(Self::TOOL_NAME, &input, "message") {
            Ok(m) => m,
            Err(e) => return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        };

        if message.trim().is_empty() {
            return Ok(ToolResult::error(
                "git_commit_staged: commit message must not be empty".to_string(),
                &ctx.tool_use_id,
            ));
        }

        let allow_empty = opt_bool(&input, "allow_empty").unwrap_or(false);

        // Check if there is anything staged (unless allow_empty)
        if !allow_empty {
            let index_output = tokio::process::Command::new("git")
                .args(["diff", "--cached", "--name-only"])
                .current_dir(&self.repo_path)
                .output()
                .await
                .map_err(|e| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: e.to_string(),
                })?;

            let staged_files = String::from_utf8_lossy(&index_output.stdout);
            if staged_files.trim().is_empty() {
                return Ok(ToolResult::error(
                    "git_commit_staged: nothing staged to commit; use git_add first or set allow_empty=true".to_string(),
                    &ctx.tool_use_id,
                ));
            }
        }

        let mut args: Vec<String> =
            vec!["commit".to_string(), "-m".to_string(), message.to_string()];
        if allow_empty {
            args.push("--allow-empty".to_string());
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
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let reason = if stderr.is_empty() { stdout } else { stderr };
            return Ok(ToolResult::error(
                format!("git commit failed: {reason}"),
                &ctx.tool_use_id,
            ));
        }

        // Get the new HEAD SHA
        let sha = self
            .repo
            .head_sha()
            .await
            .unwrap_or_else(|_| "unknown".to_string());

        // Count changed files in the commit
        let diff_output = tokio::process::Command::new("git")
            .args(["diff-tree", "--no-commit-id", "-r", "--name-only", "HEAD"])
            .current_dir(&self.repo_path)
            .output()
            .await
            .ok();

        let files_count = diff_output
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .count()
            })
            .unwrap_or(0);

        let result = serde_json::json!({
            "sha": sha,
            "subject": message.lines().next().unwrap_or(message),
            "files_changed": files_count,
        });
        let json = serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".into());
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
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[test]
    fn requires_confirmation_is_true() {
        let tmp = TempDir::new().unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap_or_else(|_| {
            // Init first for open to succeed
            Command::new("git")
                .args(["init", "--initial-branch=main"])
                .current_dir(tmp.path())
                .output()
                .unwrap();
            GitRepo::open(tmp.path()).unwrap()
        }));
        let tool = GitCommitStagedTool::new(repo, tmp.path());
        assert!(tool.requires_confirmation());
    }

    #[tokio::test]
    async fn commit_rejects_empty_message() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCommitStagedTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"message": "   "}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn commit_rejects_when_nothing_staged() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCommitStagedTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"message": "test commit"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("nothing staged"));
    }

    #[tokio::test]
    async fn commit_creates_commit() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("new.txt"), "content").unwrap();
        Command::new("git")
            .args(["add", "new.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCommitStagedTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(serde_json::json!({"message": "add new file"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("sha").is_some());
        assert_eq!(parsed["subject"].as_str().unwrap(), "add new file");
    }
}
