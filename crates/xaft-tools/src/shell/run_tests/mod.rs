//! `RunTestsTool` — run the project's test suite and return structured results.
//!
//! # What agenthicc ships, and what this keeps
//!
//! agenthicc's `run_tests` (`tools/exec/__init__.py`, `RunTestsTool`) is a
//! pytest runner: `framework="pytest"` (or `"unittest"`), `path="tests/"`,
//! `args`, `timeout=120`. It runs `python -m pytest <path> --json-report
//! --json-report-file=<tmp> -q <args>`, reads `summary.passed/failed/error`
//! from the report and falls back to `(\d+) passed` / `(\d+) failed` over
//! stdout, then returns the command outcome plus `passed`, `failed` and
//! `errors` (`null` when nothing could be read).
//!
//! xaft keeps the tool name, the four parameters, their defaults where they
//! still mean something, the JSON report with its regex fallback, and the
//! `null`-when-unknown counts — and answers for the ecosystems an xaft user
//! actually has:
//!
//! | `framework` | command | report |
//! |---|---|---|
//! | `cargo` | `cargo test --no-fail-fast` | libtest summaries, panics, rustc diagnostics |
//! | `pytest` | `python -m pytest <path> --json-report … -q` | the JSON report, else the summary line |
//! | `unittest` | `python -m unittest discover <path>` | `Ran N tests` / `FAILED (…)` |
//! | `npm` / `yarn` / `pnpm` | `<pm> test` | jest, vitest, mocha or `node --test` output |
//! | `go` | `go test -json ./...` | the JSON event stream |
//!
//! `framework` defaults to `auto`, which reads the project files (see
//! [`detect_framework`]).
//!
//! # Safety
//!
//! A test suite is arbitrary code, so this sits exactly where `run_command`
//! does: behind the approval gate ([`Tool::requires_confirmation`]), through
//! the same [`CommandExecutor`] (execution policy, sandbox environment, output
//! cap), with no shell, and with `cwd` and `path` confined to the workspace by
//! the same `..`-rejecting rule.
//!
//! # Documented divergences from agenthicc
//!
//! - **`framework` defaults to `auto`**, not `pytest`; an unknown project is a
//!   plain refusal naming the frameworks that can be forced.
//! - **`path` defaults to the runner's own scope.** pytest and unittest still
//!   use `tests/` when that directory exists (agenthicc's default); otherwise
//!   pytest runs its configured `testpaths` instead of failing on a missing
//!   directory.
//! - **A missing `pytest-json-report` plugin is not fatal.** agenthicc passes
//!   `--json-report` unconditionally, so without the plugin pytest exits with a
//!   usage error and no test runs; here the run is repeated once without the
//!   flag and the output parser is used.
//! - **More fields**: `skipped`, `total` and `failures` (name + message) are
//!   added, and `stdout`/`stderr` are the *tails* of the output (where every
//!   runner prints its summary), bounded so a verbose suite cannot flood the
//!   context.
//! - **`timeout = 0`** means no tool deadline, as in agenthicc, but is still
//!   capped at 24 hours: the shared executor falls back to its sandbox limit
//!   when given no deadline at all.
//! - **`cwd`** is accepted (workspace-confined), for monorepos whose test
//!   suite lives in a subdirectory.

pub mod report;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::instrument;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_shell::CommandExecutor;
use agtrs_shell::error::ShellError;

use super::run_command::ArgvCommand;
use report::TestReport;

/// agenthicc's default `timeout`, seconds.
pub const DEFAULT_TIMEOUT_S: f64 = 120.0;
/// What `timeout = 0` ("no deadline") becomes.
const NO_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);
/// Characters kept from the end of stdout and of stderr.
pub const OUTPUT_TAIL_CHARS: usize = 8_000;

/// A test runner `run_tests` knows how to drive and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    /// `cargo test`.
    Cargo,
    /// `python -m pytest`.
    Pytest,
    /// `python -m unittest`.
    Unittest,
    /// `npm test`.
    Npm,
    /// `yarn test`.
    Yarn,
    /// `pnpm test`.
    Pnpm,
    /// `go test`.
    Go,
}

impl Framework {
    /// Every framework, in the order `framework` documents them.
    pub const ALL: [Framework; 7] = [
        Self::Cargo,
        Self::Pytest,
        Self::Unittest,
        Self::Npm,
        Self::Yarn,
        Self::Pnpm,
        Self::Go,
    ];

