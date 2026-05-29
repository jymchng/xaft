//! `ForgetTool` — delete stale or incorrect memories.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::manager::XaftMemoryManager;

/// Delete a memory entry by its ID.
///
/// # Input schema
///
/// ```json
/// {
///   "id": "memory-id-to-delete"
/// }
/// ```
pub struct ForgetTool {
    manager: Arc<XaftMemoryManager>,
}

impl ForgetTool {
    pub fn new(manager: Arc<XaftMemoryManager>) -> Self {
        Self { manager }
    }

    const TOOL_NAME: &'static str = "forget";
}

impl std::fmt::Debug for ForgetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgetTool").finish()
    }
}

#[async_trait]
impl Tool for ForgetTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delete a memory entry that is stale, incorrect, or no longer relevant. Use `recall` \
         first to find the ID of the entry you want to forget."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The ID of the memory entry to delete."
                }
            },
            "required": ["id"]
        })
    }

    #[instrument(name = "memory_forget", skip(self, _ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let id_str =
            input
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.into(),
                    reason: "missing required field 'id'".into(),
                })?;

        let id = agtrs_memory::MemoryId::from(id_str);
        let deleted = self
            .manager
            .forget(&id)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: e.to_string(),
            })?;

        if deleted {
            Ok(ToolResult::ok(
                format!("Forgot memory: {}", id_str),
                &_ctx.tool_use_id,
            ))
        } else {
            Ok(ToolResult::error(
                format!("Memory not found: {}", id_str),
                &_ctx.tool_use_id,
            ))
        }
    }
}
