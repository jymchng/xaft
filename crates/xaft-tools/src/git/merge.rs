//! `GitMergeTool` — merge a branch via `git merge`, `git merge --squash`, or `git rebase`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::require_str;

/// Merge another branch into the current branch.
pub struct GitMergeTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitMergeTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_merge";
}

impl std::fmt::Debug for GitMergeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitMergeTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitMergeTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Merge a branch into the current branch using merge, squash, or rebase strategy. \
         Returns JSON with {merged, branch, conflicts}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch to merge into the current branch."
                },
                "strategy": {
                    "type": "string",
                    "enum": ["merge", "squash", "rebase"],
                    "description": "Merge strategy: 'merge' (default), 'squash', or 'rebase'."
                },
                "message": {
                    "type": "string",
                    "description": "Optional commit message for the merge commit."
                }
            },
            "required": ["branch"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "git_merge", skip_all)]
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

        let branch = match require_str(Self::TOOL_NAME, &input, "branch") {
            Ok(b) => b,
            Err(e) => return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        };

        let strategy = input
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("merge");

        let message = input.get("message").and_then(|v| v.as_str());

        let output = match strategy {
            "rebase" => {
                tokio::process::Command::new("git")
                    .args(["rebase", branch])
                    .current_dir(&self.repo_path)
                    .output()
                    .await
            }
            "squash" => {
                let mut args: Vec<String> = vec![
                    "merge".to_string(),
                    "--squash".to_string(),
                    branch.to_string(),
                ];
                if let Some(m) = message {
                    args.push("-m".to_string());
                    args.push(m.to_string());
                }
                tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&self.repo_path)
                    .output()
                    .await
            }
            _ => {
                // Default: regular merge
                let mut args: Vec<String> = vec!["merge".to_string(), branch.to_string()];
                if let Some(m) = message {
                    args.push("-m".to_string());
                    args.push(m.to_string());
                }
                tokio::process::Command::new("git")
                    .args(&args)
                    .current_dir(&self.repo_path)
                    .output()
                    .await
            }
        }
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: e.to_string(),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

            // Check for conflict markers in output
            let combined = format!("{stdout}\n{stderr}");
            let has_conflicts = combined.to_lowercase().contains("conflict");

            if has_conflicts {
                // Collect conflicted files
                let conflict_output = tokio::process::Command::new("git")
                    .args(["diff", "--name-only", "--diff-filter=U"])
                    .current_dir(&self.repo_path)
                    .output()
                    .await
                    .ok();
                let conflicts_text = conflict_output
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
                let conflict_files: Vec<&str> =
                    conflicts_text.lines().filter(|l| !l.is_empty()).collect();
                return Ok(ToolResult::error(
                    serde_json::to_string_pretty(&serde_json::json!({
                        "merged": false,
                        "branch": branch,
                        "conflicts": conflict_files,
                        "message": stderr,
                    }))
                    .unwrap_or_else(|_| stderr),
                    &ctx.tool_use_id,
                ));
            }

            return Ok(ToolResult::error(
                format!("git merge failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let result = serde_json::json!({
            "merged": true,
            "branch": branch,
            "conflicts": [],
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
        std::fs::write(dir.path().join("main.txt"), "main content").unwrap();
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
        let tool = GitMergeTool::new(repo, tmp.path());
        assert!(tool.requires_confirmation());
    }

    #[tokio::test]
    async fn merge_merges_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);

        // Create feature branch with a new commit
        Command::new("git")
            .args(["checkout", "-b", "feat/test"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("feature.txt"), "feature content").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        // Return to main
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitMergeTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"branch": "feat/test"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["merged"].as_bool().unwrap(), true);
        assert!(tmp.path().join("feature.txt").exists());
    }

    #[tokio::test]
    async fn merge_missing_branch_errors() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitMergeTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
    }
}
