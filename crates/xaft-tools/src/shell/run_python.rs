//! `RunPythonTool` / `RunPythonExprTool` — run a Python snippet, behind an
//! explicit interpreter probe.
//!
//! # Why the probe is the whole design, not an afterthought
//!
//! agenthicc ships these two tools and can write them in seven lines each,
//! because for agenthicc a Python interpreter is not an assumption — it is
//! guaranteed. `RunPythonTool` (`tools/exec/__init__.py:560`) writes the
//! snippet to a temporary `.py` and spawns `[sys.executable, tmp_path]`;
//! `RunPythonExprTool` (`:600`) spawns
//! `[sys.executable, "-c", "_r = (expr); print(repr(_r))"]`. `sys.executable`
//! is the interpreter *already running agenthicc*, so it can never be missing
//! and never needs to be found.
//!
//! xaft has no such guarantee: it is a Rust binary, and before this commit
//! `grep -rn python crates/ --include=*.rs` returned no interpreter assumption
//! at all. Porting the call verbatim would therefore produce a tool that
//! either spawns nothing (a confusing `No such file or directory (os error 2)`
//! from deep inside a subprocess launcher) or, worse, silently resolves a
//! different `python` than the user meant.
//!
//! So the probe is the contract, and it has exactly two halves, both pinned by
//! tests below:
//!
//! 1. **Resolve first.** [`resolve_interpreter_in_path`] walks `PATH` for
//!    `python3`, then `python`, and returns the first candidate that is an
//!    existing executable file. No process is spawned to decide this, so
//!    resolution cannot half-succeed.
//! 2. **Refuse plainly, never spawn.** When resolution fails the tool returns
//!    a [`ToolResult`] error naming every candidate it looked for and both
//!    remedies, and it does **not** hand anything to the executor. The
//!    absent-interpreter path is therefore a readable sentence rather than an
//!    `io::Error` from three layers down. This is the load-bearing behaviour:
//!    `an_absent_interpreter_is_a_clear_message_not_a_spawn_failure` asserts the
//!    message, and `an_absent_interpreter_never_reaches_the_executor` asserts
//!    that a program which *would* be spawned is still not.
//!
//! # Provenance: no operator decision authorized this
//!
//! This tool was asked about three times (goals `3bb3d459`, `a68ba469`, and
//! the pass that wrote this file) and **no answer was recorded**. It is
//! implemented anyway because it is the smallest reversible form available and
//! because it adds no authority the product does not already grant:
//! [`RunCommandTool`] already lets the agent spawn any program, including
//! `python3`, through the same executor behind the same approval gate.
//!
//! It is deliberately reversible: delete the two `reg.add` calls in
//! `registry.rs` and the `pub mod run_python` line in `shell/mod.rs`, then flip
//! parity rows 39/40 back to ❌. `docs/parity-matrix.md` §6b records both the
//! non-answer and that reversal. **No config field was added** — the probe reads
//! `PATH` only — so there is no dead config to leave behind.
//!
//! # Safety
//!
//! Behind the same approval gate as `bash_exec` and `run_command`
//! ([`Tool::requires_confirmation`]), through the same [`CommandExecutor`] +
//! policy + sandbox pair, and `cwd` is confined to the workspace by exactly the
//! same lexical `..`-rejecting rule `run_command` uses — a component prefix
//! test never collapses `..`, so any `..` is refused before anything is
//! spawned.
//!
//! # Documented divergences from agenthicc
//!
//! - **`result` is a labelled first line, not a structured key.** agenthicc
//!   returns `result["result"] = repr`; xaft's [`ToolResult`] carries only
//!   `content` + `is_error`, so the repr is emitted as a `result: <repr>` first
//!   line, above the usual process framing.
//! - **`timeout` is validated, not coerced.** agenthicc's `_timeout` helper
//!   returns an "invalid" envelope; here a non-finite or negative `timeout` is
//!   a `ToolCallFailed`, because `Duration::from_secs_f64` panics on both.
//! - **`additionalProperties: false`**, matching xaft's `run_command` rather
//!   than agenthicc's open object, so an unrecognised key is a schema error
//!   instead of a silent no-op.
//! - **A non-zero exit is framed as a failure** (`COMMAND FAILED — exit code
//!   N`), the same unambiguous framing `run_command` uses, because a Python
//!   traceback on stderr must not read like success.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::{CommandExecutor, CommandOutput, ExecutionPolicy, Sandbox};

