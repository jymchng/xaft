//! `DynamicToolFactory` — xaft-specific `ToolFactoryTool` with sandbox and signals.
//!
//! Extends `agtrs_runtime::dynamic_tools::ToolFactoryTool` with:
//!  - `ScriptedTool` (backed by `CommandExecutor` with the workspace sandbox)
//!    instead of the plain closure fallback used in agtrs.
//!  - A signal callback for `XaftDynamicToolCreated` (wire this up in the
//!    orchestrator once both crates are in scope).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use agtrs_runtime::dynamic_tools::{DynamicToolRegistry, LlmBackedTool, TOOL_FACTORY_NAME};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox};

use crate::dynamic::scripted::ScriptedTool;

/// Callback invoked after a tool is successfully registered.
///
/// Receives `(tool_name, tool_kind)`. The `tool_kind` is one of
/// `"shell_script"` or `"llm_prompt"`. The created_by_agent name is
/// not available at this layer; set it to `"unknown"` and let the
/// orchestrator fill it in if needed.
pub type OnToolCreated = Arc<dyn Fn(String, String) + Send + Sync>;

/// xaft-specific `ToolFactoryTool` that:
///  1. Validates the tool definition (same schema as `ToolFactoryTool`).
///  2. Creates `ScriptedTool` (with workspace sandbox) or `LlmBackedTool`.
///  3. Registers the new tool in the shared `DynamicToolRegistry`.
///  4. Invokes the optional `on_tool_created` callback (wired to emit
///     `XaftDynamicToolCreated` from the orchestrator layer).
pub struct DynamicToolFactory {
    /// The shared registry to register new tools into.
    pub registry: DynamicToolRegistry,
    /// LLM provider used for `llm_prompt` implementation kind.
    pub llm: Arc<dyn LlmProvider>,
    /// Workspace root for sandboxing shell tools.
    pub workspace_root: PathBuf,
    /// Pre-built executor for shell tools.
    executor: Arc<CommandExecutor>,
    /// Whether dynamic tool creation is enabled.
    pub allow_dynamic_tools: bool,
    /// Whether dynamically-created shell tools require approval-gate confirmation.
    pub dynamic_tool_approval: bool,
    /// Optional callback invoked after successful tool registration.
    pub on_tool_created: Option<OnToolCreated>,
}

impl DynamicToolFactory {
    /// Create a new `DynamicToolFactory`.
    pub fn new(
        registry: DynamicToolRegistry,
        llm: Arc<dyn LlmProvider>,
        workspace_root: PathBuf,
        allow_dynamic_tools: bool,
        dynamic_tool_approval: bool,
    ) -> Self {
        let sandbox = Sandbox::new(&workspace_root);
        let policy = ExecutionPolicy::permissive();
        let executor = Arc::new(CommandExecutor::new(sandbox, policy));
        Self {
            registry,
            llm,
            workspace_root,
            executor,
            allow_dynamic_tools,
            dynamic_tool_approval,
            on_tool_created: None,
        }
    }

    /// Attach a callback that fires after each successful tool registration.
    pub fn with_on_tool_created(mut self, cb: OnToolCreated) -> Self {
        self.on_tool_created = Some(cb);
        self
    }
}

impl std::fmt::Debug for DynamicToolFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicToolFactory")
            .field("allow_dynamic_tools", &self.allow_dynamic_tools)
            .field("dynamic_tool_approval", &self.dynamic_tool_approval)
            .finish()
    }
}