    /// The `framework` argument naming this runner.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Pytest => "pytest",
            Self::Unittest => "unittest",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
            Self::Go => "go",
        }
    }

    /// Parse a `framework` argument (`auto` is not a framework).
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.as_str() == name)
    }
}

/// Pick a runner for `run_dir`.
///
/// A file `path` decides by its extension (`.rs`, `.py`, `.go`, a JavaScript
/// or TypeScript test file). Otherwise the project files decide, first match
/// wins: `Cargo.toml` → cargo, `go.mod` → go, Python markers (`pytest.ini`,
/// `conftest.py`, `pyproject.toml`, `setup.cfg`, `tox.ini`, `setup.py`) →
/// pytest, `package.json` → the package manager its lockfile names
/// (`pnpm-lock.yaml`, `yarn.lock`, else npm).
pub fn detect_framework(run_dir: &Path, path_hint: Option<&Path>) -> Option<Framework> {
    if let Some(hint) = path_hint {
        match hint.extension().and_then(|e| e.to_str()) {
            Some("rs") => return Some(Framework::Cargo),
            Some("py") => return Some(Framework::Pytest),
            Some("go") => return Some(Framework::Go),
            Some("js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx") => {
                return Some(node_manager(run_dir));
            }
            _ => {}
        }
    }
    let has = |name: &str| run_dir.join(name).exists();
    if has("Cargo.toml") {
        Some(Framework::Cargo)
    } else if has("go.mod") {
        Some(Framework::Go)
    } else if [
        "pytest.ini",
        "conftest.py",
        "pyproject.toml",
        "setup.cfg",
        "tox.ini",
        "setup.py",
    ]
    .iter()
    .any(|m| has(m))
    {
        Some(Framework::Pytest)
    } else if has("package.json") {
        Some(node_manager(run_dir))
    } else {
        None
    }
}

fn node_manager(run_dir: &Path) -> Framework {
    if run_dir.join("pnpm-lock.yaml").exists() {
        Framework::Pnpm
    } else if run_dir.join("yarn.lock").exists() {
        Framework::Yarn
    } else {
        Framework::Npm
    }
}

/// The command a run will spawn.
#[derive(Debug, Clone, PartialEq)]
pub struct TestCommand {
    /// `argv[0]`.
    pub program: String,
    /// `argv[1..]`.
    pub args: Vec<String>,
    /// Environment overlay (colour off, non-interactive).
    pub env: HashMap<String, String>,
    /// pytest's JSON report file, when one was requested.
    pub json_report: Option<PathBuf>,
}

