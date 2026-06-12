//! `GitStashTool` — save changes to the stash via `git stash push`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_bool;

/// Save working-tree changes to the stash.
pub struct GitStashTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitStashTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_stash";
}

impl std::fmt::Debug for GitStashTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitStashTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitStashTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Save current working-tree changes to the stash. Returns JSON with {stash_ref, message}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Optional stash message."
                },
                "include_untracked": {
                    "type": "boolean",
                    "description": "Include untracked files in the stash."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "git_stash", skip_all)]
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

        let message = input.get("message").and_then(|v| v.as_str());
        let include_untracked = opt_bool(&input, "include_untracked").unwrap_or(false);

        let mut args: Vec<String> = vec!["stash".to_string(), "push".to_string()];
        if include_untracked {
            args.push("-u".to_string());
        }
        if let Some(msg) = message {
            args.push("-m".to_string());
            args.push(msg.to_string());
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
                format!("git stash push failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // The message defaults to what git says
        let stash_message = message
            .map(|m| m.to_string())
            .unwrap_or_else(|| stdout.lines().next().unwrap_or("WIP stash").to_string());

        let result = serde_json::json!({
            "stash_ref": "stash@{0}",
            "message": stash_message,
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

    #[tokio::test]
    async fn stash_saves_changes() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("file.txt"), "modified").unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitStashTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"message": "WIP: test"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("stash_ref").is_some());

        // Verify working tree is clean
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_str = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_str.trim().is_empty(),
            "working tree should be clean after stash"
        );
    }
}