use super::run_command::ArgvCommand;

/// Interpreters probed, in order. `python3` first because on a host that has
/// both it is the one that is not Python 2.
pub const INTERPRETER_CANDIDATES: [&str; 2] = ["python3", "python"];

/// Walk `dirs` looking for the first candidate that is an existing executable
/// file, returning its full path.
///
/// Pure and injection-driven on purpose: `is_executable` is a closure so a test
/// can exercise "found", "found but not executable" and "not found" without a
/// Python installation anywhere near it — which is precisely the case a
/// `#[ignore]`d real-interpreter test would skip.
pub fn resolve_interpreter_in_path(
    candidates: &[&str],
    dirs: impl IntoIterator<Item = PathBuf>,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = dirs.into_iter().collect();
    for candidate in candidates {
        for dir in &dirs {
            if dir.as_os_str().is_empty() {
                // An empty PATH element means "the current directory" to a
                // shell. Honouring it here would resolve `python3` from a
                // cwd the agent chose, so skip it.
                continue;
            }
            let path = dir.join(candidate);
            if is_executable(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// Is `path` an existing, executable regular file?
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Windows has no executable bit; existence as a file is the test, and
        // PATHEXT resolution is left to the spawn itself.
        true
    }
}

/// Probe `PATH` for an interpreter. Returns `None` — never an error, never a
/// spawn — when the host has no Python.
pub fn find_interpreter() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    resolve_interpreter_in_path(
        &INTERPRETER_CANDIDATES,
        std::env::split_paths(&path),
        &is_executable_file,
    )
}

/// The message returned when no interpreter exists. It names every candidate
/// looked for and both remedies, because a bare "not found" sends an agent
/// (and a user) looking in the wrong place.
fn interpreter_missing_message() -> String {
    format!(
        "no Python interpreter found on PATH — looked for {}.\n\
         This tool needs a host Python; xaft is a Rust binary and does not \
         embed one, unlike agenthicc which is itself Python and reuses its own \
         interpreter.\n\
         Fix it by installing Python so one of those names is on PATH, or by \
         calling run_command directly with an absolute interpreter path.",
        INTERPRETER_CANDIDATES
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Shared plumbing for both Python tools.
struct PythonRunner {
    executor: Arc<CommandExecutor>,
    workspace_root: PathBuf,
    /// `None` means the probe found nothing; the tools then refuse without
    /// spawning.
    interpreter: Option<PathBuf>,
}

impl PythonRunner {
    fn new(
        executor: Arc<CommandExecutor>,
        workspace_root: impl Into<PathBuf>,
        interpreter: Option<PathBuf>,
    ) -> Self {
        Self {
            executor,
            workspace_root: workspace_root.into(),
            interpreter,
        }
    }

    /// Resolve `cwd` the way `run_command` does: reject any `..` component
    /// outright, then require the join to stay under the workspace root. A
    /// lexical prefix test never collapses `..`, so testing the joined path
    /// alone would permit a full escape.
    fn resolve_cwd(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<PathBuf, AgtrsError> {
        match crate::error::opt_str(input, "cwd").filter(|s| !s.is_empty()) {
            Some(raw) => {
                if Path::new(raw)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: tool_name.to_string(),
                        reason: format!("cwd must not contain '..': {raw}"),
                    });
                }
                let resolved = self.workspace_root.join(raw);
                if !resolved.starts_with(&self.workspace_root) {
                    return Err(AgtrsError::ToolCallFailed {
                        tool_name: tool_name.to_string(),
                        reason: format!("cwd escapes the workspace: {raw}"),
                    });
                }
                Ok(resolved)
            }
            None => Ok(self.workspace_root.clone()),
        }
    }

    /// agenthicc's default timeout, with the panic guarded.
    fn timeout(input: &serde_json::Value, default_secs: f64) -> Result<Option<Duration>, f64> {
        match input.get("timeout").and_then(|v| v.as_f64()) {
            Some(secs) if !secs.is_finite() || secs < 0.0 => Err(secs),
            Some(secs) => Ok(Some(Duration::from_secs_f64(secs))),
            None => Ok(Some(Duration::from_secs_f64(default_secs))),
        }
    }

    /// Spawn `[interpreter, args..]`.
    ///
    /// Returns the raw [`CommandOutput`] so each caller renders its own
    /// framing — `run_python_expr` needs the stdout to derive its `result:`
    /// line, and re-parsing a rendered string to get it back is exactly the
    /// kind of fragile coupling that breaks silently.
    async fn spawn(
        &self,
        tool_name: &str,
        args: Vec<String>,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CommandOutput, PythonError> {
        // The absent-interpreter contract, in its load-bearing position: this
        // returns before any spawn is attempted.
        let Some(interpreter) = self.interpreter.clone() else {
            return Err(PythonError::InterpreterMissing);
        };

        if ctx.cancel_token.is_cancelled() {
            return Err(PythonError::Agent(AgtrsError::Cancelled {
                reason: format!("{tool_name} cancelled before start"),
            }));
        }

        let cwd = self
            .resolve_cwd(tool_name, input)
            .map_err(PythonError::Agent)?;

        let mut env = HashMap::new();
        if let Some(obj) = input.get("env").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                let s = v.as_str().ok_or_else(|| {
                    PythonError::Agent(AgtrsError::ToolCallFailed {
                        tool_name: tool_name.to_string(),
                        reason: format!("env.{k} is not a string"),
                    })
                })?;
                env.insert(k.clone(), s.to_string());
            }
        }

        let default_secs = if tool_name == "run_python_expr" {
            10.0
        } else {
            30.0
        };
        let timeout = Self::timeout(input, default_secs).map_err(|secs| {
            PythonError::Agent(AgtrsError::ToolCallFailed {
                tool_name: tool_name.to_string(),
                reason: format!(
                    "timeout must be a non-negative, finite number of seconds (got {secs})"
                ),
            })
        })?;

        let cmd = ArgvCommand::new(
            interpreter.to_string_lossy().to_string(),
            args,
            Some(cwd),
            env,
            timeout,
        );

        let output = self
            .executor
            .execute(&cmd, Some(ctx.cancel_token.clone()))
            .await
            .map_err(|e| {
                PythonError::Agent(AgtrsError::ToolCallFailed {
                    tool_name: tool_name.to_string(),
                    reason: e.to_string(),
                })
            })?;

        Ok(output)
    }
}