impl TestCommand {
    fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// Build the command for `framework`.
///
/// `path` is relative to `run_dir` and already confined to the workspace;
/// `extra` is the caller's `args`, appended last so they can override.
pub fn plan_command(
    framework: Framework,
    run_dir: &Path,
    path: Option<&str>,
    extra: &[String],
    python: Option<&Path>,
) -> Result<TestCommand, String> {
    let mut env = HashMap::new();
    env.insert("NO_COLOR".to_string(), "1".to_string());
    let mut json_report = None;

    let (program, mut args): (String, Vec<String>) = match framework {
        Framework::Cargo => {
            env.insert("CARGO_TERM_COLOR".into(), "never".into());
            let mut args = vec!["test".to_string(), "--no-fail-fast".to_string()];
            if let Some(path) = path {
                args.extend(cargo_selection(run_dir, path)?);
            }
            ("cargo".to_string(), args)
        }
        Framework::Pytest | Framework::Unittest => {
            let python = python.ok_or_else(|| {
                "no Python interpreter found on PATH — looked for 'python3', 'python'. \
                 Install Python, or run the suite with run_command and an absolute \
                 interpreter path."
                    .to_string()
            })?;
            env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
            let default_dir = run_dir.join("tests").is_dir().then(|| "tests/".to_string());
            let target = path.map(str::to_string).or(default_dir);
            let args = if framework == Framework::Pytest {
                let report = std::env::temp_dir().join(format!(
                    "xaft-pytest-report-{}.json",
                    uuid::Uuid::new_v4().simple()
                ));
                let mut args = vec!["-m".to_string(), "pytest".to_string()];
                args.extend(target);
                args.push("--json-report".to_string());
                args.push(format!("--json-report-file={}", report.display()));
                args.push("-q".to_string());
                json_report = Some(report);
                args
            } else {
                let target = target.unwrap_or_else(|| ".".to_string());
                if target.ends_with(".py") {
                    vec!["-m".into(), "unittest".into(), target]
                } else {
                    vec!["-m".into(), "unittest".into(), "discover".into(), target]
                }
            };
            (python.to_string_lossy().into_owned(), args)
        }
        Framework::Npm | Framework::Yarn | Framework::Pnpm => {
            env.insert("CI".into(), "true".into());
            env.insert("FORCE_COLOR".into(), "0".into());
            let mut args = vec!["test".to_string()];
            let mut passthrough: Vec<String> = path.map(str::to_string).into_iter().collect();
            passthrough.extend(extra.iter().cloned());
            if framework == Framework::Npm && !passthrough.is_empty() {
                // npm keeps everything before `--` for itself.
                args.push("--".to_string());
            }
            args.extend(passthrough);
            return Ok(TestCommand {
                program: framework.as_str().to_string(),
                args,
                env,
                json_report,
            });
        }
        Framework::Go => {
            // Go tests packages, so a file selects its directory.
            let pattern = match path.map(|p| p.trim_end_matches('/')) {
                None | Some("" | ".") => "./...".to_string(),
                Some(p) if run_dir.join(p).is_file() => {
                    match Path::new(p).parent().map(|d| d.to_string_lossy()) {
                        Some(d) if !d.is_empty() => format!("./{d}"),
                        _ => ".".to_string(),
                    }
                }
                Some(p) => format!("./{p}/..."),
            };
            (
                "go".to_string(),
                vec!["test".to_string(), "-json".to_string(), pattern],
            )
        }
    };
    args.extend(extra.iter().cloned());
    Ok(TestCommand {
        program,
        args,
        env,
        json_report,
    })
}

/// Cargo arguments selecting what `path` names: a package directory, an
/// integration-test file or directory, or a source file (whose module path
/// becomes the test-name filter).
fn cargo_selection(run_dir: &Path, path: &str) -> Result<Vec<String>, String> {
    let target = run_dir.join(path);
    if !target.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    // The nearest package: the path itself or an ancestor with a Cargo.toml,
    // never above the run directory.
    let package_dir = target
        .ancestors()
        .take_while(|a| a.starts_with(run_dir))
        .find(|a| a.join("Cargo.toml").is_file())
        .ok_or_else(|| format!("no Cargo.toml at or above {path}"))?;
    let mut args = vec![
        "--manifest-path".to_string(),
        package_dir
            .join("Cargo.toml")
            .to_string_lossy()
            .into_owned(),
    ];
    let inside = target.strip_prefix(package_dir).unwrap_or(Path::new(""));
    let mut parts = inside.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    match parts.next() {
        Some("tests") => {
            if target.is_file() {
                // tests/foo.rs and tests/foo/main.rs are both the `foo` target.
                let rest: Vec<&str> = parts.collect();
                let name = match rest.as_slice() {
                    [file] => file.trim_end_matches(".rs").to_string(),
                    [dir, ..] => (*dir).to_string(),
                    [] => String::new(),
                };
                if !name.is_empty() {
                    args.push("--test".into());
                    args.push(name);
                }
            } else {
                args.push("--test".into());
                args.push("*".into());
            }
        }
        Some("src") if target.is_file() => {
            let has_lib = package_dir.join("src/lib.rs").is_file();
            args.push(if has_lib { "--lib" } else { "--bins" }.into());
            let mut module: Vec<String> = parts.map(str::to_string).collect();
            if let Some(last) = module.last_mut() {
                *last = last.trim_end_matches(".rs").to_string();
            }
            if matches!(
                module.last().map(String::as_str),
                Some("lib" | "main" | "mod")
            ) {
                module.pop();
            }
            if !module.is_empty() {
                args.push(format!("{}::", module.join("::")));
            }
        }
        _ => {}
    }
    Ok(args)
}

/// Resolve a workspace-relative path argument, refusing traversal.
fn confine(root: &Path, raw: &str, what: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(raw);
    if candidate
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(format!("{what} must not contain '..': {raw}"));
    }
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    if !resolved.starts_with(root) {
        return Err(format!("{what} escapes the workspace: {raw}"));
    }
    Ok(resolved)
}

/// `text`'s last `max` characters, with a marker when anything was dropped.
fn tail(text: &str, max: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max {
        return (text.to_string(), false);
    }
    let skip = total - max;
    let cut = text
        .char_indices()
        .nth(skip)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    (
        format!("[… {skip} earlier characters omitted]\n{}", &text[cut..]),
        true,
    )
}

