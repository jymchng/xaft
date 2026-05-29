//! `ReadBeforeEditHook` — enforces that files must be read before they can be edited.
//!
//! This hook prevents agents from calling `edit_file` on files they haven't read,
//! which avoids `replace_block: pattern not found` errors caused by guessing
//! file contents.
//!
//! # Behavior
//!
//! | Tool         | Condition              | Result    |
//! |--------------|------------------------|-----------|
//! | `read_file`  | any                    | Allowed; tracks path on success |
//! | `write_file` | any                    | Allowed; tracks path on success |
//! | `edit_file`  | path in known_files    | Allowed   |
//! | `edit_file`  | path NOT in known_files| **Rejected** |
//! | other tools  | any                    | Allowed (no tracking) |
//!
//! # Usage
//!
//! Register as a global hook on the `HandoffOrchestrator` builder:
//!
//! ```rust,ignore
//! use xaft_tools::fs::read_before_edit::ReadBeforeEditHook;
//!
//! let hook = Arc::new(ReadBeforeEditHook::new());
//! let orchestrator = HandoffOrchestrator::builder()
//!     .with_global_tool_hook(hook)
//!     .build();
//! ```

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tracing::debug;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool_hooks::{
    AfterHookDecision, BeforeHookDecision, ToolAfterContext, ToolCallContext, ToolExecutionStatus,
    ToolHook,
};

/// Enforces that files must be read (or written) before they can be edited.
///
/// Tracks successful `read_file` and `write_file` calls in an internal
/// `HashSet`. Blocks `edit_file` calls for paths not in the set.
///
/// # Thread Safety
///
/// The internal `HashSet` is protected by a `RwLock`, making this hook
/// safe to share across concurrent agent executions via `Arc`.
pub struct ReadBeforeEditHook {
    known_files: Arc<RwLock<HashSet<String>>>,
}

impl ReadBeforeEditHook {
    /// Create a new hook with an empty known-files set.
    pub fn new() -> Self {
        Self {
            known_files: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Create a hook pre-seeded with known file paths (useful for tests).
    pub fn with_known_files(files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let set: HashSet<String> = files.into_iter().map(Into::into).collect();
        Self {
            known_files: Arc::new(RwLock::new(set)),
        }
    }

    /// Check if a path is in the known-files set.
    pub fn is_known(&self, path: &str) -> bool {
        self.known_files
            .read()
            .map(|set| set.contains(path))
            .unwrap_or(false)
    }

    /// Manually add a path to the known-files set.
    pub fn mark_known(&self, path: impl Into<String>) {
        if let Ok(mut set) = self.known_files.write() {
            set.insert(path.into());
        }
    }
}

impl Default for ReadBeforeEditHook {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ReadBeforeEditHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.known_files.read().map(|set| set.len()).unwrap_or(0);
        f.debug_struct("ReadBeforeEditHook")
            .field("known_files_count", &count)
            .finish()
    }
}

#[async_trait]
impl ToolHook for ReadBeforeEditHook {
    async fn before(&self, context: &ToolCallContext) -> Result<BeforeHookDecision, AgtrsError> {
        // Only enforce for edit_file
        if context.tool_name != "edit_file" {
            return Ok(BeforeHookDecision::Proceed {
                modified_input: None,
            });
        }

        let path = context
            .input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let known = self
            .known_files
            .read()
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: "edit_file".into(),
                reason: format!("internal lock error: {e}"),
            })?;