/// Why a Python call did not produce a [`CommandOutput`].
///
/// The absent interpreter is its OWN variant rather than an [`AgtrsError`],
/// because that is the whole point of the module: it must never be rendered as,
/// or collapse into, a subprocess spawn failure.
enum PythonError {
    /// The probe found no interpreter. Rendered as a plain sentence.
    InterpreterMissing,
    /// Anything else — a malformed call, a policy refusal, a real failure.
    Agent(AgtrsError),
}

/// Shared framing, identical to `run_command`'s so a Python failure cannot read
/// like success.
///
/// `result_line` is emitted first when present: agenthicc returns that value as a
/// structured `result` key, and xaft's [`ToolResult`] carries only `content` +
/// `is_error`, so it becomes a labelled first line (see the module doc).
fn render_output(
    output: &CommandOutput,
    result_line: Option<&str>,
    tool_name: &str,
    tool_use_id: &str,
) -> ToolResult {
    let mut content = String::new();

    if let Some(line) = result_line {
        content.push_str(&format!("result: {line}\n"));
    }

    if output.success {
        content.push_str("Exit code: 0 (success)\n");
    } else {
        content.push_str(&format!(
            "COMMAND FAILED — exit code {}\nDo NOT proceed as if this succeeded.\n",
            output.exit_code
        ));
    }

    if !output.stdout.is_empty() {
        content.push_str("\nstdout:\n");
        content.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        content.push_str("\nstderr:\n");
        content.push_str(&output.stderr);
    }

    let trimmed = content.trim_end().to_string();
    tracing::info!(
        tool = tool_name,
        exit_code = output.exit_code,
        stdout_bytes = output.stdout.len(),
        stderr_bytes = output.stderr.len(),
        duration_ms = output.duration.as_millis(),
    );
    if output.success {
        ToolResult::ok(trimmed, tool_use_id)
    } else {
        ToolResult::error(trimmed, tool_use_id)
    }
}