/// Run a test suite and report pass/fail/skip counts plus each failure.
pub struct RunTestsTool {
    executor: Arc<CommandExecutor>,
    workspace_root: PathBuf,
    /// The Python interpreter probe, taken once at construction (as
    /// `run_python` does).
    python: Option<PathBuf>,
}

impl RunTestsTool {
    /// The LLM-facing name — agenthicc's.
    pub const TOOL_NAME: &'static str = "run_tests";

    /// Build over `executor`, probing `PATH` for a Python interpreter.
    pub fn new(executor: Arc<CommandExecutor>, workspace_root: impl Into<PathBuf>) -> Self {
        Self::with_python(
            executor,
            workspace_root,
            super::run_python::find_interpreter(),
        )
    }

    /// Build with an explicit interpreter (or none) — for tests.
    pub fn with_python(
        executor: Arc<CommandExecutor>,
        workspace_root: impl Into<PathBuf>,
        python: Option<PathBuf>,
    ) -> Self {
        Self {
            executor,
            workspace_root: workspace_root.into(),
            python,
        }
    }

    fn refuse(&self, message: impl Into<String>, extra: Value, tool_use_id: &str) -> ToolResult {
        let mut body = json!({"ok": false, "error": message.into()});
        if let (Some(obj), Some(more)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                obj.insert(k.clone(), v.clone());
            }
        }
        ToolResult::error(pretty(&body), tool_use_id)
    }
}

impl std::fmt::Debug for RunTestsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunTestsTool")
            .field("workspace_root", &self.workspace_root)
            .field("python", &self.python)
            .finish_non_exhaustive()
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn bad_input(reason: impl Into<String>) -> AgtrsError {
    AgtrsError::ToolCallFailed {
        tool_name: RunTestsTool::TOOL_NAME.to_string(),
        reason: reason.into(),
    }
}

/// Did pytest reject `--json-report` because the plugin is not installed?
fn json_report_unsupported(stdout: &str, stderr: &str) -> bool {
    let needle = "unrecognized arguments: --json-report";
    stderr.contains(needle) || stdout.contains(needle)
}