#[async_trait]
impl Tool for DynamicToolFactory {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        TOOL_FACTORY_NAME
    }

    fn description(&self) -> &str {
        "Define and register a new tool for use in subsequent turns of this run. \
         Supports 'shell_script' and 'llm_prompt' implementations. \
         The registered tool is immediately callable by name."
    }

    fn schema(&self) -> Value {
        agtrs_runtime::dynamic_tools::ToolFactoryTool::schema_json()
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if !self.allow_dynamic_tools {
            return Ok(ToolResult::error(
                "dynamic tool creation is disabled (allow_dynamic_tools = false in config)",
                &ctx.tool_use_id,
            ));
        }

        let tool_name = input["tool_name"].as_str().unwrap_or("").to_string();
        if tool_name.is_empty() {
            return Ok(ToolResult::error(
                "tool_factory: missing 'tool_name' field",
                &ctx.tool_use_id,
            ));
        }
        if tool_name.len() > 64 {
            return Ok(ToolResult::error(
                "tool_factory: 'tool_name' must be at most 64 characters",
                &ctx.tool_use_id,
            ));
        }
        for reserved in &["tool_factory", "bash_exec", "read_file", "write_file"] {
            if tool_name == *reserved {
                return Ok(ToolResult::error(
                    format!("tool_factory: '{}' is a reserved tool name", tool_name),
                    &ctx.tool_use_id,
                ));
            }
        }

        let description = input["description"].as_str().unwrap_or("").to_string();
        let input_schema = input
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type":"object"}));

        let impl_obj = match input.get("implementation") {
            Some(v) => v,
            None => {
                return Ok(ToolResult::error(
                    "tool_factory: missing 'implementation' field",
                    &ctx.tool_use_id,
                ));
            }
        };
        let kind = impl_obj["kind"].as_str().unwrap_or("");

        let tool_kind_label: String;
        let erased: Arc<ErasedTool> = match kind {
            "shell_script" => {
                tool_kind_label = "shell_script".into();
                let script = impl_obj["script"].as_str().unwrap_or("").to_string();
                let requires_approval = self.dynamic_tool_approval;
                Arc::new(ScriptedTool::new(
                    tool_name.clone(),
                    description.clone(),
                    script,
                    input_schema,
                    requires_approval,
                    Arc::clone(&self.executor),
                )) as Arc<ErasedTool>
            }
            "llm_prompt" => {
                tool_kind_label = "llm_prompt".into();
                let prompt = impl_obj["prompt"].as_str().unwrap_or("").to_string();
                let max_tokens = impl_obj["max_tokens"].as_u64().unwrap_or(1024) as u32;
                Arc::new(LlmBackedTool {
                    name: tool_name.clone(),
                    description: description.clone(),
                    schema: input_schema,
                    system_prompt: prompt,
                    max_tokens,
                    requires_confirmation: false,
                    llm: Arc::clone(&self.llm),
                }) as Arc<ErasedTool>
            }
            other => {
                return Ok(ToolResult::error(
                    format!("tool_factory: unknown implementation kind: '{other}'"),
                    &ctx.tool_use_id,
                ));
            }
        };

        self.registry
            .register(tool_name.clone(), erased)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: TOOL_FACTORY_NAME.into(),
                reason: e.to_string(),
            })?;

        // Fire optional signal callback.
        if let Some(ref cb) = self.on_tool_created {
            cb(tool_name.clone(), tool_kind_label);
        }

        Ok(ToolResult::ok(
            format!("Tool '{}' registered. You may now call it.", tool_name),
            &ctx.tool_use_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::dynamic_tools::DynamicToolRegistry;
    use agtrs_runtime::testing::{MockLlmProvider, MockTransport};

    fn make_factory(tmp: &tempfile::TempDir) -> DynamicToolFactory {
        let transport = Arc::new(MockTransport::new());
        let mock_llm = MockLlmProvider::new(transport);
        DynamicToolFactory::new(
            DynamicToolRegistry::new(),
            Arc::new(mock_llm),
            tmp.path().to_path_buf(),
            true,
            false,
        )
    }

    #[tokio::test]
    async fn dynamic_tool_factory_creates_shell_tool() {
        let tmp = tempfile::TempDir::new().unwrap();
        let factory = make_factory(&tmp);
        let ctx = ToolContext::new("t1");
        let result = factory
            .call(
                serde_json::json!({
                    "tool_name": "my_echo",
                    "description": "echoes hello",
                    "implementation": {
                        "kind": "shell_script",
                        "script": "echo hello"
                    }
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "got error: {}", result.content);
        assert!(result.content.contains("my_echo"));
        // The tool should now be in the registry.
        assert!(factory.registry.get("my_echo").await.is_some());
    }

    #[tokio::test]
    async fn dynamic_tool_factory_rejects_reserved_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let factory = make_factory(&tmp);
        let ctx = ToolContext::new("t1");
        let result = factory
            .call(
                serde_json::json!({
                    "tool_name": "bash_exec",
                    "description": "tries to shadow",
                    "implementation": {
                        "kind": "shell_script",
                        "script": "echo bad"
                    }
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("reserved"));
    }

    #[tokio::test]
    async fn dynamic_tool_factory_disabled_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        let mock_llm = MockLlmProvider::new(transport);
        let factory = DynamicToolFactory::new(
            DynamicToolRegistry::new(),
            Arc::new(mock_llm),
            tmp.path().to_path_buf(),
            false, // allow_dynamic_tools = false
            false,
        );
        let ctx = ToolContext::new("t1");
        let result = factory
            .call(
                serde_json::json!({
                    "tool_name": "whatever",
                    "description": "test",
                    "implementation": {"kind": "shell_script", "script": "echo hi"}
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("disabled"));
    }
}
