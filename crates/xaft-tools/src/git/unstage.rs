//! `GitUnstageTool` — unstage files via `git restore --staged`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_bool;

/// Unstage previously staged files.
pub struct GitUnstageTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitUnstageTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_unstage";
}

impl std::fmt::Debug for GitUnstageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitUnstageTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitUnstageTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Unstage staged files (git restore --staged). Specify paths or set all=true \
         to unstage everything. Returns JSON with unstaged_count."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "File paths to unstage."
                },
                "all": {
                    "type": "boolean",
                    "description": "If true, unstage all staged changes."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "git_unstage", skip_all)]
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

        let all = opt_bool(&input, "all").unwrap_or(false);
        let paths: Vec<String> = input
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        if !all && paths.is_empty() {
            return Ok(ToolResult::error(
                "git_unstage: either 'paths' or 'all' must be provided".to_string(),
                &ctx.tool_use_id,
            ));
        }

        let mut args: Vec<String> = vec!["restore".to_string(), "--staged".to_string()];
        if all {
            args.push(".".to_string());
        } else {
            args.extend(paths.iter().cloned());
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
                format!("git restore --staged failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let result = serde_json::json!({
            "unstaged_count": if all { 0 } else { paths.len() },
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

    #[tokio::test]
    async fn unstage_removes_from_index() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("test.txt"), "hello").unwrap();
        // Stage the file first
        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        // Verify it is staged
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_str = String::from_utf8_lossy(&status.stdout);
        assert!(status_str.contains('A'), "file should be staged");

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitUnstageTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"paths": ["test.txt"]}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);

        // Verify it is unstaged now
        let status2 = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let status_str2 = String::from_utf8_lossy(&status2.stdout);
        assert!(
            status_str2.contains("??"),
            "file should be untracked after unstage"
        );
    }
}
