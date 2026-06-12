//! `GitCreateBranchTool` — create a new git branch.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::{opt_bool, require_str};

/// Validate that a branch name is safe.
fn validate_branch_name(name: &str) -> Result<(), String> {
    if name.contains(' ') {
        return Err(format!("branch name '{name}' contains spaces"));
    }
    if name.ends_with(".git") {
        return Err(format!("branch name '{name}' ends with '.git'"));
    }
    if name.contains("//") {
        return Err(format!("branch name '{name}' contains '//'"));
    }
    if name.is_empty() {
        return Err("branch name must not be empty".to_string());
    }
    Ok(())
}

/// Create a new branch, optionally checking it out immediately.
pub struct GitCreateBranchTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitCreateBranchTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_create_branch";
}

impl std::fmt::Debug for GitCreateBranchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCreateBranchTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitCreateBranchTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a new git branch. Optionally check it out. Rejects invalid branch names \
         (spaces, .git suffix, //). Returns JSON with {created, name, checked_out}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Branch name to create."
                },
                "from_ref": {
                    "type": "string",
                    "description": "Base ref to create branch from (default: HEAD)."
                },
                "checkout": {
                    "type": "boolean",
                    "description": "If true, check out the new branch after creation (default: true)."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    #[instrument(name = "git_create_branch", skip_all)]
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

        let name = match require_str(Self::TOOL_NAME, &input, "name") {
            Ok(n) => n,
            Err(e) => return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        };

        if let Err(e) = validate_branch_name(name) {
            return Ok(ToolResult::error(
                format!("git_create_branch: invalid branch name: {e}"),
                &ctx.tool_use_id,
            ));
        }

        let from_ref = input.get("from_ref").and_then(|v| v.as_str());
        let checkout = opt_bool(&input, "checkout").unwrap_or(true);

        let output = if checkout {
            let mut args: Vec<String> =
                vec!["checkout".to_string(), "-b".to_string(), name.to_string()];
            if let Some(r) = from_ref {
                args.push(r.to_string());
            }
            tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&self.repo_path)
                .output()
                .await
        } else {
            let mut args: Vec<String> = vec!["branch".to_string(), name.to_string()];
            if let Some(r) = from_ref {
                args.push(r.to_string());
            }
            tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&self.repo_path)
                .output()
                .await
        }
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: e.to_string(),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(ToolResult::error(
                format!("git branch creation failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let result = serde_json::json!({
            "created": true,
            "name": name,
            "checked_out": checkout,
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
    fn validate_branch_name_rejects_spaces() {
        assert!(validate_branch_name("my branch").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_git_suffix() {
        assert!(validate_branch_name("feature.git").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_double_slash() {
        assert!(validate_branch_name("feat//x").is_err());
    }

    #[test]
    fn validate_branch_name_accepts_valid() {
        assert!(validate_branch_name("feat/new-feature").is_ok());
        assert!(validate_branch_name("fix/bug-123").is_ok());
    }

    #[tokio::test]
    async fn create_branch_creates_new_branch() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCreateBranchTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(
                serde_json::json!({"name": "feat/my-feature", "checkout": false}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);

        // Verify the branch exists
        let branches = Command::new("git")
            .args(["branch", "--list", "feat/my-feature"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let branches_str = String::from_utf8_lossy(&branches.stdout);
        assert!(branches_str.contains("feat/my-feature"));
    }

    #[tokio::test]
    async fn create_branch_rejects_invalid_name() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCreateBranchTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"name": "my bad branch"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
    }
}
