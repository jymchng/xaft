//! `GitLogTool` — recent commit history via `agtrs-git::GitRepo`.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::error::opt_u64;

/// Return recent commit history as a JSON array.
///
/// Each entry: `{ "sha", "author", "date", "subject" }`.
pub struct GitLogTool {
    repo: Arc<GitRepo>,
}

impl GitLogTool {
    /// Create from a shared `GitRepo`.
    pub fn new(repo: Arc<GitRepo>) -> Self {
        Self { repo }
    }

    const TOOL_NAME: &'static str = "git_log";
}

impl std::fmt::Debug for GitLogTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitLogTool").finish()
    }
}

#[async_trait]
impl Tool for GitLogTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Show recent commit history as a JSON array of \
         {sha, author, date, subject} entries."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum number of commits to return (default: 10)."
                }
            },
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_log", skip_all)]
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

        let max = opt_u64(&input, "max").unwrap_or(10).clamp(1, 100) as usize;

        match self.repo.log(max).await {
            Ok(entries) => {
                let json = serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into());
                Ok(ToolResult::ok(json, &ctx.tool_use_id))
            }
            Err(e) => Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        }
    }
}
