//! `GitCheckoutFilesTool` — discard local changes to files via `git checkout`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Restore files to their state at a given ref, discarding local changes.
pub struct GitCheckoutFilesTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitCheckoutFilesTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_checkout_files";
}

impl std::fmt::Debug for GitCheckoutFilesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCheckoutFilesTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitCheckoutFilesTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Restore files to their state at a given ref, discarding local changes. \
         This is destructive — local modifications to the specified files will be lost. \
         Returns JSON with {restored}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "File paths to restore."
                },
                "ref": {
                    "type": "string",
                    "description": "Git ref to restore from (default: HEAD)."
                }
            },
            "required": ["paths"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "git_checkout_files", skip_all)]
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

        let paths: Vec<String> = match input.get("paths").and_then(|v| v.as_array()) {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            None => {
                return Ok(ToolResult::error(
                    "git_checkout_files: 'paths' is required".to_string(),
                    &ctx.tool_use_id,
                ));
            }
        };

        if paths.is_empty() {
            return Ok(ToolResult::error(
                "git_checkout_files: 'paths' must not be empty".to_string(),
                &ctx.tool_use_id,
            ));
        }

        let git_ref = input.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");

        let mut args: Vec<String> = vec![
            "checkout".to_string(),
            git_ref.to_string(),
            "--".to_string(),
        ];
        args.extend(paths.iter().cloned());

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
                format!("git checkout failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let result = serde_json::json!({ "restored": paths });
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
        std::fs::write(dir.path().join("file.txt"), "original").unwrap();
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
        let tool = GitCheckoutFilesTool::new(repo, tmp.path());
        assert!(tool.requires_confirmation());
    }

    #[tokio::test]
    async fn checkout_restores_file() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        std::fs::write(tmp.path().join("file.txt"), "modified").unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCheckoutFilesTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(
                serde_json::json!({"paths": ["file.txt"], "ref": "HEAD"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);

        let content = std::fs::read_to_string(tmp.path().join("file.txt")).unwrap();
        assert_eq!(content, "original");
    }

    #[tokio::test]
    async fn checkout_missing_paths_errors() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitCheckoutFilesTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
    }
}
