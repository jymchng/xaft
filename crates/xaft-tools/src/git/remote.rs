//! `GitRemoteTool` — list git remotes.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// List configured git remotes with their fetch and push URLs.
pub struct GitRemoteTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitRemoteTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_remote";
}

impl std::fmt::Debug for GitRemoteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRemoteTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitRemoteTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "List all configured git remotes. Returns JSON with a list of \
         {name, fetch_url, push_url} entries."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_remote", skip_all)]
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

        let output = tokio::process::Command::new("git")
            .args(["remote", "-v"])
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
                format!("git remote failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout);

        // Parse "name\turl (fetch|push)" lines; group by name
        let mut remotes: std::collections::HashMap<String, (Option<String>, Option<String>)> =
            std::collections::HashMap::new();

        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            // Format: "origin\thttps://... (fetch)"
            let parts: Vec<&str> = line.splitn(2, '\t').collect();
            if parts.len() < 2 {
                continue;
            }
            let name = parts[0].to_string();
            let rest = parts[1];
            if let Some(url_type) = rest.strip_suffix(')') {
                if let Some(url_part) = url_type.split_once(" (") {
                    let url = url_part.0.trim().to_string();
                    let kind = url_part.1.trim();
                    let entry = remotes.entry(name).or_insert((None, None));
                    if kind == "fetch" {
                        entry.0 = Some(url);
                    } else if kind == "push" {
                        entry.1 = Some(url);
                    }
                }
            }
        }

        let remote_list: Vec<serde_json::Value> = remotes
            .into_iter()
            .map(|(name, (fetch, push))| {
                serde_json::json!({
                    "name": name,
                    "fetch_url": fetch,
                    "push_url": push,
                })
            })
            .collect();

        let result = serde_json::json!({ "remotes": remote_list });
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
    async fn remote_empty_when_no_remote() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitRemoteTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let remotes = parsed["remotes"].as_array().unwrap();
        assert!(remotes.is_empty());
    }
}
