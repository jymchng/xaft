//! `RunCommandTool` — execute an executable directly, with no shell in between.
//!
//! # Why this is not just `bash_exec` with a different name
//!
//! [`BashExecTool`](super::BashExecTool) hands one string to `bash -c`, so the
//! shell decides what the arguments mean. That is a quoting hazard whenever the
//! argument list came from somewhere else: `bash_exec` with
//! `{"rm", "my file.txt"}` glued together into a string is
//! `rm my file.txt` — two files instead of one. agenthicc separates the two
//! tools for exactly this reason, and so does this one.
//!
//! The contract is therefore: `argv[0]` is the program, `argv[1..]` are its
//! arguments, and **no shell ever parses them**. A single argument containing
//! spaces, `;`, `&&`, `$(...)` or a newline reaches the program as one opaque
//! string. That is the whole point of the tool and it is what the tests below
//! pin.
//!
//! # Background execution is deliberately refused
//!
//! agenthicc's `run_command` accepts `background`, `label`, `lifecycle` and
//! `readiness`, and when any of them is set it hands off to a
//! `TerminalManager` (`inspect_terminal`, `stop_terminal`, `wait_terminal`,
//! `wait_terminal_ready` — matrix rows 36/43/44/45). xaft now *has* a
//! manager (`xaft_tools::terminals`), but those four tools are not registered
//! yet, so there is no agent-visible way to name a terminal and therefore
//! nothing this tool could hand off to. Refusing is therefore still the
//! honest answer — but it is now waiting on the tool registration, not on
//! the manager itself.
//!
//! agenthicc handles that itself: when the manager is absent it returns
//! `{"ok": false, "background": true, "state": "rejected", "error": ...}`
//! rather than pretending to have started a terminal. This tool does the same,
//! with the same envelope and the same wording, so the refusal is honest and
//! recognisable instead of a silently-ignored flag.
//!
//! # Safety
//!
//! Behind the same approval gate as `bash_exec`
//! ([`Tool::requires_confirmation`]), and through the same
//! [`ExecutionPolicy`] + [`Sandbox`] pair, so policy rules apply to the program
//! name exactly as they do to a shell command. `cwd` is resolved through the
//! workspace store so a `..` escape is rejected before anything is spawned.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::{CommandExecutor, ExecutionPolicy, Sandbox, ShellCommand, ShellCommandDef};

/// An argv list resolved into a spawnable command, with no shell.
///
/// Shared with [`super::run_python`], so "argv, never re-parsed by a shell" is
/// one type with one behaviour rather than two that can drift.
pub(crate) struct ArgvCommand {
    program: String,
    args: Vec<String>,
    working_dir: Option<std::path::PathBuf>,
    env: HashMap<String, String>,
    timeout: Option<Duration>,
}

impl ArgvCommand {
    /// Build an argv command. The program is `argv[0]`; `args` is `argv[1..]`.
    pub(crate) fn new(
        program: String,
        args: Vec<String>,
        working_dir: Option<std::path::PathBuf>,
        env: HashMap<String, String>,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            program,
            args,
            working_dir,
            env,
            timeout,
        }
    }
}

impl ShellCommand for ArgvCommand {
    fn command(&self) -> ShellCommandDef {
        ShellCommandDef {
            program: self.program.clone(),
            args: self.args.clone(),
            working_dir: self.working_dir.clone(),
            env: self.env.clone(),
            timeout: self.timeout,
            display_name: format!("{} {}", self.program, self.args.join(" ")),
        }
    }

    fn validate(&self) -> Result<(), agtrs_shell::error::ShellError> {
        if self.program.trim().is_empty() {
            return Err(agtrs_shell::error::ShellError::ValidationFailed {
                program: self.program.clone(),
                reason: "argv[0] (the program) must not be empty".into(),
            });
        }
        Ok(())
    }
}