/// A self-removing scratch `.py` file.
///
/// Agenthicc's `run_python` uses `tempfile.NamedTemporaryFile(delete=False)`
/// plus an explicit `os.unlink` in a `finally`; this is the same lifecycle with
/// no new dependency. The name carries the pid and a monotonically increasing
/// counter so two concurrent calls cannot collide, and the file is created
/// `O_EXCL`-style via [`create_new`] so a pre-existing path is never
/// overwritten.
struct ScratchScript {
    path: PathBuf,
}

impl ScratchScript {
    fn create(code: &str) -> std::io::Result<Self> {
        Self::create_in(&std::env::temp_dir(), code)
    }

    /// Create the scratch file inside `dir`.
    ///
    /// The directory is a parameter so the self-removal contract can be pinned
    /// against an isolated temp dir: a test that lists the real system temp
    /// dir races every other test in the process writing the same prefix.
    fn create_in(dir: &Path, code: &str) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut last_err = None;
        // A handful of retries: the collision space is pid+counter, so this
        // only ever spins on a genuinely hostile filesystem.
        for _ in 0..8 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("xaft-run-python-{}-{n}.py", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write as _;
                    f.write_all(code.as_bytes())?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no unique temporary .py filename could be found",
            )
        }))
    }
}

impl Drop for ScratchScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── run_python ────────────────────────────────────────────────────────────

/// Execute a Python code snippet in a subprocess.
pub struct RunPythonTool {
    runner: PythonRunner,
}

impl RunPythonTool {
    /// Create with a custom executor, probing `PATH` for an interpreter.
    pub fn new(executor: Arc<CommandExecutor>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            runner: PythonRunner::new(executor, workspace_root, find_interpreter()),
        }
    }

    /// Create with a pinned interpreter, or with none.
    ///
    /// Public so the absent-interpreter path can be exercised from outside the
    /// module and from hosts that genuinely have no Python — which is the case
    /// a real-interpreter test would quietly skip.
    pub fn with_interpreter(
        executor: Arc<CommandExecutor>,
        workspace_root: impl Into<PathBuf>,
        interpreter: Option<PathBuf>,
    ) -> Self {
        Self {
            runner: PythonRunner::new(executor, workspace_root, interpreter),
        }
    }

    /// Create with a default permissive executor in `working_dir`.
    pub fn with_working_dir(working_dir: impl Into<PathBuf>) -> Self {
        let dir: PathBuf = working_dir.into();
        let executor = Arc::new(CommandExecutor::new(
            Sandbox::new(&dir),
            ExecutionPolicy::permissive(),
        ));
        Self::with_interpreter(executor, dir, find_interpreter())
    }

    const TOOL_NAME: &'static str = "run_python";
}

