//! `GitBranchTool` — list git branches.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_bool;

/// List local and/or remote branches.
pub struct GitBranchTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitBranchTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_branch";
}

impl std::fmt::Debug for GitBranchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitBranchTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitBranchTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "List git branches. Returns JSON with current branch and list of all branches \
         including whether they are current, remote, and their upstream."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "all": {
                    "type": "boolean",
                    "description": "If true, include remote-tracking branches."
                },
                "verbose": {
                    "type": "boolean",
                    "description": "If true, include upstream tracking info."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_branch", skip_all)]
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

        let include_all = opt_bool(&input, "all").unwrap_or(false);

        // Get current branch
        let current_branch = self
            .repo
            .current_branch()
            .await
            .unwrap_or_else(|_| String::from("HEAD"));

        // List local branches with format
        let local_format = "--format=%(refname:short)|%(HEAD)|%(upstream:short)";
        let mut local_args = vec!["branch", local_format];
        if include_all {
            local_args.push("-a");
        }

        let local_output = tokio::process::Command::new("git")
            .args(&local_args)
            .current_dir(&self.repo_path)
            .output()
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: e.to_string(),
            })?;

        let mut branches = Vec::new();

        if local_output.status.success() {
            let text = String::from_utf8_lossy(&local_output.stdout);
            for line in text.lines() {
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                let name = parts.first().copied().unwrap_or("").to_string();
                let is_current_marker = parts.get(1).copied().unwrap_or("") == "*";
                let upstream = parts.get(2).copied().unwrap_or("").to_string();
                let is_remote = name.starts_with("remotes/") || name.starts_with("origin/");

                branches.push(serde_json::json!({
                    "name": name,
                    "is_current": is_current_marker || name == current_branch,
                    "upstream": if upstream.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(upstream) },
                    "is_remote": is_remote,
                }));
            }
        }

        let result = serde_json::json!({
            "current": current_branch,
            "branches": branches,
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
    async fn branch_lists_current() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitBranchTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("current").is_some());
        assert!(parsed.get("branches").is_some());
        let current = parsed["current"].as_str().unwrap_or("");
        assert!(!current.is_empty());
    }

    #[tokio::test]
    async fn branch_current_marked_in_list() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitBranchTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let branches = parsed["branches"].as_array().unwrap();
        let current_count = branches
            .iter()
            .filter(|b| b["is_current"].as_bool().unwrap_or(false))
            .count();
        assert_eq!(current_count, 1);
    }
}