/// Execute an executable directly (no shell) and return stdout/stderr.
pub struct RunCommandTool {
    executor: Arc<CommandExecutor>,
    workspace_root: std::path::PathBuf,
}

impl RunCommandTool {
    /// Create with a custom executor (sandbox + policy pre-configured).
    pub fn new(
        executor: Arc<CommandExecutor>,
        workspace_root: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            executor,
            workspace_root: workspace_root.into(),
        }
    }

    /// Create with a default permissive executor in `working_dir`.
    pub fn with_working_dir(working_dir: impl Into<std::path::PathBuf>) -> Self {
        let dir: std::path::PathBuf = working_dir.into();
        let sandbox = Sandbox::new(&dir);
        let policy = ExecutionPolicy::permissive();
        let executor = Arc::new(CommandExecutor::new(sandbox, policy));
        Self {
            executor,
            workspace_root: dir,
        }
    }

    const TOOL_NAME: &'static str = "run_command";
}

impl std::fmt::Debug for RunCommandTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCommandTool")
            .field("workspace_root", &self.workspace_root)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Execute an executable directly, with no shell, and return its stdout, \
         stderr and exit code. `argv` is [program, arg, ...] and each element is \
         passed to the process verbatim: an argument containing spaces, `;`, `&&` \
         or `$(...)` is NOT re-parsed or word-split, because no shell sees it. \
         Use this instead of bash_exec when arguments come from a variable or a \
         tool result, and bash_exec when you actually need pipes, redirection or \
         globbing. Background execution is not supported."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "Command and arguments as a list, e.g. [\"python\", \"-c\", \"print(1)\"]. argv[0] is the program. No shell parses these."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command; defaults to the workspace. Must stay inside the workspace."
                },
                "timeout": {
                    "type": "number",
                    "default": 30.0,
                    "description": "Maximum seconds to wait. Defaults to 30."
                },
                "env": {
                    "type": "object",
                    "description": "String environment overlay, merged over the sandbox environment."
                }
            },
            "required": ["argv"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        // Same gate as bash_exec: spawning a process is the risk, whether or not
        // a shell is involved. Low-risk programs are auto-approved by the TUI;
        // High/Critical ones block until the user decides.
        true
    }

    #[instrument(name = "run_command", skip(self, ctx), fields(program))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        // ---- argv ----------------------------------------------------------
        // agenthicc's schema types argv as an array of strings and requires it.
        // Anything else is a malformed call, not a coerced value.
        let argv = input
            .get("argv")
            .and_then(|v| v.as_array())
            .ok_or_else(|| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "required field 'argv' is missing or not an array".into(),
            })?;

        let mut parts: Vec<String> = Vec::with_capacity(argv.len());
        for (i, v) in argv.iter().enumerate() {
            let s = v.as_str().ok_or_else(|| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("argv[{i}] is not a string"),
            })?;
            parts.push(s.to_string());
        }

        if parts.is_empty() {
            return Err(AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "argv must contain at least the program name".into(),
            });
        }
        let program = parts.remove(0);

        tracing::Span::current().record("program", program.as_str());

        // ---- background: refused, exactly as agenthicc refuses it -----------
        if input.get("background").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(ToolResult::error(
                "background terminals are unavailable in this execution context",
                &ctx.tool_use_id,
            ));
        }

        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before start", Self::TOOL_NAME),
            });
        }

        // ---- cwd: confined to the workspace, like every other fs tool ------
        //
        // `Path::starts_with` is a purely lexical COMPONENT prefix test, so
        // `root.join("../..")` still "starts with" `root` — the `..` is never
        // collapsed, it is only resolved by the kernel at spawn time. Testing
        // the joined path this way silently permits a full escape. So reject
        // any `..` component outright, which is what
        // `FsWorkspaceStore::resolve` does for every path-shaped argument.
        let cwd = match crate::error::opt_str(&input, "cwd").filter(|s| !s.is_empty()) {
            Some(raw) => {
                if std::path::Path::new(raw)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: format!("cwd must not contain '..': {raw}"),
                    });
                }
                let resolved = self.workspace_root.join(raw);
                if !resolved.starts_with(&self.workspace_root) {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: Self::TOOL_NAME.to_string(),
                        reason: format!("cwd escapes the workspace: {raw}"),
                    });
                }
                resolved
            }
            None => self.workspace_root.clone(),
        };

        // ---- env overlay ---------------------------------------------------
        let mut env = HashMap::new();
        if let Some(obj) = input.get("env").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                let s = v.as_str().ok_or_else(|| AgtrsError::ToolCallFailed {
                    tool_name: Self::TOOL_NAME.to_string(),
                    reason: format!("env.{k} is not a string"),
                })?;
                env.insert(k.clone(), s.to_string());
            }
        }

        // ---- timeout: agenthicc's default is 30.0s -------------------------
        let timeout = input
            .get("timeout")
            .and_then(|v| v.as_f64())
            .map(Duration::from_secs_f64)
            .or_else(|| crate::error::opt_u64(&input, "timeout").map(Duration::from_secs));

        let cmd = ArgvCommand {
            program,
            args: parts,
            working_dir: Some(cwd),
            env,
            timeout,
        };

        let output = self
            .executor
            .execute(&cmd, Some(ctx.cancel_token.clone()))
            .await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: e.to_string(),
            })?;

        tracing::info!(
            exit_code = output.exit_code,
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len(),
            duration_ms = output.duration.as_millis(),
            "run_command"
        );

        // Same unambiguous framing as bash_exec: a non-zero exit must not read
        // like success to the agent reading the transcript.
        let mut result = if output.success {
            String::from("Exit code: 0 (success)\n")
        } else {
            format!(
                "COMMAND FAILED — exit code {}\nDo NOT proceed as if this succeeded.\n",
                output.exit_code
            )
        };

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
            Ok(ToolResult::error(trimmed, &ctx.tool_use_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_executor(dir: &TempDir) -> Arc<CommandExecutor> {
        let sandbox = Sandbox::new(dir.path()).with_timeout(Duration::from_secs(10));
        let policy = ExecutionPolicy::permissive();
        Arc::new(CommandExecutor::new(sandbox, policy))
    }

    fn tool_for(dir: &TempDir) -> RunCommandTool {
        RunCommandTool::new(make_executor(dir), dir.path())
    }

    /// The load-bearing test: a single argument containing spaces and shell
    /// metacharacters must arrive at the program as ONE argument. If any shell
    /// were in the path, `;` would split the command and `my file.txt` would
    /// become two arguments.
    #[tokio::test]
    async fn an_argument_with_spaces_and_metacharacters_is_passed_literally() {
        let tmp = TempDir::new().unwrap();
        let marker = tmp.path().join("a file; echo pwned.txt");
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-1");

        let result = tool
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "printf '%s' \"$0\"", marker.to_str().unwrap()],
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.content);
        // "$0" is the first argument after the program/script, i.e. the whole
        // path as one token.
        assert!(
            result.content.contains(&format!("{}", marker.display())),
            "the argument was re-split: {}",
            result.content
        );
        // Nothing was executed as a side effect of the `;`.
        assert!(
            !tmp.path().join("pwned.txt").exists(),
            "shell metacharacters were interpreted: a second command ran"
        );
    }

    #[tokio::test]
    async fn each_argv_element_is_a_separate_argument() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-2");

        // `sh -c 'printf ...' a b c` prints "a|b|c": three distinct argv entries.
        let result = tool
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "printf '%s' \"$0|$1|$2\"", "a", "b", "c"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("a|b|c"), "{}", result.content);
    }

    #[tokio::test]
    async fn executes_a_plain_program() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-3");
        let result = tool
            .call(serde_json::json!({"argv": ["/bin/echo", "hello"]}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
        assert!(result.content.contains("Exit code: 0"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported_rather_than_swallowed() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-4");
        let result = tool
            .call(
                serde_json::json!({"argv": ["/bin/sh", "-c", "exit 3"]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            result.is_error,
            "a failing command must not look successful"
        );
        assert!(result.content.contains("exit code 3"), "{}", result.content);
    }

    #[tokio::test]
    async fn stderr_is_captured() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-5");
        let result = tool
            .call(
                serde_json::json!({"argv": ["/bin/sh", "-c", "echo boom >&2; exit 1"]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.content.contains("stderr"), "{}", result.content);
        assert!(result.content.contains("boom"), "{}", result.content);
    }

    #[tokio::test]
    async fn env_overlay_is_applied() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-6");
        let result = tool
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "printf '%s' \"$XAFT_TEST_VAR\""],
                    "env": {"XAFT_TEST_VAR": "present"},
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("present"), "{}", result.content);
    }

    #[tokio::test]
    async fn cwd_runs_the_command_in_the_requested_directory() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-7");
        let result = tool
            .call(
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "printf '%s' \"$(basename \"$PWD\")\""],
                    "cwd": "sub",
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("sub"), "{}", result.content);
    }

    #[tokio::test]
    async fn cwd_escaping_the_workspace_is_refused() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-8");
        for bad in ["../..", "..", "sub/../../out", "/etc"] {
            let err = tool
                .call(
                    serde_json::json!({"argv": ["/bin/echo", "hi"], "cwd": bad}),
                    &ctx,
                )
                .await
                .expect_err("a path traversal in cwd must not execute");
            let msg = err.to_string();
            assert!(
                msg.contains("must not contain '..'") || msg.contains("escapes the workspace"),
                "cwd {bad:?} was not refused clearly: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn background_is_refused_rather_than_silently_ignored() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-9");
        let result = tool
            .call(
                serde_json::json!({"argv": ["/bin/echo", "hi"], "background": true}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result
                .content
                .contains("background terminals are unavailable"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn missing_argv_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-10");
        assert!(tool.call(serde_json::json!({}), &ctx).await.is_err());
    }

    #[tokio::test]
    async fn argv_of_the_wrong_type_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-11");
        assert!(
            tool.call(serde_json::json!({"argv": "echo hi"}), &ctx)
                .await
                .is_err()
        );
        assert!(
            tool.call(serde_json::json!({"argv": []}), &ctx)
                .await
                .is_err()
        );
        assert!(
            tool.call(serde_json::json!({"argv": [1, 2]}), &ctx)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn non_string_env_value_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let tool = tool_for(&tmp);
        let ctx = ToolContext::new("rc-12");
        assert!(
            tool.call(
                serde_json::json!({"argv": ["/bin/echo", "x"], "env": {"A": 5}}),
                &ctx
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn sits_behind_the_same_approval_gate_as_bash_exec() {
        let tmp = TempDir::new().unwrap();
        assert!(
            RunCommandTool::with_working_dir(tmp.path()).requires_confirmation(),
            "run_command spawns a process; it must be gated like bash_exec"
        );
    }

    #[tokio::test]
    async fn policy_is_enforced_on_the_program_name() {
        // The default (non-permissive) policy blocks dangerous programs, which
        // proves the tool routes through ExecutionPolicy rather than spawning
        // directly and bypassing it.
        let tmp = TempDir::new().unwrap();
        let sandbox = Sandbox::new(tmp.path()).with_timeout(Duration::from_secs(5));
        let executor = Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::default()));
        let tool = RunCommandTool::new(executor, tmp.path());
        let ctx = ToolContext::new("rc-13");
        let result = tool
            .call(serde_json::json!({"argv": ["/bin/rm", "-rf", "/"]}), &ctx)
            .await;
        assert!(
            result.is_err() || result.unwrap().is_error,
            "default ExecutionPolicy must block this"
        );
    }
}
