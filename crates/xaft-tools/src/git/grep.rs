//! `GitGrepTool` — search tracked file contents via `git grep`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::{opt_bool, opt_u64, require_str};

/// Search tracked file contents via `git grep`.
pub struct GitGrepTool {
    repo: Arc<GitRepo>,
    repo_path: PathBuf,
}

impl GitGrepTool {
    /// Create from a shared `GitRepo` and the repo root path.
    pub fn new(repo: Arc<GitRepo>, repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            repo_path: repo_path.into(),
        }
    }

    const TOOL_NAME: &'static str = "git_grep";
}

impl std::fmt::Debug for GitGrepTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitGrepTool")
            .field("repo_path", &self.repo_path)
            .finish()
    }
}

#[async_trait]
impl Tool for GitGrepTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Search tracked file contents using git grep. Returns JSON array of \
         {file, line_number, content} matches."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Pattern to search for."
                },
                "path_prefix": {
                    "type": "string",
                    "description": "Limit search to files under this path prefix."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether the search is case-sensitive (default: true)."
                },
                "use_regex": {
                    "type": "boolean",
                    "description": "Treat pattern as an extended regex (default: false)."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "description": "Maximum number of results to return (default: 100)."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_grep", skip_all)]
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

        let pattern = match require_str(Self::TOOL_NAME, &input, "pattern") {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        };

        let path_prefix = input
            .get("path_prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let case_sensitive = opt_bool(&input, "case_sensitive").unwrap_or(true);
        let use_regex = opt_bool(&input, "use_regex").unwrap_or(false);
        let max_results = opt_u64(&input, "max_results").unwrap_or(100).clamp(1, 1000) as usize;

        let mut args: Vec<String> = vec!["grep".to_string(), "-n".to_string()];

        if !case_sensitive {
            args.push("-i".to_string());
        }
        if use_regex {
            args.push("-E".to_string());
        }

        args.push(pattern.to_string());

        if !path_prefix.is_empty() {
            args.push("--".to_string());
            args.push(path_prefix.to_string());
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

        // Exit code 1 means no matches (not an error)
        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(ToolResult::error(
                format!("git grep failed: {stderr}"),
                &ctx.tool_use_id,
            ));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut matches = Vec::new();

        for line in text.lines().take(max_results) {
            if line.is_empty() {
                continue;
            }
            // Format: "file:line_number:content"
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() >= 3 {
                let file = parts[0].to_string();
                let line_number: u64 = parts[1].parse().unwrap_or(0);
                let content = parts[2].to_string();
                matches.push(serde_json::json!({
                    "file": file,
                    "line_number": line_number,
                    "content": content,
                }));
            }
        }

        let json = serde_json::to_string_pretty(&matches).unwrap_or_else(|_| "[]".into());
        Ok(ToolResult::ok(json, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo_with_content(dir: &TempDir) {
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
        std::fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
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
    async fn grep_finds_pattern() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(serde_json::json!({"pattern": "fn main"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["content"].as_str().unwrap_or("").contains("fn main"));
    }

    #[tokio::test]
    async fn grep_no_match_returns_empty() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"pattern": "ZZZNOMATCH"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert!(arr.is_empty());
    }

    #[tokio::test]
    async fn grep_missing_pattern_errors() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t3");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn grep_is_case_insensitive_when_asked() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t4");

        let sensitive = tool
            .call(
                serde_json::json!({"pattern": "HELLO", "case_sensitive": true}),
                &ctx,
            )
            .await
            .unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&sensitive.content).unwrap();
        assert!(arr.is_empty(), "case-sensitive search must not match");

        let insensitive = tool
            .call(
                serde_json::json!({"pattern": "HELLO", "case_sensitive": false}),
                &ctx,
            )
            .await
            .unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&insensitive.content).unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["content"].as_str().unwrap_or("").contains("hello"));
    }

    #[tokio::test]
    async fn grep_treats_the_pattern_as_a_regex_when_asked() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t5");

        // Without -E the pattern is a literal, so `fn\s+main` finds nothing.
        let literal = tool
            .call(serde_json::json!({"pattern": "fn\\s+main"}), &ctx)
            .await
            .unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&literal.content).unwrap();
        assert!(arr.is_empty(), "the default search is not a regex");

        let regex = tool
            .call(
                serde_json::json!({"pattern": "fn\\s+main", "use_regex": true}),
                &ctx,
            )
            .await
            .unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&regex.content).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line_number"].as_u64(), Some(1));
        assert_eq!(arr[0]["file"].as_str(), Some("main.rs"));
    }

    #[tokio::test]
    async fn grep_path_prefix_limits_the_search_to_one_directory() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub/nested.rs"), "fn helper() {}\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add nested"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t6");
        let result = tool
            .call(
                serde_json::json!({"pattern": "fn ", "use_regex": true, "path_prefix": "sub"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 1, "only sub/nested.rs is under the prefix");
        assert!(arr[0]["file"].as_str().unwrap_or("").starts_with("sub/"));
    }

    #[tokio::test]
    async fn grep_max_results_truncates_the_match_list() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        std::fs::write(tmp.path().join("many.rs"), "needle\nneedle\nneedle\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add many"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let ctx = ToolContext::new("t7");
        let result = tool
            .call(
                serde_json::json!({"pattern": "needle", "max_results": 2}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "error: {}", result.content);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.content).unwrap();
        assert_eq!(arr.len(), 2, "max_results must cap the returned matches");
    }

    #[tokio::test]
    async fn grep_outside_a_repository_reports_git_failure() {
        let tmp = TempDir::new().unwrap();
        let not_a_repo = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), not_a_repo.path());
        let ctx = ToolContext::new("t8");
        let result = tool
            .call(serde_json::json!({"pattern": "anything"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.content.starts_with("git grep failed:"),
            "unexpected content: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn grep_is_cancelled_before_it_starts() {
        let tmp = TempDir::new().unwrap();
        init_repo_with_content(&tmp);
        let repo = Arc::new(GitRepo::open(tmp.path()).unwrap());
        let tool = GitGrepTool::new(Arc::clone(&repo), tmp.path());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = ToolContext::new("t9").with_cancel_token(token);
        let err = tool
            .call(serde_json::json!({"pattern": "fn"}), &ctx)
            .await
            .expect_err("a cancelled context must abort the call");
        assert!(
            err.to_string().contains("git_grep cancelled before start"),
            "unexpected error: {err}"
        );
    }
}
