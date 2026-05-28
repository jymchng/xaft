//! `HandoffTool` — generic tool for inter-agent delegation.
//!
//! Injected into every agent in a dynamic workflow so agents can hand off
//! to each other via `handoff_to_agent`.

use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::team::HandoffAgentStore;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

// ── HandoffTool ───────────────────────────────────────────────────────────────

/// Tool injected into agents to delegate work to another agent.
///
/// When the LLM calls `handoff_to_agent`, this tool writes the target and
/// reason into the shared [`HandoffAgentStore`] so [`HandoffOrchestrator`]
/// can switch agents after the current turn finishes.
///
/// [`HandoffOrchestrator`]: agtrs_runtime::team::HandoffOrchestrator
pub struct HandoffTool {
    pub(crate) store: Arc<HandoffAgentStore>,
    /// Allowed target agent names. Empty = any registered agent.
    pub(crate) allowed_targets: Vec<String>,
    /// Shared flag — set to `true` when this tool fires so the owning agent's
    /// `before_llm_call` can abort the next LLM call, preventing loops.
    pub(crate) triggered: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl HandoffTool {
    /// Create with explicit allowed targets. Pass empty vec to allow any target.
    pub fn new(store: Arc<HandoffAgentStore>, allowed_targets: Vec<String>) -> Self {
        Self {
            store,
            allowed_targets,
            triggered: None,
        }
    }

    /// Create with a stop flag shared with the owning agent.
    ///
    /// When this tool fires the flag is set to `true`. The agent should check
    /// this flag in `before_llm_call` and return an error to abort the run.
    pub fn new_with_flag(
        store: Arc<HandoffAgentStore>,
        allowed_targets: Vec<String>,
        flag: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            store,
            allowed_targets,
            triggered: Some(flag),
        }
    }
}

impl std::fmt::Debug for HandoffTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoffTool")
            .field("allowed_targets", &self.allowed_targets)
            .finish()
    }
}

#[async_trait::async_trait]
impl Tool for HandoffTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "handoff_to_agent"
    }

    fn description(&self) -> &str {
        "Transfer the conversation to another agent when the current task \
         is better handled by a specialist. Provide a concise reason."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "Name of the agent to hand off to."
                },
                "reason": {
                    "type": "string",
                    "description": "Why this handoff is needed. \
                                    The next agent receives this as context."
                }
            },
            "required": ["target_agent", "reason"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let target = input["target_agent"].as_str().unwrap_or("").to_string();
        let reason = input["reason"]
            .as_str()
            .unwrap_or("Agent handoff")
            .to_string();

        if target.is_empty() {
            return Ok(ToolResult::ok(
                "handoff failed: target_agent is required".to_string(),
                &ctx.tool_use_id,
            ));
        }

        // Validate against allowed_targets when the list is non-empty.
        if !self.allowed_targets.is_empty() && !self.allowed_targets.contains(&target) {
            return Ok(ToolResult::ok(
                format!(
                    "handoff to '{target}' not permitted. Allowed: {:?}",
                    self.allowed_targets
                ),
                &ctx.tool_use_id,
            ));
        }

        let conv_id = ctx
            .state
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !conv_id.is_empty() {
            self.store.set_active_agent(&conv_id, &target).await;
            self.store.set_pending_summary(&conv_id, &reason).await;
        }

        // Signal the owning agent to terminate after this tool result.
        if let Some(ref flag) = self.triggered {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }

        Ok(ToolResult::ok(
            format!(
                "Handoff to '{target}' initiated. \
                 Your task is complete — do not call any more tools.",
            ),
            &ctx.tool_use_id,
        ))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::tool::ToolContext;

    fn make_store() -> Arc<HandoffAgentStore> {
        Arc::new(HandoffAgentStore::new())
    }

    fn make_ctx(conv_id: &str) -> ToolContext {
        let mut ctx = ToolContext::new("tid");
        if !conv_id.is_empty() {
            ctx.state
                .insert("conversation_id".into(), serde_json::json!(conv_id));
        }
        ctx
    }

    #[tokio::test]
    async fn handoff_writes_to_store() {
        let store = make_store();
        let tool = HandoffTool::new(Arc::clone(&store), vec!["fixer".into()]);
        let ctx = make_ctx("conv-1");

        let result = tool
            .call(
                serde_json::json!({"target_agent": "fixer", "reason": "found bugs"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(
            store.get_active_agent("conv-1").await,
            Some("fixer".to_string())
        );
        assert_eq!(
            store.get_and_clear_summary("conv-1").await,
            Some("found bugs".to_string())
        );
    }

    #[tokio::test]
    async fn handoff_rejects_disallowed_target() {
        let store = make_store();
        let tool = HandoffTool::new(Arc::clone(&store), vec!["fixer".into()]);
        let ctx = make_ctx("conv-2");

        let result = tool
            .call(
                serde_json::json!({"target_agent": "evil_agent", "reason": "escape"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(
            !result.is_error,
            "tool returns ok with rejection message inside"
        );
        assert!(result.content.contains("not permitted"));
        assert_eq!(store.get_active_agent("conv-2").await, None);
    }

    #[tokio::test]
    async fn handoff_empty_allowed_targets_permits_any() {
        let store = make_store();
        let tool = HandoffTool::new(Arc::clone(&store), vec![]); // unrestricted
        let ctx = make_ctx("conv-3");

        let result = tool
            .call(
                serde_json::json!({"target_agent": "any_agent", "reason": "reason"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(
            store.get_active_agent("conv-3").await,
            Some("any_agent".to_string())
        );
    }

    #[tokio::test]
    async fn handoff_sets_flag() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let store = make_store();
        let flag = Arc::new(AtomicBool::new(false));
        let tool =
            HandoffTool::new_with_flag(Arc::clone(&store), vec!["coder".into()], Arc::clone(&flag));
        let ctx = make_ctx("conv-flag");

        assert!(!flag.load(Ordering::Acquire));
        tool.call(
            serde_json::json!({"target_agent": "coder", "reason": "plan ready"}),
            &ctx,
        )
        .await
        .unwrap();
        assert!(
            flag.load(Ordering::Acquire),
            "flag must be true after handoff"
        );
    }

    #[tokio::test]
    async fn handoff_empty_conv_id_does_not_panic() {
        let store = make_store();
        let tool = HandoffTool::new(Arc::clone(&store), vec![]);
        let ctx = make_ctx(""); // no conversation_id

        let result = tool
            .call(
                serde_json::json!({"target_agent": "agent", "reason": "test"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn handoff_missing_target_returns_error_msg() {
        let store = make_store();
        let tool = HandoffTool::new(Arc::clone(&store), vec![]);
        let ctx = make_ctx("conv-x");

        let result = tool
            .call(serde_json::json!({"target_agent": "", "reason": "x"}), &ctx)
            .await
            .unwrap();

        assert!(result.content.contains("target_agent is required"));
    }
}
