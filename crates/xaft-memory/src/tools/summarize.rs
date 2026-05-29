//! `SummarizeMemoryTool` — compress old memories into durable summaries.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use crate::manager::XaftMemoryManager;

/// Compress project memories into a durable summary.
///
/// Lists all project memories (optionally filtered by tags) and returns them
/// as a formatted summary suitable for compression by the LLM.
///
/// # Input schema
///
/// ```json
/// {
///   "tags": ["architecture"],
///   "max_entries": 20
/// }
/// ```
pub struct SummarizeMemoryTool {
    manager: Arc<XaftMemoryManager>,
}

impl SummarizeMemoryTool {
    pub fn new(manager: Arc<XaftMemoryManager>) -> Self {
        Self { manager }
    }

    const TOOL_NAME: &'static str = "summarize_memory";
}

impl std::fmt::Debug for SummarizeMemoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummarizeMemoryTool").finish()
    }
}

#[async_trait]
impl Tool for SummarizeMemoryTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "List and summarize project memories, optionally filtered by tags. Use this to review \
         what the project knows before planning a compression pass. Returns a formatted summary \
         of all matching memories."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional tag filter (e.g. [\"architecture\"]). Empty = all."
                },
                "max_entries": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum entries to include (default: 20)."
                }
            }
        })
    }

    #[instrument(name = "memory_summarize", skip(self, _ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let tags: Option<Vec<String>> = input.get("tags").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

        let max_entries = input
            .get("max_entries")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;

        let entries = self
            .manager
            .list_project_memories(tags.clone())
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.into(),
                reason: e.to_string(),
            })?;

        if entries.is_empty() {
            return Ok(ToolResult::ok(
                "No memories found to summarize.",
                &_ctx.tool_use_id,
            ));
        }

        let total = entries.len();
        let mut output = format!("## Project Memory Summary ({} entries)\n\n", total);

        for (i, entry) in entries.iter().take(max_entries).enumerate() {
            let tags_str = if entry.metadata.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", entry.metadata.tags.join(", "))
            };
            let preview = if entry.content.len() <= 200 {
                &entry.content
            } else {
                &entry.content[..200]
            };
            output.push_str(&format!("{}. {}{}\n", i + 1, preview, tags_str));
        }

        if total > max_entries {
            output.push_str(&format!(
                "\n... and {} more entries not shown.\n",
                total - max_entries
            ));
        }

        Ok(ToolResult::ok(output, &_ctx.tool_use_id))
    }
}
