//! `RecallTool` — search project memory for relevant entries.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::manager::XaftMemoryManager;

/// Search project memory for facts, prior fixes, and architectural insights.
///
/// # Input schema
///
/// ```json
/// {
///   "query": "authentication service architecture",
///   "tags": ["auth"],
///   "limit": 5
/// }
/// ```
pub struct RecallTool {
    manager: Arc<XaftMemoryManager>,
}

impl RecallTool {
    pub fn new(manager: Arc<XaftMemoryManager>) -> Self {
        Self { manager }
    }

    const TOOL_NAME: &'static str = "recall";
}

impl std::fmt::Debug for RecallTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecallTool").finish()
    }
}

#[async_trait]
impl Tool for RecallTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Search project memory for previously stored facts, architectural insights, debugging \
         discoveries, and code conventions. Use this before starting work to check if relevant \
         knowledge exists from prior sessions."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query — keywords or description of what you're looking for."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional tag filter (e.g. [\"architecture\"])."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of results to return (default: 5)."
                }
            },
            "required": ["query"]
        })
    }

    #[instrument(name = "memory_recall", skip(self, _ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let query = input.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
            AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: "missing required field 'query'".into(),
            }
        })?;

        let results = self
            .manager
            .recall(query)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: e.to_string(),
            })?;

        if results.is_empty() {
            return Ok(ToolResult::ok(
                "No matching memories found.",
                &_ctx.tool_use_id,
            ));
        }

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            let tags = if result.entry.metadata.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", result.entry.metadata.tags.join(", "))
            };
            let agent = result
                .entry
                .metadata
                .source_agent
                .as_deref()
                .unwrap_or("unknown");
            let preview = if result.entry.content.len() <= 300 {
                &result.entry.content
            } else {
                &result.entry.content[..300]
            };
            output.push_str(&format!(
                "{}. {} (score: {:.2}, agent: {}){}\n",
                i + 1,
                preview,
                result.score,
                agent,
                tags
            ));
        }

        Ok(ToolResult::ok(output, &_ctx.tool_use_id))
    }
}
