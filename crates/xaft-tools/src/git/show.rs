//! `GitShowTool` — show a commit's details via `git show`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_bool;

/// Show details about a commit: metadata + changed files.
pub struct GitShowTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitShowTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_show";
}

impl std::fmt::Debug for GitShowTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitShowTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

/// Parse `git show --stat` output into a list of changed file entries.
fn parse_show_stat(stat: &str) -> Vec<serde_json::Value> {
    let mut files = Vec::new();
    for line in stat.lines() {
        // Lines like: " src/main.rs | 10 +++++-----"
        if let Some((path_part, change_part)) = line.split_once('|') {
            let path = path_part.trim().to_string();
            if path.is_empty() || path.contains("changed") {
                continue;
            }
            let change_str = change_part.trim();
            let additions = change_str.chars().filter(|&c| c == '+').count();
            let deletions = change_str.chars().filter(|&c| c == '-').count();
            // Extract the number before the +/- symbols
            let total: usize = change_str
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            // Use the total count distributed by ratio
            let (adds, dels) = if additions + deletions == 0 {
                (total, 0)
            } else {
                let a = (total * additions).saturating_div(additions + deletions);
                let d = total.saturating_sub(a);
                (a, d)
            };
            files.push(serde_json::json!({
                "path": path,
                "additions": adds,
                "deletions": dels,
            }));
        }
    }
    files
}

#[async_trait]
impl Tool for GitShowTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Show details about a git commit. Returns JSON with sha, author, email, date, \
         subject, body, and changed_files list."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Git ref (SHA, tag, branch) to show. Defaults to HEAD."
                },
                "stat_only": {
                    "type": "boolean",
                    "description": "If true, omit the diff body and only return metadata + stat."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_show", skip_all)]
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

        let git_ref = input.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");
        let stat_only = opt_bool(&input, "stat_only").unwrap_or(false);

        // Run git show with a custom format to get metadata
        let format = "--format=%H|%an|%ae|%ad|%s|%b";
        let mut args: Vec<&str> = vec!["show", format, "--stat"];
        if stat_only {
            args.push("--no-patch");
        }
        args.push(git_ref);

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
                format!("git show failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();

        // First line is our format string
        let header = lines.next().unwrap_or("");
        let parts: Vec<&str> = header.splitn(6, '|').collect();

        let (sha, author, email, date, subject, body) = if parts.len() >= 5 {
            (
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
                parts[3].to_string(),
                parts[4].to_string(),
                parts.get(5).unwrap_or(&"").to_string(),
            )
        } else {
            (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                header.to_string(),
                String::new(),
            )
        };

        // Remaining lines are the stat output
        let stat_text: String = lines.collect::<Vec<_>>().join("\n");
        let changed_files = parse_show_stat(&stat_text);

        let result = serde_json::json!({
            "sha": sha,
            "author": author,
            "email": email,
            "date": date,
            "subject": subject,
            "body": body.trim(),
            "changed_files": changed_files,
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
        std::fs::write(dir.path().join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(dir.path())
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn show_head_returns_commit_info() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitShowTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"ref": "HEAD"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert!(parsed.get("sha").is_some());
        assert!(parsed.get("author").is_some());
        assert!(parsed.get("subject").is_some());
        assert!(parsed.get("changed_files").is_some());
    }

    #[tokio::test]
    async fn show_defaults_to_head() {
        let tmp = TempDir::new().unwrap();
        init_repo(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitShowTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["subject"].as_str().unwrap_or(""), "initial commit");
    }
}
