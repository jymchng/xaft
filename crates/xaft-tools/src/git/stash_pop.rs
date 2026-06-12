//! `GitStashPopTool` — restore stashed changes via `git stash pop`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_u64;

/// Restore stashed changes (potentially causing merge conflicts).
pub struct GitStashPopTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitStashPopTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_stash_pop";
}

impl std::fmt::Debug for GitStashPopTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitStashPopTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitStashPopTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Restore stashed changes (git stash pop). May cause merge conflicts. \
         Returns JSON with {files_restored}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "index": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Stash index to pop (default: 0, i.e. stash@{0})."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "git_stash_pop", skip_all)]
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

        let index = opt_u64(&input, "index").unwrap_or(0);
        let stash_ref = format!("stash@{{{}}}", index);

        let output = tokio::process::Command::new("git")
            .args(["stash", "pop", &stash_ref])
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
                format!("git stash pop failed: {reason}"),
                &ctx.tool_use_id,
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let files_restored: Vec<&str> = stdout
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("Dropped"))
            .collect();

        let result = serde_json::json!({
            "files_restored": files_restored,
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
        std::fs::write(dir.path().join("file.txt"), "initial").unwrap();
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

    #[test]
    fn requires_confirmation_is_true() {
        let tmp = TempDir::new().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitStashPopTool::new(repo, tmp.path());
        assert!(tool.requires_confirmation());
    }

    #[tokio::test]
    async fn stash_pop_restores_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("file.txt"), "modified").unwrap();
        // Stash it
        Command::new("git")
            .args(["stash"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitStashPopTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"index": 0}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);

        let content = std::fs::read_to_string(tmp.path().join("file.txt")).unwrap();
        assert_eq!(content, "modified");
    }
}