        if known.contains(path) {
            debug!(path, "read_before_edit: path is known, allowing edit");
            Ok(BeforeHookDecision::Proceed {
                modified_input: None,
            })
        } else {
            debug!(path, "read_before_edit: path NOT known, rejecting edit");
            Ok(BeforeHookDecision::Reject(format!(
                "Cannot edit '{path}' — you must call read_file on this path first. \
                 This ensures you have the current file contents before making changes."
            )))
        }
    }

    async fn after(&self, context: &ToolAfterContext) -> Result<AfterHookDecision, AgtrsError> {
        // Track successful read_file and write_file calls
        if matches!(context.call.tool_name.as_str(), "read_file" | "write_file") {
            if let ToolExecutionStatus::Success(_) = &context.result {
                if let Some(path) = context.call.input.get("path").and_then(|v| v.as_str()) {
                    if let Ok(mut known) = self.known_files.write() {
                        debug!(path, tool = %context.call.tool_name, "read_before_edit: tracking known file");
                        known.insert(path.to_string());
                    }
                }
            }
        }

        Ok(AfterHookDecision::Return {
            modified_result: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::tool::ToolContext;
    use agtrs_runtime::tool_hooks::ToolAfterContext;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn make_call_context(tool_name: &str, path: &str) -> ToolCallContext {
        ToolCallContext {
            tool_name: tool_name.to_string(),
            input: serde_json::json!({"path": path}),
            agent_context: make_agent_context(),
            tool_context: ToolContext::new("test-use-id"),
            started_at: Instant::now(),
            call_id: Uuid::new_v4(),
        }
    }

    fn make_after_context(
        tool_name: &str,
        path: &str,
        status: ToolExecutionStatus,
    ) -> ToolAfterContext {
        ToolAfterContext {
            call: make_call_context(tool_name, path),
            result: status,
            duration: Duration::from_millis(10),
        }
    }

    fn make_agent_context() -> agtrs_runtime::agent::AgentContext {
        use agtrs_runtime::agent::AgentConfig;
        use injectable_runtime::ResolveContext;

        // We need a dummy LLM for AgentContext construction
        struct DummyLlm;
        #[async_trait]
        impl agtrs_runtime::llm::LlmProvider for DummyLlm {
            async fn complete(
                &self,
                _: &[agtrs_runtime::transport::Message],
                _: &agtrs_runtime::llm::LlmOptions,
            ) -> Result<agtrs_runtime::llm::LlmResponse, AgtrsError> {
                unreachable!()
            }
            async fn stream(
                &self,
                _: &[agtrs_runtime::transport::Message],
                _: &agtrs_runtime::llm::LlmOptions,
            ) -> Result<
                std::pin::Pin<
                    Box<
                        dyn futures::Stream<
                                Item = Result<agtrs_runtime::llm::StreamChunk, AgtrsError>,
                            > + Send,
                    >,
                >,
                AgtrsError,
            > {
                unreachable!()
            }
            async fn embed(
                &self,
                _: &[String],
                _: Option<&str>,
            ) -> Result<Vec<agtrs_runtime::transport::Embedding>, AgtrsError> {
                Ok(vec![])
            }
            async fn count_tokens(
                &self,
                _: &[agtrs_runtime::transport::Message],
            ) -> Result<usize, AgtrsError> {
                Ok(0)
            }
            fn context_window(&self) -> usize {
                1024
            }
            fn model(&self) -> &str {
                "dummy"
            }
        }

        agtrs_runtime::agent::AgentContext::new(
            "test-agent",
            AgentConfig::default(),
            Arc::new(DummyLlm),
            Arc::new(ResolveContext::from_store(Arc::new(
                injectable_runtime::EmptySingletonStore,
            ))),
        )
    }

    #[tokio::test]
    async fn edit_without_read_rejected() {
        let hook = ReadBeforeEditHook::new();
        let ctx = make_call_context("edit_file", "src/main.rs");
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(result, BeforeHookDecision::Reject(_)));
        if let BeforeHookDecision::Reject(msg) = result {
            assert!(msg.contains("read_file"));
            assert!(msg.contains("src/main.rs"));
        }
    }

    #[tokio::test]
    async fn edit_after_read_allowed() {
        let hook = ReadBeforeEditHook::new();

        // First, simulate a successful read
        let read_ctx = make_after_context(
            "read_file",
            "src/main.rs",
            ToolExecutionStatus::Success(agtrs_runtime::tool::ToolResult::ok(
                "file contents",
                "use-1",
            )),
        );
        hook.after(&read_ctx).await.unwrap();

        // Now edit should be allowed
        let edit_ctx = make_call_context("edit_file", "src/main.rs");
        let result = hook.before(&edit_ctx).await.unwrap();
        assert!(matches!(
            result,
            BeforeHookDecision::Proceed {
                modified_input: None
            }
        ));
    }

    #[tokio::test]
    async fn edit_after_write_allowed() {
        let hook = ReadBeforeEditHook::new();

        // Simulate a successful write
        let write_ctx = make_after_context(
            "write_file",
            "src/new_file.rs",
            ToolExecutionStatus::Success(agtrs_runtime::tool::ToolResult::ok("created", "use-2")),
        );
        hook.after(&write_ctx).await.unwrap();

        // Now edit should be allowed
        let edit_ctx = make_call_context("edit_file", "src/new_file.rs");
        let result = hook.before(&edit_ctx).await.unwrap();
        assert!(matches!(
            result,
            BeforeHookDecision::Proceed {
                modified_input: None
            }
        ));
    }

    #[tokio::test]
    async fn write_without_read_allowed() {
        let hook = ReadBeforeEditHook::new();
        let ctx = make_call_context("write_file", "src/new_file.rs");
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(
            result,
            BeforeHookDecision::Proceed {
                modified_input: None
            }
        ));
    }

    #[tokio::test]
    async fn read_file_always_allowed() {
        let hook = ReadBeforeEditHook::new();
        let ctx = make_call_context("read_file", "src/main.rs");
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(
            result,
            BeforeHookDecision::Proceed {
                modified_input: None
            }
        ));
    }

    #[tokio::test]
    async fn other_tools_allowed() {
        let hook = ReadBeforeEditHook::new();
        let ctx = make_call_context("grep", "pattern");
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(
            result,
            BeforeHookDecision::Proceed {
                modified_input: None
            }
        ));
    }

    #[tokio::test]
    async fn read_failure_not_tracked() {
        let hook = ReadBeforeEditHook::new();

        // Simulate a failed read
        let read_ctx = make_after_context(
            "read_file",
            "src/missing.rs",
            ToolExecutionStatus::Error("file not found".to_string()),
        );
        hook.after(&read_ctx).await.unwrap();

        // Edit should still be rejected
        let edit_ctx = make_call_context("edit_file", "src/missing.rs");
        let result = hook.before(&edit_ctx).await.unwrap();
        assert!(matches!(result, BeforeHookDecision::Reject(_)));
    }

    #[tokio::test]
    async fn write_failure_not_tracked() {
        let hook = ReadBeforeEditHook::new();

        // Simulate a failed write
        let write_ctx = make_after_context(
            "write_file",
            "src/bad.rs",
            ToolExecutionStatus::Error("permission denied".to_string()),
        );
        hook.after(&write_ctx).await.unwrap();

        // Edit should still be rejected
        let edit_ctx = make_call_context("edit_file", "src/bad.rs");
        let result = hook.before(&edit_ctx).await.unwrap();
        assert!(matches!(result, BeforeHookDecision::Reject(_)));
    }

    #[tokio::test]
    async fn multiple_files_tracked_independently() {
        let hook = ReadBeforeEditHook::new();

        // Read file A
        let read_a = make_after_context(
            "read_file",
            "src/a.rs",
            ToolExecutionStatus::Success(agtrs_runtime::tool::ToolResult::ok(
                "a contents",
                "use-a",
            )),
        );
        hook.after(&read_a).await.unwrap();

        // Edit A should work
        let edit_a = make_call_context("edit_file", "src/a.rs");
        assert!(matches!(
            hook.before(&edit_a).await.unwrap(),
            BeforeHookDecision::Proceed { .. }
        ));

        // Edit B should be rejected
        let edit_b = make_call_context("edit_file", "src/b.rs");
        assert!(matches!(
            hook.before(&edit_b).await.unwrap(),
            BeforeHookDecision::Reject(_)
        ));
    }

    #[tokio::test]
    async fn with_known_files_pre_seeded() {
        let hook = ReadBeforeEditHook::with_known_files(["src/existing.rs", "lib.rs"]);

        assert!(hook.is_known("src/existing.rs"));
        assert!(hook.is_known("lib.rs"));
        assert!(!hook.is_known("src/unknown.rs"));

        // Edit pre-seeded file should work
        let ctx = make_call_context("edit_file", "src/existing.rs");
        assert!(matches!(
            hook.before(&ctx).await.unwrap(),
            BeforeHookDecision::Proceed { .. }
        ));
    }

    #[tokio::test]
    async fn mark_known_manually() {
        let hook = ReadBeforeEditHook::new();
        assert!(!hook.is_known("manual.rs"));

        hook.mark_known("manual.rs");
        assert!(hook.is_known("manual.rs"));

        let ctx = make_call_context("edit_file", "manual.rs");
        assert!(matches!(
            hook.before(&ctx).await.unwrap(),
            BeforeHookDecision::Proceed { .. }
        ));
    }

    #[tokio::test]
    async fn empty_path_rejected() {
        let hook = ReadBeforeEditHook::new();
        let ctx = make_call_context("edit_file", "");
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(result, BeforeHookDecision::Reject(_)));
    }

    #[tokio::test]
    async fn missing_path_field_rejected() {
        let hook = ReadBeforeEditHook::new();
        let ctx = ToolCallContext {
            tool_name: "edit_file".to_string(),
            input: serde_json::json!({}), // no path field
            agent_context: make_agent_context(),
            tool_context: ToolContext::new("test-use-id"),
            started_at: Instant::now(),
            call_id: Uuid::new_v4(),
        };
        let result = hook.before(&ctx).await.unwrap();
        assert!(matches!(result, BeforeHookDecision::Reject(_)));
    }
}
