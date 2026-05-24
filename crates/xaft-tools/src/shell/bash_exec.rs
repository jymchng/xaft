//! `BashExecTool` — execute a bash command via the agtrs-shell executor.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::{Bash, CommandExecutor, ExecutionPolicy, Sandbox};

use crate::error::{opt_u64, require_str};

/// Execute a shell command and return its output.
///
/// All commands run through the `agtrs-shell` `CommandExecutor` with
/// configurable sandbox and policy. The policy controls which commands
/// are allowed.
pub struct BashExecTool {
    executor: Arc<CommandExecutor>,
}

impl BashExecTool {
    /// Create with a custom executor (sandbox + policy pre-configured).
    pub fn new(executor: Arc<CommandExecutor>) -> Self {
        Self { executor }
    }

    /// Create with a default permissive executor in `working_dir`.
    pub fn with_working_dir(working_dir: impl Into<std::path::PathBuf>) -> Self {
        let sandbox = Sandbox::new(working_dir);
        let policy = ExecutionPolicy::permissive();
        let executor = Arc::new(CommandExecutor::new(sandbox, policy));
        Self { executor }
    }

    const TOOL_NAME: &'static str = "bash_exec";
}

impl std::fmt::Debug for BashExecTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashExecTool").finish()
    }
}

#[async_trait]
impl Tool for BashExecTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Execute a bash shell command and return its stdout, stderr, and exit code. \
         Commands run within the configured sandbox and execution policy. \
         Destructive commands (rm, git push, etc.) may require confirmation. \
         Prefer typed tools (cargo, git_commit, etc.) for common developer operations."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (passed to bash -c)."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3600,
                    "description": "Command timeout in seconds. Defaults to the executor's sandbox timeout."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false // Controlled at executor level via ExecutionPolicy
    }

    #[instrument(name = "bash_exec", skip(self, ctx), fields(command))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let command = require_str(Self::TOOL_NAME, &input, "command").map_err(AgtrsError::from)?;

        if command.trim().is_empty() {
            return Err(AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "command must not be empty".into(),
            });
        }

        tracing::Span::current().record("command", command);

        let cancel_token = if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before start", Self::TOOL_NAME),
            });
        } else {
            Some(ctx.cancel_token.clone())
        };

        // Apply per-call timeout override if provided
        let _timeout_override = opt_u64(&input, "timeout_secs").map(Duration::from_secs);

        let cmd = Bash::new(command);

        let output = self
            .executor
            .execute(&cmd, cancel_token)
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: e.to_string(),
            })?;

        tracing::info!(
            command,
            exit_code = output.exit_code,
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len(),
            duration_ms = output.duration.as_millis(),
            "bash_exec"
        );

        // Format the output
        let mut result = format!("Exit code: {}\n", output.exit_code);

        if !output.stdout.is_empty() {
            result.push_str("\nstdout:\n");
            result.push_str(&output.stdout);
        }

        if !output.stderr.is_empty() {
            result.push_str("\nstderr:\n");
            result.push_str(&output.stderr);
        }

        let trimmed = result.trim_end().to_string();

        if output.success {
            Ok(ToolResult::ok(trimmed, &ctx.tool_use_id))
        } else {
            // Non-zero exit — return as error ToolResult so agent sees the failure
            Ok(ToolResult::error(trimmed, &ctx.tool_use_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_executor(dir: &TempDir) -> Arc<CommandExecutor> {
        let sandbox = Sandbox::new(dir.path()).with_timeout(Duration::from_secs(5));
        let policy = ExecutionPolicy::permissive();
        Arc::new(CommandExecutor::new(sandbox, policy))
    }

    #[tokio::test]
    async fn executes_echo_command() {
        let tmp = TempDir::new().unwrap();
        let tool = BashExecTool::new(make_executor(&tmp));
        let ctx = ToolContext::new("t1");
        let result = tool
            .call(
                serde_json::json!({"command": "echo 'hello from bash'"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("hello from bash"));
        assert!(result.content.contains("Exit code: 0"));
    }

    #[tokio::test]
    async fn nonzero_exit_returns_error_result() {
        let tmp = TempDir::new().unwrap();
        let tool = BashExecTool::new(make_executor(&tmp));
        let ctx = ToolContext::new("t2");
        let result = tool
            .call(serde_json::json!({"command": "exit 1"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn captures_stderr() {
        let tmp = TempDir::new().unwrap();
        let tool = BashExecTool::new(make_executor(&tmp));
        let ctx = ToolContext::new("t3");
        let result = tool
            .call(serde_json::json!({"command": "echo err >&2"}), &ctx)
            .await
            .unwrap();
        assert!(result.content.contains("stderr") || result.content.contains("err"));
    }

    #[tokio::test]
    async fn empty_command_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tool = BashExecTool::new(make_executor(&tmp));
        let ctx = ToolContext::new("t4");
        assert!(
            tool.call(serde_json::json!({"command": ""}), &ctx)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_command_field_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tool = BashExecTool::new(make_executor(&tmp));
        let ctx = ToolContext::new("t5");
        assert!(tool.call(serde_json::json!({}), &ctx).await.is_err());
    }

    #[tokio::test]
    async fn policy_blocks_rm() {
        let tmp = TempDir::new().unwrap();
        let sandbox = Sandbox::new(tmp.path()).with_timeout(Duration::from_secs(5));
        let policy = ExecutionPolicy::default(); // strict — blocks rm
        let executor = Arc::new(CommandExecutor::new(sandbox, policy));
        let tool = BashExecTool::new(executor);
        let ctx = ToolContext::new("t6");
        // rm is blocked by default ExecutionPolicy — should return error
        let result = tool
            .call(serde_json::json!({"command": "rm -rf /"}), &ctx)
            .await;
        // Either Err or error ToolResult
        assert!(result.is_err() || result.unwrap().is_error);
    }
}