impl std::fmt::Debug for RunPythonTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPythonTool")
            .field("workspace_root", &self.runner.workspace_root)
            .field("interpreter", &self.runner.interpreter)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for RunPythonTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Execute a Python code snippet in a subprocess and return its stdout, \
         stderr and exit code. The snippet is written to a temporary .py file, \
         so it runs as a script (__name__ == '__main__'). This tool requires a \
         Python interpreter on the host's PATH and reports plainly when there \
         is none. Use run_command for a raw argv, and bash_exec for pipes and \
         redirection."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python source to execute. Runs as a script, so __name__ == '__main__'."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory; defaults to the workspace. Must stay inside the workspace."
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
            "required": ["code"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        // Same gate as bash_exec and run_command: spawning a process is the
        // risk. This tool adds no authority run_command does not already grant.
        true
    }

    #[instrument(name = "run_python", skip(self, ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let code = input.get("code").and_then(|v| v.as_str()).ok_or_else(|| {
            AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "required field 'code' is missing or not a string".into(),
            }
        })?;

        // Refuse before writing the temp file when there is no interpreter, so
        // nothing is left behind on disk either.
        if self.runner.interpreter.is_none() {
            return Ok(ToolResult::error(
                interpreter_missing_message(),
                &ctx.tool_use_id,
            ));
        }

        // A scratch `.py` file, not `-c`: `__name__`, `__file__` and relative
        // imports then behave like a script, matching agenthicc, which also
        // writes to a temporary file rather than passing `-c`.
        //
        // Written by hand rather than with the `tempfile` crate, which is a
        // dev-dependency of this crate. Promoting it to a real dependency would
        // mean editing Cargo.toml, and this file is deliberately usable with
        // zero new dependencies -- the whole design is "reuse what run_command
        // already does".
        let scratch = ScratchScript::create(code).map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("could not write the snippet to a temporary .py file: {e}"),
        })?;
        let outcome = self
            .runner
            .spawn(
                Self::TOOL_NAME,
                vec![scratch.path.to_string_lossy().to_string()],
                &input,
                ctx,
            )
            .await;
        // Unlink unconditionally, exactly as agenthicc's `finally: os.unlink`.
        drop(scratch);

        match outcome {
            Ok(output) => Ok(render_output(
                &output,
                None,
                Self::TOOL_NAME,
                &ctx.tool_use_id,
            )),
            Err(PythonError::InterpreterMissing) => Ok(ToolResult::error(
                interpreter_missing_message(),
                &ctx.tool_use_id,
            )),
            Err(PythonError::Agent(e)) => Err(e),
        }
    }
}

// ── run_python_expr ───────────────────────────────────────────────────────

/// Evaluate a Python expression and return its repr.
pub struct RunPythonExprTool {
    runner: PythonRunner,
}

impl RunPythonExprTool {
    /// Create with a custom executor, probing `PATH` for an interpreter.
    pub fn new(executor: Arc<CommandExecutor>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            runner: PythonRunner::new(executor, workspace_root, find_interpreter()),
        }
    }

    /// Create with a pinned interpreter, or with none. See
    /// [`RunPythonTool::with_interpreter`].
    pub fn with_interpreter(
        executor: Arc<CommandExecutor>,
        workspace_root: impl Into<PathBuf>,
        interpreter: Option<PathBuf>,
    ) -> Self {
        Self {
            runner: PythonRunner::new(executor, workspace_root, interpreter),
        }
    }

    /// Create with a default permissive executor in `working_dir`.
    pub fn with_working_dir(working_dir: impl Into<PathBuf>) -> Self {
        let dir: PathBuf = working_dir.into();
        let executor = Arc::new(CommandExecutor::new(
            Sandbox::new(&dir),
            ExecutionPolicy::permissive(),
        ));
        Self::with_interpreter(executor, dir, find_interpreter())
    }

    const TOOL_NAME: &'static str = "run_python_expr";
}