#[async_trait]
impl Tool for RunTestsTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        Self::TOOL_NAME
    }

    fn description(&self) -> &str {
        "Run the test suite and return pass/fail counts plus every failing test's \
         name and message. Detects the runner from the project files (cargo, \
         pytest, unittest, npm/yarn/pnpm for jest, vitest, mocha or node --test, \
         and go) unless `framework` says which. `path` narrows the run to a test \
         file or directory; `args` are passed to the runner after xaft's own. \
         Counts are null when the run never got far enough to report them."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Test file or directory, relative to the workspace (default: \
                                    the runner's own scope; `tests/` for pytest and unittest when it exists)."
                },
                "framework": {
                    "type": "string",
                    "enum": ["auto", "cargo", "pytest", "unittest", "npm", "yarn", "pnpm", "go"],
                    "default": "auto",
                    "description": "Test runner to use; `auto` detects it from the project files."
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Extra arguments passed to the test runner."
                },
                "timeout": {
                    "type": "number",
                    "default": DEFAULT_TIMEOUT_S,
                    "minimum": 0,
                    "description": "Maximum seconds to wait (default 120; 0 = no limit)."
                },
                "cwd": {
                    "type": "string",
                    "description": "Directory to run in, relative to the workspace (default: the workspace root)."
                }
            },
            "additionalProperties": false
        })
    }

    /// Running a test suite runs arbitrary project code.
    fn requires_confirmation(&self) -> bool {
        true
    }

    #[instrument(name = "run_tests", skip_all)]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before start", Self::TOOL_NAME),
            });
        }

        // ── arguments ────────────────────────────────────────────────────
        let run_dir = match crate::error::opt_str(&input, "cwd").filter(|s| !s.trim().is_empty()) {
            Some(raw) => confine(&self.workspace_root, raw, "cwd").map_err(bad_input)?,
            None => self.workspace_root.clone(),
        };
        if !run_dir.is_dir() {
            return Ok(self.refuse(
                format!("cwd is not a directory: {}", run_dir.display()),
                json!({}),
                &ctx.tool_use_id,
            ));
        }
        let path = crate::error::opt_str(&input, "path")
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != ".");
        if let Some(p) = path {
            let full = confine(&run_dir, p, "path").map_err(bad_input)?;
            if !full.exists() {
                return Ok(self.refuse(
                    format!("path does not exist: {p}"),
                    json!({}),
                    &ctx.tool_use_id,
                ));
            }
        }
        let extra: Vec<String> = match input.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| bad_input("tool argument 'args' must be a list of strings"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(bad_input("tool argument 'args' must be a list of strings")),
        };
        let timeout = match input.get("timeout") {
            None | Some(Value::Null) => Duration::from_secs_f64(DEFAULT_TIMEOUT_S),
            Some(v) => match v.as_f64() {
                // `0` means no tool deadline (still capped, see NO_DEADLINE).
                Some(secs) if secs.is_finite() && secs >= 0.0 => {
                    if secs > 0.0 {
                        Duration::from_secs_f64(secs).min(NO_DEADLINE)
                    } else {
                        NO_DEADLINE
                    }
                }
                _ => {
                    return Err(bad_input(format!(
                        "timeout must be a finite, non-negative number of seconds (got {v})"
                    )));
                }
            },
        };
        let requested = crate::error::opt_str(&input, "framework")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("auto");
        let framework = if requested.eq_ignore_ascii_case("auto") {
            match detect_framework(&run_dir, path.map(Path::new)) {
                Some(f) => f,
                None => {
                    return Ok(self.refuse(
                        format!(
                            "could not detect a test framework in {}",
                            run_dir.display()
                        ),
                        json!({
                            "fix": "Pass framework as one of: cargo, pytest, unittest, npm, yarn, pnpm, go."
                        }),
                        &ctx.tool_use_id,
                    ));
                }
            }
        } else {
            Framework::parse(requested).ok_or_else(|| {
                bad_input(format!(
                    "unknown framework '{requested}'; expected auto, cargo, pytest, unittest, \
                     npm, yarn, pnpm or go"
                ))
            })?
        };

        let mut command =
            match plan_command(framework, &run_dir, path, &extra, self.python.as_deref()) {
                Ok(c) => c,
                Err(reason) => {
                    return Ok(self.refuse(
                        reason,
                        json!({"framework": framework.as_str()}),
                        &ctx.tool_use_id,
                    ));
                }
            };

        // ── run (pytest: once more without the JSON report if the plugin is
        //    missing) ────────────────────────────────────────────────────
        let mut outcome = self.execute(&command, &run_dir, timeout, ctx).await?;
        let plugin_missing = framework == Framework::Pytest
            && matches!(&outcome, Ok(out) if out.exit_code == 4
                && json_report_unsupported(&out.stdout, &out.stderr));
        if plugin_missing {
            if let Some(file) = command.json_report.take() {
                let _ = std::fs::remove_file(&file);
            }
            command
                .args
                .retain(|a| a != "--json-report" && !a.starts_with("--json-report-file="));
            outcome = self.execute(&command, &run_dir, timeout, ctx).await?;
        }
        // agenthicc's `_consume_json_report`: read, then always delete.
        let json_doc = command.json_report.as_ref().and_then(|file| {
            let doc = std::fs::read_to_string(file)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok());
            let _ = std::fs::remove_file(file);
            doc
        });

        let base = json!({
            "framework": framework.as_str(),
            "argv": command.argv(),
            "cwd": run_dir.display().to_string(),
        });

        let output = match outcome {
            Ok(output) => output,
            Err(ShellError::Timeout { timeout_ms, .. }) => {
                let mut body = base;
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("ok".into(), json!(false));
                    obj.insert("timed_out".into(), json!(true));
                    obj.insert("returncode".into(), json!(-1));
                    obj.insert(
                        "error".into(),
                        json!(format!(
                            "the test run timed out after {:.1}s",
                            timeout_ms as f64 / 1000.0
                        )),
                    );
                    for (k, v) in TestReport::default()
                        .to_json()
                        .as_object()
                        .into_iter()
                        .flatten()
                    {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                return Ok(ToolResult::error(pretty(&body), &ctx.tool_use_id));
            }
            Err(other) => {
                let hint = match &other {
                    ShellError::SpawnError { program, .. } => {
                        format!(" — is `{program}` installed and on PATH?")
                    }
                    _ => String::new(),
                };
                return Ok(self.refuse(format!("{other}{hint}"), base, &ctx.tool_use_id));
            }
        };

        let report = match framework {
            Framework::Cargo => report::parse_cargo(&output.stdout, &output.stderr),
            Framework::Pytest => json_doc
                .as_ref()
                .and_then(report::parse_pytest_json)
                .unwrap_or_else(|| report::parse_pytest_output(&output.stdout, &output.stderr)),
            Framework::Unittest => report::parse_unittest(&output.stdout, &output.stderr),
            Framework::Npm | Framework::Yarn | Framework::Pnpm => {
                report::parse_node(&output.stdout, &output.stderr)
            }
            Framework::Go => report::parse_go(&output.stdout, &output.stderr),
        };

        let (stdout, out_cut) = tail(&output.stdout, OUTPUT_TAIL_CHARS);
        let (stderr, err_cut) = tail(&output.stderr, OUTPUT_TAIL_CHARS);
        let mut body = base;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("ok".into(), json!(output.success));
            obj.insert("returncode".into(), json!(output.exit_code));
            obj.insert(
                "duration_ms".into(),
                json!((output.duration.as_secs_f64() * 1000.0 * 10.0).round() / 10.0),
            );
            obj.insert("timed_out".into(), json!(false));
            for (k, v) in report.to_json().as_object().into_iter().flatten() {
                obj.insert(k.clone(), v.clone());
            }
            obj.insert("stdout".into(), json!(stdout));
            obj.insert("stderr".into(), json!(stderr));
            obj.insert("truncated".into(), json!(out_cut || err_cut));
        }
        tracing::info!(
            framework = framework.as_str(),
            exit_code = output.exit_code,
            passed = ?report.passed,
            failed = ?report.failed,
            "run_tests"
        );
        let text = pretty(&body);
        Ok(if output.success {
            ToolResult::ok(text, &ctx.tool_use_id)
        } else {
            ToolResult::error(text, &ctx.tool_use_id)
        })
    }
}

