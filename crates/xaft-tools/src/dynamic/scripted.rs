//! `ScriptedTool` — a dynamically-defined tool backed by a bash script.
//!
//! Uses the `agtrs_shell` `CommandExecutor` and `Sandbox` for path sandboxing
//! and timeout enforcement, and respects the approval gate via
//! `requires_confirmation`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::{Bash, CommandExecutor, ExecutionPolicy, Sandbox};

/// A tool whose implementation is a bash script.
///
/// The script receives the tool input as:
/// - `XAFT_TOOL_INPUT`: the full JSON-encoded input object.
/// - `XAFT_INPUT_<FIELD>`: one variable per top-level string field in the input
///   (key converted to `SCREAMING_SNAKE_CASE`).
///
/// The tool returns stdout on success or `stderr + exit code` on failure.
///
/// # Path safety
///
/// `ScriptedTool` runs inside the workspace sandbox configured at construction
/// time. The script cannot create files outside `workspace_root`.
///
/// # Approval gate
///
/// When `requires_confirmation = true` (the default for dynamically-created
/// shell tools), the `ApprovalGate` blocks execution until the user approves.
pub struct ScriptedTool {
    /// Tool name exposed to the LLM.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Bash script body executed on each tool call.
    pub script: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: Value,
    /// Whether this tool requires human-in-the-loop approval.
    pub requires_confirmation: bool,
    executor: Arc<CommandExecutor>,
}

impl ScriptedTool {
    /// Create a new `ScriptedTool`.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        script: impl Into<String>,
        input_schema: Value,
        requires_confirmation: bool,
        executor: Arc<CommandExecutor>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            script: script.into(),
            input_schema,
            requires_confirmation,
            executor,
        }
    }

    /// Create a `ScriptedTool` with a fresh executor rooted at `workspace_root`.
    pub fn with_workspace(
        name: impl Into<String>,
        description: impl Into<String>,
        script: impl Into<String>,
        input_schema: Value,
        requires_confirmation: bool,
        workspace_root: &std::path::Path,
    ) -> Self {
        let sandbox = Sandbox::new(workspace_root);
        let policy = ExecutionPolicy::permissive();
        let executor = Arc::new(CommandExecutor::new(sandbox, policy));
        Self::new(
            name,
            description,
            script,
            input_schema,
            requires_confirmation,
            executor,
        )
    }

    /// Build the environment variable map from the tool input.
    fn build_env(input: &Value) -> HashMap<String, String> {
        let mut env = HashMap::new();
        let json_str = serde_json::to_string(input).unwrap_or_default();
        env.insert("XAFT_TOOL_INPUT".into(), json_str);

        if let Value::Object(map) = input {
            for (key, val) in map {
                let env_key = format!("XAFT_INPUT_{}", key.to_uppercase());
                let env_val = match val {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => String::new(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                env.insert(env_key, env_val);
            }
        }
        env
    }
}

impl std::fmt::Debug for ScriptedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedTool")
            .field("name", &self.name)
            .finish()
    }
}

#[async_trait]
impl Tool for ScriptedTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.input_schema.clone()
    }
    fn requires_confirmation(&self) -> bool {
        self.requires_confirmation
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("scripted_tool '{}' cancelled", self.name),
            });
        }

        let env = Self::build_env(&input);
        let script = self.script.clone();

        // Build env prefix to inject variables into the bash invocation.
        let mut env_prefix = String::new();
        for (key, val) in &env {
            let escaped = val.replace('\\', "\\\\").replace('\'', "'\\''");
            env_prefix.push_str(&format!("export {}='{}'; ", key, escaped));
        }
        let full_script = format!("{env_prefix}{script}");
        let bash_cmd = Bash::new(&full_script);

        let cancel_token = Some(ctx.cancel_token.clone());
        let result = self
            .executor
            .execute(&bash_cmd, cancel_token)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: self.name.clone(),
                reason: e.to_string(),
            })?;

        if result.exit_code == 0 {
            Ok(ToolResult::ok(
                result.stdout.trim().to_string(),
                &ctx.tool_use_id,
            ))
        } else {
            Ok(ToolResult::error(
                format!("exit {}: {}", result.exit_code, result.stderr),
                &ctx.tool_use_id,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_executor(dir: &TempDir) -> Arc<CommandExecutor> {
        let sandbox = Sandbox::new(dir.path());
        let policy = ExecutionPolicy::permissive();
        Arc::new(CommandExecutor::new(sandbox, policy))
    }

    #[tokio::test]
    async fn scripted_tool_runs_echo() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);
        let tool = ScriptedTool::new(
            "echo_test",
            "Echo test",
            "echo hello_world",
            serde_json::json!({"type":"object"}),
            false,
            executor,
        );
        let ctx = ToolContext::new("t1");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("hello_world"));
    }

    #[tokio::test]
    async fn scripted_tool_env_injection() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);
        let tool = ScriptedTool::new(
            "env_test",
            "Env test",
            "echo $XAFT_INPUT_MSG",
            serde_json::json!({"type":"object"}),
            false,
            executor,
        );
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"msg": "greetings"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("greetings"));
    }

    #[tokio::test]
    async fn scripted_tool_failure_returns_error_result() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);
        let tool = ScriptedTool::new(
            "fail_test",
            "Fail test",
            "exit 1",
            serde_json::json!({"type":"object"}),
            false,
            executor,
        );
        let ctx = ToolContext::new("t3");
        let result = tool.call(serde_json::json!({}), &ctx).await.unwrap();
        assert!(result.is_error);
    }
}
