//! `RememberTool` — store facts, insights, and discoveries in project memory.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::manager::XaftMemoryManager;

/// Store a fact or insight in project memory for future recall.
///
/// # Input schema
///
/// ```json
/// {
///   "content": "The auth service uses JWT tokens with 1-hour expiry",
///   "tags": ["architecture", "auth"]
/// }
/// ```
pub struct RememberTool {
    manager: Arc<XaftMemoryManager>,
}

impl RememberTool {
    pub fn new(manager: Arc<XaftMemoryManager>) -> Self {
        Self { manager }
    }

    const TOOL_NAME: &'static str = "remember";
}

impl std::fmt::Debug for RememberTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberTool").finish()
    }
}

#[async_trait]
impl Tool for RememberTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Store a fact, architectural insight, debugging discovery, or code convention in project \
         memory. Use this to remember important information that should persist across sessions. \
         Tags help organize and filter memories later."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The fact or insight to remember. Be specific and concise."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tags for organizing this memory (e.g. [\"architecture\", \"auth\"])."
                }
            },
            "required": ["content"]
        })
    }

    #[instrument(name = "memory_remember", skip(self, _ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: "missing required field 'content'".into(),
            })?;

        if content.trim().is_empty() {
            return Ok(ToolResult::error(
                "Error: content cannot be empty",
                &_ctx.tool_use_id,
            ));
        }

        let tags: Vec<&str> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let id = self.manager.remember(content, &tags).await.map_err(|e| {
            AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: e.to_string(),
            }
        })?;

        let preview = if content.len() <= 200 {
            content
        } else {
            &content[..200]
        };

        Ok(ToolResult::ok(
            format!("Remembered (id: {}): {}", id, preview),
            &_ctx.tool_use_id,
        ))
    }
}