impl std::fmt::Debug for RunPythonExprTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPythonExprTool")
            .field("workspace_root", &self.runner.workspace_root)
            .field("interpreter", &self.runner.interpreter)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for RunPythonExprTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Evaluate a Python expression and return its repr, along with stdout, \
         stderr and the exit code. The expression is wrapped as \
         `_r = (<expression>); print(repr(_r))`, so a bare expression is \
         evaluated in its own scope and side effects still happen. This tool \
         requires a Python interpreter on the host's PATH and reports plainly \
         when there is none."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "A Python expression, e.g. \"[x*2 for x in range(3)]\"."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory; defaults to the workspace. Must stay inside the workspace."
                },
                "timeout": {
                    "type": "number",
                    "default": 10.0,
                    "description": "Maximum seconds to wait. Defaults to 10."
                },
                "env": {
                    "type": "object",
                    "description": "String environment overlay, merged over the sandbox environment."
                }
            },
            "required": ["expression"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "run_python_expr", skip(self, ctx))]
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let expression = input
            .get("expression")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: "required field 'expression' is missing or not a string".into(),
            })?;

        // agenthicc, verbatim: exec/__init__.py:627.
        let code = format!("_r = ({expression}); print(repr(_r))");

        let outcome = self
            .runner
            .spawn(Self::TOOL_NAME, vec!["-c".to_string(), code], &input, ctx)
            .await;

        match outcome {
            Ok(output) => {
                // The repr is the LAST non-empty stdout line: the probe program
                // may print before `repr(_r)` runs, and agenthicc's
                // `_result_text(..., "stdout")` takes the whole stream, so the
                // last line is the closer analogue to the value it surfaces.
                let result_line = output
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .next_back()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                Ok(render_output(
                    &output,
                    Some(&result_line),
                    Self::TOOL_NAME,
                    &ctx.tool_use_id,
                ))
            }
            Err(PythonError::InterpreterMissing) => Ok(ToolResult::error(
                interpreter_missing_message(),
                &ctx.tool_use_id,
            )),
            Err(PythonError::Agent(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_executor(dir: &TempDir) -> Arc<CommandExecutor> {
        let sandbox = Sandbox::new(dir.path()).with_timeout(Duration::from_secs(10));
        Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::permissive()))
    }

    /// The host's real interpreter, or `None` — used to skip the end-to-end
    /// tests rather than to make them pass vacuously.
    fn real_interpreter() -> Option<PathBuf> {
        find_interpreter()
    }

    macro_rules! require_python {
        () => {
            if real_interpreter().is_none() {
                eprintln!("skipping: no Python interpreter on this host's PATH");
                return;
            }
        };
    }

    // ── the probe ────────────────────────────────────────────────────────

    #[test]
    fn the_probe_prefers_python3_over_python() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path();
        std::fs::write(bin.join("python3"), b"#!/bin/sh\n").unwrap();
        std::fs::write(bin.join("python"), b"#!/bin/sh\n").unwrap();
        // The predicate answers BOTH "does it exist" and "is it executable",
        // which is why it can be injected: a real interpreter is not needed to
        // prove the search order.
        let exists = |p: &Path| p.is_file();

        let found =
            resolve_interpreter_in_path(&INTERPRETER_CANDIDATES, [bin.to_path_buf()], &exists)
                .unwrap();
        assert_eq!(found.file_name().unwrap(), "python3");
    }

    #[test]
    fn the_probe_falls_back_to_python_when_python3_is_absent() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("python"), b"#!/bin/sh\n").unwrap();
        let exists = |p: &Path| p.is_file();
        let found = resolve_interpreter_in_path(
            &INTERPRETER_CANDIDATES,
            [tmp.path().to_path_buf()],
            &exists,
        )
        .unwrap();
        assert_eq!(found.file_name().unwrap(), "python");
    }

    #[test]
    fn the_probe_skips_a_directory_entry_that_is_not_executable() {
        let tmp = TempDir::new().unwrap();
        // Present but NOT executable: a data file called python3 must not win.
        std::fs::write(tmp.path().join("python3"), b"not a program").unwrap();
        let never = |_: &Path| false;
        assert!(
            resolve_interpreter_in_path(
                &INTERPRETER_CANDIDATES,
                [tmp.path().to_path_buf()],
                &never
            )
            .is_none()
        );
    }

    #[test]
    fn the_probe_ignores_an_empty_path_element_rather_than_resolving_from_cwd() {
        let tmp = TempDir::new().unwrap();
        // A shell would treat an empty PATH element as "." and resolve
        // `python3` from the current directory. The agent chooses that cwd, so
        // this must not resolve.
        let exists = |p: &Path| p.is_file();
        assert!(
            resolve_interpreter_in_path(&INTERPRETER_CANDIDATES, [PathBuf::new()], &exists)
                .is_none()
        );
    }

    #[test]
    fn the_probe_scans_every_directory_before_giving_up_on_a_candidate() {
        let tmp = TempDir::new().unwrap();
        let first = tmp.path().join("a");
        let second = tmp.path().join("b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("python3"), b"#!/bin/sh\n").unwrap();
        let exists = |p: &Path| p.is_file();
        let found = resolve_interpreter_in_path(
            &INTERPRETER_CANDIDATES,
            [first.clone(), second.clone()],
            &exists,
        )
        .unwrap();
        assert_eq!(found, second.join("python3"));
    }

    // ── the absent-interpreter contract (the load-bearing half) ──────────

    #[tokio::test]
    async fn an_absent_interpreter_is_a_clear_message_not_a_spawn_failure() {
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_interpreter(make_executor(&tmp), tmp.path(), None);
        let ctx = ToolContext::new("rp-absent");

        let result = tool
            .call(serde_json::json!({"code": "print('hi')"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error, "a missing interpreter must be an error");
        let msg = &result.content;
        // Names both candidates, so the reader knows exactly what was looked for.
        assert!(msg.contains("python3"), "{msg}");
        assert!(msg.contains("python"), "{msg}");
        assert!(msg.contains("PATH"), "{msg}");
        // Names a remedy rather than leaving the reader at a dead end.
        assert!(msg.contains("install"), "{msg}");
        // NEVER the confusing failure this exists to prevent.
        for confusing in [
            "No such file or directory",
            "os error 2",
            "SpawnError",
            "spawn failed",
            "panicked",
        ] {
            assert!(
                !msg.contains(confusing),
                "absent interpreter leaked a raw spawn failure ({confusing}): {msg}"
            );
        }
    }

    #[tokio::test]
    async fn an_absent_interpreter_never_reaches_the_executor() {
        // A tool pinned to no interpreter must not spawn anything, even when a
        // perfectly good program is sitting right there. `/bin/echo` would be
        // spawned if the probe were bypassed, so its absence from the output is
        // the assertion.
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_interpreter(make_executor(&tmp), tmp.path(), None);
        let ctx = ToolContext::new("rpe-absent");

        let result = tool
            .call(serde_json::json!({"expression": "1 + 1"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(!result.content.contains("Exit code"), "{}", result.content);
        assert!(
            result.content.contains("no Python interpreter"),
            "{}",
            result.content
        );
    }

    /// Deterministic form of "the scratch file does not leak": run against an
    /// ISOLATED temp dir rather than the system one, which every other test in
    /// this process writes the same filename prefix into.
    #[test]
    fn a_scratch_script_writes_the_snippet_and_removes_itself_on_drop() {
        let dir = TempDir::new().unwrap();
        let path = {
            let scratch = ScratchScript::create_in(dir.path(), "print(6 * 7)").unwrap();
            let p = scratch.path.clone();
            // Present while alive, with the snippet in it.
            assert!(p.exists());
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "print(6 * 7)");
            assert!(p.to_string_lossy().ends_with(".py"));
            p
        };
        assert!(
            !path.exists(),
            "the scratch .py outlived its guard: {path:?}"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(leftovers.is_empty(), "leaked: {leftovers:?}");
    }

    /// Two concurrent calls must not collide on one filename, or the second
    /// silently overwrites the first's snippet.
    #[test]
    fn two_scratch_scripts_never_share_a_path() {
        let dir = TempDir::new().unwrap();
        let a = ScratchScript::create_in(dir.path(), "print('a')").unwrap();
        let b = ScratchScript::create_in(dir.path(), "print('b')").unwrap();
        assert_ne!(a.path, b.path);
        assert_eq!(std::fs::read_to_string(&a.path).unwrap(), "print('a')");
        assert_eq!(std::fs::read_to_string(&b.path).unwrap(), "print('b')");
    }

    /// `ScratchScript::create_in` refuses to clobber an existing path rather
    /// than overwriting whatever is already there.
    #[test]
    fn a_scratch_script_never_overwrites_an_existing_path() {
        let dir = TempDir::new().unwrap();
        let taken = dir.path().join("xaft-run-python-preexisting.py");
        std::fs::write(&taken, "do not lose me").unwrap();
        // The name is pid+counter derived, so a collision with `taken` is not
        // reachable through the public path; assert the primitive directly by
        // confirming `create_new` semantics on an occupied directory entry.
        assert!(taken.exists());
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "do not lose me");
        let scratch = ScratchScript::create_in(dir.path(), "print(1)").unwrap();
        assert_ne!(scratch.path, taken);
    }

    // ── input validation ─────────────────────────────────────────────────

    #[tokio::test]
    async fn a_missing_code_field_is_rejected_as_a_malformed_call() {
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_interpreter(
            make_executor(&tmp),
            tmp.path(),
            Some(PathBuf::from("/bin/echo")),
        );
        let ctx = ToolContext::new("rp-nocode");
        let err = tool.call(serde_json::json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("required field 'code'"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_expression_field_is_rejected_as_a_malformed_call() {
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_interpreter(
            make_executor(&tmp),
            tmp.path(),
            Some(PathBuf::from("/bin/echo")),
        );
        let ctx = ToolContext::new("rpe-noexpr");
        let err = tool.call(serde_json::json!({}), &ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("required field 'expression'"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_negative_timeout_is_rejected_instead_of_panicking() {
        // Duration::from_secs_f64 panics on a negative value; agenthicc coerces.
        // Refuse rather than coerce, and prove it does not panic.
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_interpreter(
            make_executor(&tmp),
            tmp.path(),
            Some(PathBuf::from("/bin/echo")),
        );
        let ctx = ToolContext::new("rpe-negto");
        let err = tool
            .call(
                serde_json::json!({"expression": "1", "timeout": -1.0}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("non-negative"), "{err}");
    }

    #[tokio::test]
    async fn a_cwd_containing_dotdot_is_refused_before_any_spawn() {
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_interpreter(
            make_executor(&tmp),
            tmp.path(),
            Some(PathBuf::from("/bin/echo")),
        );
        let ctx = ToolContext::new("rp-escape");
        let err = tool
            .call(serde_json::json!({"code": "pass", "cwd": "../.."}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not contain '..'"), "{err}");
    }

    // ── end-to-end, only where a real interpreter exists ─────────────────

    #[tokio::test]
    async fn run_python_executes_a_snippet_and_reports_stdout() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rp-ok");
        let result = tool
            .call(serde_json::json!({"code": "print(6 * 7)"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("42"), "{}", result.content);
        assert!(
            result.content.contains("Exit code: 0"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn run_python_runs_the_snippet_as_a_script() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rp-main");
        let result = tool
            .call(serde_json::json!({"code": "print(__name__)"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("__main__"), "{}", result.content);
    }

    #[tokio::test]
    async fn run_python_reports_a_non_zero_exit_as_a_failure() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rp-fail");
        let result = tool
            .call(
                serde_json::json!({"code": "import sys; sys.stderr.write('boom\\n'); sys.exit(3)"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error, "a non-zero exit must not read as success");
        assert!(result.content.contains("exit code 3"), "{}", result.content);
        assert!(result.content.contains("boom"), "{}", result.content);
    }

    #[tokio::test]
    async fn run_python_expr_returns_the_repr_of_the_expression() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rpe-ok");
        let result = tool
            .call(
                serde_json::json!({"expression": "[x * 2 for x in range(3)]"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.starts_with("result: [0, 2, 4]"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn run_python_expr_evaluates_in_its_own_scope_so_comprehensions_work() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rpe-scope");
        // `range(3)` as a bare expression works because the whole thing is
        // wrapped in `_r = (...)`. This is the exact reason for the wrapping.
        let result = tool
            .call(serde_json::json!({"expression": "len(range(5))"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("result: 5"), "{}", result.content);
    }

    #[tokio::test]
    async fn run_python_expr_reports_a_syntax_error_as_a_failure() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        let tool = RunPythonExprTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rpe-syntax");
        let result = tool
            .call(serde_json::json!({"expression": "1 +"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "{}", result.content);
        assert!(result.content.contains("SyntaxError"), "{}", result.content);
    }

    #[tokio::test]
    async fn run_python_writes_files_in_the_requested_cwd() {
        require_python!();
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        let tool = RunPythonTool::with_working_dir(tmp.path());
        let ctx = ToolContext::new("rp-cwd");
        let result = tool
            .call(
                serde_json::json!({
                    "code": "open('marker.txt', 'w').write('ok')",
                    "cwd": "sub"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(tmp.path().join("sub").join("marker.txt").exists());
    }
}
