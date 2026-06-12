//! `GitPushTool` — push to a remote via `git push`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_bool;

/// Push commits to a remote repository.
pub struct GitPushTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitPushTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_push";
}

impl std::fmt::Debug for GitPushTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitPushTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitPushTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Push commits to a remote. Always requires confirmation. \
         Refuses to force-push to main or master unless remote+branch are explicit. \
         Returns JSON with {pushed, remote, branch}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "remote": {
                    "type": "string",
                    "description": "Remote name (default: origin)."
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name to push. Defaults to current branch."
                },
                "force": {
                    "type": "boolean",
                    "description": "Force push (--force). Refused on main/master."
                },
                "set_upstream": {
                    "type": "boolean",
                    "description": "Set the upstream tracking reference (-u)."
                }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "git_push", skip_all)]
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

        let remote = input
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");
        let branch = input.get("branch").and_then(|v| v.as_str());
        let force = opt_bool(&input, "force").unwrap_or(false);
        let set_upstream = opt_bool(&input, "set_upstream").unwrap_or(false);

        // Determine the actual branch we're about to push
        let push_branch = if let Some(b) = branch {
            b.to_string()
        } else {
            self.repo
                .current_branch()
                .await
                .unwrap_or_else(|_| "HEAD".to_string())
        };

        // Guard: refuse to force-push to main or master
        if force && (push_branch == "main" || push_branch == "master") {
            return Ok(ToolResult::error(
                format!(
                    "git_push: refusing to force-push to protected branch '{push_branch}'. \
                     Provide both remote and branch explicitly to override."
                ),
                &ctx.tool_use_id,
            ));
        }

        let mut args: Vec<String> = vec!["push".to_string()];
        if force {
            args.push("--force".to_string());
        }
        if set_upstream {
            args.push("-u".to_string());
        }
        args.push(remote.to_string());
        if !push_branch.is_empty() && push_branch != "HEAD" {
            args.push(push_branch.clone());
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
                format!("git push failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let result = serde_json::json!({
            "pushed": true,
            "remote": remote,
            "branch": push_branch,
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
        Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitPushTool::new(repo, tmp.path());
        assert!(tool.requires_confirmation());
    }

    #[tokio::test]
    async fn push_refuses_force_to_main() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitPushTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(
                serde_json::json!({"remote": "origin", "branch": "main", "force": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refusing to force-push"));
    }

    #[tokio::test]
    async fn push_refuses_force_to_master() {
        let tmp = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--initial-branch=master"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitPushTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"branch": "master", "force": true}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("refusing to force-push"));
    }
}