impl RunTestsTool {
    /// One spawn through the shared executor. `Err` only for cancellation;
    /// every other executor failure is returned for the caller to report.
    async fn execute(
        &self,
        command: &TestCommand,
        run_dir: &Path,
        timeout: Duration,
        ctx: &ToolContext,
    ) -> Result<Result<agtrs_shell::CommandOutput, ShellError>, AgtrsError> {
        let cmd = ArgvCommand::new(
            command.program.clone(),
            command.args.clone(),
            Some(run_dir.to_path_buf()),
            command.env.clone(),
            Some(timeout),
        );
        match self
            .executor
            .execute(&cmd, Some(ctx.cancel_token.clone()))
            .await
        {
            Err(ShellError::Cancelled { .. }) => Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", Self::TOOL_NAME),
            }),
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_shell::{ExecutionPolicy, Sandbox};
    use tempfile::TempDir;

    fn tool(root: &Path, python: Option<PathBuf>) -> RunTestsTool {
        let sandbox = Sandbox::new(root).with_timeout(Duration::from_secs(60));
        let executor = Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::permissive()));
        RunTestsTool::with_python(executor, root, python)
    }

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    async fn call(tool: &RunTestsTool, input: Value) -> (Value, bool) {
        let result = tool
            .call(input, &ToolContext::new("t1"))
            .await
            .expect("tool call");
        let body: Value = serde_json::from_str(&result.content).unwrap_or(Value::Null);
        (body, result.is_error)
    }

    #[test]
    fn detection_follows_project_files_then_path_extensions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        assert_eq!(detect_framework(root, None), None);
        touch(root, "package.json");
        assert_eq!(detect_framework(root, None), Some(Framework::Npm));
        touch(root, "yarn.lock");
        assert_eq!(detect_framework(root, None), Some(Framework::Yarn));
        touch(root, "pnpm-lock.yaml");
        assert_eq!(detect_framework(root, None), Some(Framework::Pnpm));
        touch(root, "pyproject.toml");
        assert_eq!(detect_framework(root, None), Some(Framework::Pytest));
        touch(root, "go.mod");
        assert_eq!(detect_framework(root, None), Some(Framework::Go));
        touch(root, "Cargo.toml");
        assert_eq!(detect_framework(root, None), Some(Framework::Cargo));
        assert_eq!(
            detect_framework(root, Some(Path::new("tests/test_x.py"))),
            Some(Framework::Pytest),
            "a file path's extension wins over the project files"
        );
        assert_eq!(
            detect_framework(root, Some(Path::new("src/a.test.ts"))),
            Some(Framework::Pnpm)
        );
    }

    #[test]
    fn cargo_paths_select_packages_tests_and_modules() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "Cargo.toml");
        touch(root, "crates/foo/Cargo.toml");
        touch(root, "crates/foo/src/lib.rs");
        touch(root, "crates/foo/src/net/client.rs");
        touch(root, "crates/foo/tests/api.rs");
        let manifest = root.join("crates/foo/Cargo.toml").display().to_string();

        let plan = |p: &str| {
            plan_command(Framework::Cargo, root, Some(p), &[], None)
                .unwrap()
                .args
        };
        assert_eq!(
            plan("crates/foo"),
            ["test", "--no-fail-fast", "--manifest-path", &manifest]
        );
        assert_eq!(
            plan("crates/foo/tests/api.rs"),
            [
                "test",
                "--no-fail-fast",
                "--manifest-path",
                &manifest,
                "--test",
                "api"
            ]
        );
        assert_eq!(
            plan("crates/foo/tests"),
            [
                "test",
                "--no-fail-fast",
                "--manifest-path",
                &manifest,
                "--test",
                "*"
            ]
        );
        assert_eq!(
            plan("crates/foo/src/net/client.rs"),
            [
                "test",
                "--no-fail-fast",
                "--manifest-path",
                &manifest,
                "--lib",
                "net::client::"
            ]
        );
        let bare = plan_command(Framework::Cargo, root, None, &["-q".into()], None).unwrap();
        assert_eq!(bare.args, ["test", "--no-fail-fast", "-q"]);
        assert_eq!(
            bare.env.get("CARGO_TERM_COLOR").map(String::as_str),
            Some("never")
        );
    }

    #[test]
    fn python_runners_need_an_interpreter_and_default_to_tests_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let err = plan_command(Framework::Pytest, root, None, &[], None).unwrap_err();
        assert!(err.contains("no Python interpreter"), "{err}");

        let py = Path::new("/usr/bin/python3");
        let no_tests_dir = plan_command(Framework::Pytest, root, None, &[], Some(py)).unwrap();
        assert_eq!(&no_tests_dir.args[..2], ["-m", "pytest"]);
        assert_eq!(
            no_tests_dir.args[2], "--json-report",
            "no path without a tests/ dir"
        );
        assert!(no_tests_dir.json_report.is_some());

        std::fs::create_dir(root.join("tests")).unwrap();
        let with_dir = plan_command(
            Framework::Pytest,
            root,
            None,
            &["-k".into(), "x".into()],
            Some(py),
        )
        .unwrap();
        assert_eq!(with_dir.args[2], "tests/", "agenthicc's default path");
        assert_eq!(&with_dir.args[with_dir.args.len() - 3..], ["-q", "-k", "x"]);

        let unittest = plan_command(Framework::Unittest, root, None, &[], Some(py)).unwrap();
        assert_eq!(unittest.args, ["-m", "unittest", "discover", "tests/"]);
    }

    #[test]
    fn node_and_go_commands() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "pkg/x/x_test.go");
        let npm = plan_command(Framework::Npm, root, Some("src"), &["--ci".into()], None).unwrap();
        assert_eq!(npm.program, "npm");
        assert_eq!(npm.args, ["test", "--", "src", "--ci"]);
        assert_eq!(npm.env.get("CI").map(String::as_str), Some("true"));
        let yarn = plan_command(Framework::Yarn, root, None, &[], None).unwrap();
        assert_eq!(yarn.args, ["test"]);

        let go = |p: Option<&str>| {
            plan_command(Framework::Go, root, p, &[], None)
                .unwrap()
                .args
        };
        assert_eq!(go(None), ["test", "-json", "./..."]);
        assert_eq!(go(Some("pkg")), ["test", "-json", "./pkg/..."]);
        assert_eq!(go(Some("pkg/x/x_test.go")), ["test", "-json", "./pkg/x"]);
    }

    #[test]
    fn tails_keep_the_end_and_say_so() {
        let (kept, cut) = tail("abcdef", 10);
        assert_eq!((kept.as_str(), cut), ("abcdef", false));
        let (kept, cut) = tail("abcdef", 2);
        assert!(cut);
        assert!(kept.ends_with("\nef"), "{kept}");
        assert!(kept.starts_with("[… 4 earlier characters omitted]"));
    }

    #[tokio::test]
    async fn traversal_and_bad_arguments_are_refused_before_spawning() {
        let tmp = TempDir::new().unwrap();
        let t = tool(tmp.path(), None);
        for input in [
            json!({"cwd": "../elsewhere"}),
            json!({"path": "../../etc"}),
            json!({"args": "not-a-list"}),
            json!({"args": [1, 2]}),
            json!({"timeout": -1}),
            json!({"framework": "maven"}),
        ] {
            let err = t.call(input.clone(), &ToolContext::new("t")).await;
            assert!(err.is_err(), "{input} must be refused");
        }
    }

    #[tokio::test]
    async fn an_undetectable_project_is_a_plain_refusal() {
        let tmp = TempDir::new().unwrap();
        let (body, is_error) = call(&tool(tmp.path(), None), json!({})).await;
        assert!(is_error);
        assert_eq!(body["ok"], json!(false));
        assert!(body["error"].as_str().unwrap().contains("could not detect"));
        assert!(body["fix"].as_str().unwrap().contains("cargo"));
    }

    #[tokio::test]
    async fn a_python_suite_without_python_is_refused_by_name() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "pytest.ini");
        let (body, is_error) = call(&tool(tmp.path(), None), json!({})).await;
        assert!(is_error);
        assert_eq!(body["framework"], json!("pytest"));
        assert!(body["error"].as_str().unwrap().contains("python3"));
    }

    /// End to end through the real executor: a fake `pytest` interpreter that
    /// rejects `--json-report` (the plugin is missing), then prints a normal
    /// run. The tool must retry without the flag and parse the output.
    #[cfg(unix)]
    #[tokio::test]
    async fn pytest_without_the_json_plugin_is_retried_and_parsed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "pytest.ini");
        let fake = root.join("fake-python");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
               if [ \"$a\" = \"--json-report\" ]; then\n\
                 echo 'pytest: error: unrecognized arguments: --json-report' >&2; exit 4\n\
               fi\n\
             done\n\
             echo 'FAILED tests/test_a.py::test_b - assert 1 == 2'\n\
             echo '1 failed, 3 passed in 0.05s'\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (body, is_error) = call(&tool(root, Some(fake)), json!({})).await;
        assert!(is_error, "a failing suite is reported as a failed call");
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["returncode"], json!(1));
        assert_eq!(body["passed"], json!(3));
        assert_eq!(body["failed"], json!(1));
        assert_eq!(body["report"], json!("output"));
        assert_eq!(
            body["failures"][0]["name"],
            json!("tests/test_a.py::test_b")
        );
        let argv: Vec<&str> = body["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            !argv.iter().any(|a| a.starts_with("--json-report")),
            "{argv:?}"
        );
    }

    /// The JSON report, when the plugin writes one, is the source of truth.
    #[cfg(unix)]
    #[tokio::test]
    async fn pytest_json_report_is_read_and_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        touch(root, "conftest.py");
        let fake = root.join("fake-python");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
               case \"$a\" in --json-report-file=*) f=\"${a#--json-report-file=}\";; esac\n\
             done\n\
             printf '%s' '{\"summary\": {\"passed\": 4, \"skipped\": 1}, \"tests\": []}' > \"$f\"\n\
             echo 'this text says 99 failed but the report wins'\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (body, is_error) = call(&tool(root, Some(fake)), json!({"timeout": 30})).await;
        assert!(!is_error, "{body}");
        assert_eq!(body["report"], json!("structured"));
        assert_eq!(
            (
                body["passed"].clone(),
                body["failed"].clone(),
                body["skipped"].clone()
            ),
            (json!(4), json!(0), json!(1))
        );
        let report_arg = body["argv"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find_map(|a| a.strip_prefix("--json-report-file="))
            .expect("report path in argv")
            .to_string();
        assert!(
            !Path::new(&report_arg).exists(),
            "the report file is deleted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_runner_names_itself() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "go.mod");
        let t = {
            let sandbox = Sandbox::new(tmp.path())
                .with_env("PATH", "/nonexistent-xaft-bin")
                .with_timeout(Duration::from_secs(10));
            let executor = Arc::new(CommandExecutor::new(sandbox, ExecutionPolicy::permissive()));
            RunTestsTool::with_python(executor, tmp.path(), None)
        };
        let (body, is_error) = call(&t, json!({})).await;
        assert!(is_error);
        assert_eq!(body["framework"], json!("go"));
        assert!(
            body["error"].as_str().unwrap().contains("`go` installed"),
            "{body}"
        );
    }

    #[test]
    fn the_tool_is_gated_and_named_like_agenthiccs() {
        let tmp = TempDir::new().unwrap();
        let t = tool(tmp.path(), None);
        assert_eq!(t.name(), "run_tests");
        assert!(t.requires_confirmation());
        let schema = t.schema();
        for key in ["path", "framework", "args", "timeout"] {
            assert!(
                schema["properties"].get(key).is_some(),
                "agenthicc's `{key}`"
            );
        }
        assert_eq!(schema["properties"]["timeout"]["default"], json!(120.0));
    }
}
