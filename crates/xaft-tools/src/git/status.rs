//! `GitStatusTool` — show working-tree status via `agtrs-git::GitRepo`.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Return working-tree status as a JSON array.
///
/// Each entry: `{ "path": "src/main.rs", "status": "M", "label": "Modified" }`.
pub struct GitStatusTool {
    repo: Arc<GitRepo>,
}

impl GitStatusTool {
    /// Create from a shared `GitRepo`.
    pub fn new(repo: Arc<GitRepo>) -> Self {
        Self { repo }
    }

    const TOOL_NAME: &'static str = "git_status";
}

impl std::fmt::Debug for GitStatusTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitStatusTool").finish()
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Show working-tree git status. Returns JSON array of \
         {path, status, label} entries (M=Modified, A=Added, D=Deleted, \
         ?=Untracked, etc.)."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    #[instrument(name = "git_status", skip_all)]
    async fn call(
        &self,
        _input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before start", Self::TOOL_NAME),
            });
        }

        match self.repo.status().await {
            Ok(entries) => {
                if entries.is_empty() {
                    Ok(ToolResult::ok(
                        "[]  (nothing to commit — working tree clean)",
                        &ctx.tool_use_id,
                    ))
                } else {
                    let json =
                        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into());
                    Ok(ToolResult::ok(json, &ctx.tool_use_id))
                }
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
    }

    fn commit_empty(dir: &TempDir) {
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn clean_repo_returns_empty() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        commit_empty(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitStatusTool::new(repo);
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("clean") || result.content.contains("[]"));
    }

    #[tokio::test]
    async fn modified_file_appears_in_status() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        // Create file and commit it
        std::fs::write(tmp.path().join("foo.txt"), "original").unwrap();
        Command::new("git")
            .args(["add", "foo.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        commit_empty(&tmp);
        // Modify
        std::fs::write(tmp.path().join("foo.txt"), "modified").unwrap();
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitStatusTool::new(repo);
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("foo.txt"));
    }
}
