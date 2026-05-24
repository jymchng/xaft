//! `GitDiffTool` — show unified diff since HEAD via `agtrs-git::GitRepo`.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_git::GitRepo;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Return the unified diff of all changes since HEAD.
pub struct GitDiffTool {
    repo: Arc<GitRepo>,
}

impl GitDiffTool {
    /// Create from a shared `GitRepo`.
    pub fn new(repo: Arc<GitRepo>) -> Self {
        Self { repo }
    }

    const TOOL_NAME: &'static str = "git_diff";
}

impl std::fmt::Debug for GitDiffTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitDiffTool").finish()
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Show the unified diff of all uncommitted changes (`git diff HEAD`). \
         Returns the raw unified diff text."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    #[instrument(name = "git_diff", skip_all)]
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

        match self.repo.diff_head().await {
            Ok(diff) => {
                let content = if diff.unified.is_empty() {
                    "No changes since HEAD.".to_string()
                } else {
                    diff.unified
                };
                Ok(ToolResult::ok(content, &ctx.tool_use_id))
            }
            Err(e) => Ok(ToolResult::error(e.to_string(), &ctx.tool_use_id)),
        }
    }
}
