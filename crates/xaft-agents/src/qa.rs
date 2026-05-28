//! QA agent — reviews code changes and decides approve or fix.
//!
//! Owns the QA system prompt builder, [`QaVerdict`], and [`RequestFixTool`].

use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::team::HandoffAgentStore;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

/// Agent name used for routing and signal identification.
pub const QA_NAME: &str = "qa";

/// Default maximum LLM turns for the QA agent.
pub const QA_MAX_TURNS: usize = 25;

// ── QaVerdict ─────────────────────────────────────────────────────────────────

/// What the QA agent decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QaVerdict {
    /// Changes look correct — approve.
    Approved,
    /// Issues found — delegate to fixer with a summary of problems.
    RequestFix(String),
}

// ── RequestFixTool ────────────────────────────────────────────────────────────

/// Tool the QA agent calls to trigger a fixer handoff.
///
/// Writes to [`HandoffAgentStore`] so the orchestrator switches to the
/// fixer agent after the QA turn completes.
pub struct RequestFixTool {
    pub(crate) store: Arc<HandoffAgentStore>,
}

impl RequestFixTool {
    /// Create a new `RequestFixTool` backed by the shared handoff store.
    pub fn new(store: Arc<HandoffAgentStore>) -> Self {
        Self { store }
    }
}

impl std::fmt::Debug for RequestFixTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFixTool").finish()
    }
}

#[async_trait::async_trait]
impl Tool for RequestFixTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "request_fix"
    }

    fn description(&self) -> &str {
        "Report code issues to the fixer agent. Call when you find bugs, syntax errors, or incomplete task completion."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Precise description of all issues found — name files, functions, lines."
                }
            },
            "required": ["summary"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let summary = input["summary"]
            .as_str()
            .unwrap_or("Issues found")
            .to_string();
        let conv_id = ctx
            .state
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !conv_id.is_empty() {
            self.store
                .set_active_agent(&conv_id, super::fixer::FIXER_NAME)
                .await;
            self.store.set_pending_summary(&conv_id, &summary).await;
        }
        Ok(ToolResult::ok(
            format!("Fix requested: {summary}"),
            &ctx.tool_use_id,
        ))
    }
}

// ── System prompt ─────────────────────────────────────────────────────────────

/// Build the QA system prompt.
pub fn qa_system_prompt(task: &str, working_dir: &str) -> String {
    format!(
        "\
You are a code reviewer. Verify that the following task was completed correctly:

TASK: {task}
WORKING DIRECTORY: {working_dir}
Use relative paths for all file operations.

INSTRUCTIONS:
1. Call `list_files` to discover all files in the workspace.
2. Call `read_file` on the most important source files (up to 5 files maximum).
   Focus on files most directly related to the task. Do NOT read every file.
3. Verify by READING the code only — do NOT run bash commands or tests:
   a. The task was ACTUALLY completed (not just partially done)
   b. No obvious syntax errors or broken imports visible in the code
   c. No completely missing or stub implementations
   d. Code structure is consistent with the task

IMPORTANT: You only have a limited number of turns. Be efficient:
- Read only the key files (main source files, entry points)
- Skip test files, lock files, README, and generated files
- Do NOT try to compile or run the code

If the key files look correct: output exactly the word APPROVED on its own line.

If there are clear, obvious issues: call the `request_fix` tool once with a concise
list of ALL issues found. Be specific — name files and functions.
Do NOT fix anything yourself. Do NOT call request_fix more than once."
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qa_prompt_contains_task_and_dir() {
        let prompt = qa_system_prompt("add auth", "/workspace/app");
        assert!(prompt.contains("add auth"));
        assert!(prompt.contains("/workspace/app"));
        assert!(prompt.contains("APPROVED"));
    }

    #[tokio::test]
    async fn request_fix_tool_writes_to_store() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = RequestFixTool::new(Arc::clone(&store));
        let mut ctx = ToolContext::new("tid-1");
        ctx.state
            .insert("conversation_id".into(), serde_json::json!("conv-1"));

        let result = tool
            .call(
                serde_json::json!({"summary": "missing error handling in auth.rs"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("Fix requested"));
        assert_eq!(
            store.get_active_agent("conv-1").await,
            Some("fixer".to_string())
        );
        assert_eq!(
            store.get_and_clear_summary("conv-1").await,
            Some("missing error handling in auth.rs".to_string())
        );
    }

    #[tokio::test]
    async fn request_fix_tool_empty_conv_id_does_not_panic() {
        let store = Arc::new(HandoffAgentStore::new());
        let tool = RequestFixTool::new(Arc::clone(&store));
        let ctx = ToolContext::new("tid-noc"); // no conversation_id

        let result = tool
            .call(serde_json::json!({"summary": "issues"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
    }
}
