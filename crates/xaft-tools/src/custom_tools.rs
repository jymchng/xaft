//! Persistent custom tools — `.xaft/tools/<name>.toml` and
//! `~/.config/xaft/tools/<name>.toml`.
//!
//! agenthicc loads user tool plugins from `.agenthicc/tools/*.py` (and the
//! `make_agenthicc_tool` workflow authors them). xaft runs no Python, so a
//! custom tool is declarative: one TOML file naming a program to run.
//!
//! ```toml
//! # .xaft/tools/recent_commits.toml
//! name = "recent_commits"              # optional; must equal the file stem
//! description = "List the most recent commit subjects touching a path."
//! capability = "git_read"              # read search write execute git_read git_write network
//! command = "git log -n {count} --format=%s -- {path}"
//! timeout_s = 30                       # default 60, at most 3600
//!
//! [[params]]
//! name = "path"
//! description = "File or directory to inspect."
//!
//! [[params]]
//! name = "count"
//! description = "How many commits to list."
//! default = "10"                      # a parameter with a default is optional
//! ```
//!
//! # How a call runs
//!
//! `command` is split into argv **before** the parameter values are
//! substituted ([`crate::command_template`] — the same rule as CLI plugins),
//! so every value is exactly one argument: a value can never add a flag, a
//! second command or a redirection. The program word itself may not contain a
//! placeholder, so the model can never choose *what* runs. There is no shell.
//! The program runs in the session's working directory with stdin closed and
//! the declared timeout; stdout (bounded) is the result and a non-zero exit is
//! an error carrying stderr. Every call goes through the approval gate
//! (`requires_confirmation = true`).
//!
//! # Where tools come from, and trust
//!
//! 1. **user** — `~/.config/xaft/tools/` (the user config directory), trusted
//!    by location;
//! 2. **project** — `.xaft/tools/` in the working directory. A repository can
//!    ship these and they run programs with the user's permissions, so each
//!    file is loaded **only when trusted** through `xaft trust add`
//!    ([`crate::trust`]); editing it revokes the trust.
//!
//! A project tool shadows a user tool of the same name (with a warning).
//! Files whose name starts with `_` or `.` (drafts, backups) are never loaded,
//! nor is anything in subdirectories. A tool that would shadow a built-in tool
//! is refused. The declared capability classifies the tool for capability
//! filters ([`crate::capabilities::declare_custom_capability`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::capabilities::{ToolCapability, builtin_capability_of, declare_custom_capability};
use crate::command_template::{placeholders, render_argv, shell_split, valid_placeholder_name};
use crate::trust::{ExtensionTrust, user_config_dir};

/// The project directory holding custom tools, relative to the working
/// directory.
pub const PROJECT_TOOLS_DIR: &str = ".xaft/tools";

/// The trust-store kind of a custom tool file.
pub const TOOL_KIND: &str = "tool";

/// Seconds a call may run when the file sets no `timeout_s`.
pub const DEFAULT_TIMEOUT_S: u64 = 60;

/// The largest `timeout_s` a file may declare.
pub const MAX_TIMEOUT_S: u64 = 3600;

/// Bytes of stdout (and of stderr) a call returns; the rest is cut.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Most parameters one tool may declare.
pub const MAX_PARAMS: usize = 32;

/// The capabilities a custom tool may declare (agenthicc's tag set).
pub const CUSTOM_TOOL_CAPABILITIES: &[&str] = &[
    "read",
    "search",
    "write",
    "execute",
    "git_read",
    "git_write",
    "network",
];

/// The user-global custom tool directory: `~/.config/xaft/tools`.
pub fn user_tools_dir() -> Option<PathBuf> {
    user_config_dir().map(|dir| dir.join("tools"))
}

/// The project custom tool directory for `working_dir`.
pub fn project_tools_dir(working_dir: &Path) -> PathBuf {
    working_dir.join(PROJECT_TOOLS_DIR)
}

/// One string parameter of a custom tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolParam {
    /// The `{name}` placeholder and the schema property.
    pub name: String,
    /// Shown to the model.
    pub description: String,
    /// The value used when the call omits the parameter; `None` makes the
    /// parameter required.
    pub default: Option<String>,
}

impl CustomToolParam {
    /// Whether a call must supply the parameter.
    pub fn required(&self) -> bool {
        self.default.is_none()
    }
}

/// A validated custom tool definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolSpec {
    /// Tool name, `lower_snake_case`.
    pub name: String,
    /// Shown to the model.
    pub description: String,
    /// What the command does, for capability filters.
    pub capability: ToolCapability,
    /// The command template.
    pub command: String,
    /// Seconds before a call is killed.
    pub timeout_s: u64,
    /// Declared parameters, in order.
    pub params: Vec<CustomToolParam>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpec {
    name: Option<String>,
    #[serde(default)]
    description: String,
    capability: Option<String>,
    #[serde(default)]
    command: String,
    timeout_s: Option<i64>,
    #[serde(default)]
    params: Vec<RawParam>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    default: Option<String>,
}

/// Whether `name` is a valid custom tool name: `lower_snake_case`, starting
/// with a letter, at most 64 characters.
pub fn valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether a built-in tool already owns `name`.
pub fn is_builtin_tool_name(name: &str) -> bool {
    builtin_capability_of(name) != ToolCapability::Undeclared
}

/// Parse and validate one custom tool definition.
///
/// `file_stem` is the name of the file it came from; the declared `name`, if
/// any, must match it. Every problem is reported, not just the first.
pub fn parse_tool_spec(text: &str, file_stem: Option<&str>) -> Result<CustomToolSpec, Vec<String>> {
    let raw: RawSpec = toml::from_str(text).map_err(|e| vec![format!("not valid TOML: {e}")])?;
    let mut errors = Vec::new();

    let name = match (&raw.name, file_stem) {
        (Some(name), Some(stem)) if name != stem => {
            errors.push(format!(
                "name '{name}' must match the file name '{stem}.toml'"
            ));
            name.clone()
        }
        (Some(name), _) => name.clone(),
        (None, Some(stem)) => stem.to_string(),
        (None, None) => {
            errors.push("name is required".into());
            String::new()
        }
    };
    if !name.is_empty() && !valid_tool_name(&name) {
        errors.push(format!(
            "name '{name}' must be lower_snake_case (a letter first, at most 64 characters)"
        ));
    }
    if is_builtin_tool_name(&name) {
        errors.push(format!("name '{name}' is a built-in tool; choose another"));
    }
    if raw.description.trim().is_empty() {
        errors.push("description must not be empty".into());
    }

    let capability_name = raw.capability.clone().unwrap_or_else(|| "execute".into());
    let capability = match ToolCapability::parse(&capability_name) {
        Some(cap) if CUSTOM_TOOL_CAPABILITIES.contains(&cap.as_str()) => cap,
        _ => {
            errors.push(format!(
                "capability '{capability_name}' is unknown; use one of {}",
                CUSTOM_TOOL_CAPABILITIES.join(", ")
            ));
            ToolCapability::Execute
        }
    };

    let timeout_s = match raw.timeout_s {
        None => DEFAULT_TIMEOUT_S,
        Some(t) if (1..=MAX_TIMEOUT_S as i64).contains(&t) => t as u64,
        Some(t) => {
            errors.push(format!(
                "timeout_s = {t} must be between 1 and {MAX_TIMEOUT_S}"
            ));
            DEFAULT_TIMEOUT_S
        }
    };

    if raw.params.len() > MAX_PARAMS {
        errors.push(format!("at most {MAX_PARAMS} params may be declared"));
    }
    let mut params = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, p) in raw.params.iter().enumerate() {
        if !valid_placeholder_name(&p.name) {
            errors.push(format!(
                "params[{index}] name '{}' must be an identifier",
                p.name
            ));
            continue;
        }
        if !seen.insert(p.name.clone()) {
            errors.push(format!("param '{}' is declared twice", p.name));
            continue;
        }
        params.push(CustomToolParam {
            name: p.name.clone(),
            description: p.description.trim().to_string(),
            default: p.default.clone(),
        });
    }

    let command = raw.command.trim().to_string();
    if command.is_empty() {
        errors.push("command must not be empty".into());
    } else {
        check_command(&command, &params, &mut errors);
    }

    if errors.is_empty() {
        Ok(CustomToolSpec {
            name,
            description: raw.description.trim().to_string(),
            capability,
            command,
            timeout_s,
            params,
        })
    } else {
        Err(errors)
    }
}

/// The command-template rules: it splits, the program word is fixed, every
/// placeholder is declared, every parameter is used, and it renders.
fn check_command(command: &str, params: &[CustomToolParam], errors: &mut Vec<String>) {
    let words = match shell_split(command) {
        Ok(words) => words,
        Err(e) => {
            errors.push(format!("command does not split into arguments: {e}"));
            return;
        }
    };
    let declared: BTreeSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (index, word) in words.iter().enumerate() {
        match placeholders(word) {
            Ok(names) => {
                if index == 0 && !names.is_empty() {
                    errors.push(format!(
                        "the program word '{word}' must not contain a placeholder: a call may \
                         choose arguments, never the program"
                    ));
                }
                for name in names {
                    if !declared.contains(name.as_str()) {
                        errors.push(format!(
                            "command uses {{{name}}}, which is not a declared param"
                        ));
                    }
                    used.insert(name);
                }
            }
            Err(e) => errors.push(format!("command word '{word}': {e}")),
        }
    }
    for p in params {
        if !used.contains(&p.name) {
            errors.push(format!("param '{}' is never used in command", p.name));
        }
    }
    let sample: BTreeMap<String, String> = params
        .iter()
        .map(|p| (p.name.clone(), "x".to_string()))
        .collect();
    if let Err(e) = render_argv(command, &sample, "command") {
        if !errors.iter().any(|known| known.contains(&e)) {
            errors.push(e);
        }
    }
}

/// Read and validate one custom tool file.
pub fn load_tool_file(path: &Path) -> Result<CustomToolSpec, Vec<String>> {
    let text = std::fs::read_to_string(path).map_err(|e| vec![format!("unreadable: {e}")])?;
    let stem = path.file_stem().and_then(|s| s.to_str());
    parse_tool_spec(&text, stem)
}

/// Where a loaded custom tool came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomToolSource {
    /// `~/.config/xaft/tools/`.
    User,
    /// `.xaft/tools/` in the working directory.
    Project,
}

impl CustomToolSource {
    /// `user` or `project`, the badge `/tools` shows.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// A custom tool that passed trust and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCustomTool {
    /// The definition.
    pub spec: CustomToolSpec,
    /// Which directory it came from.
    pub source: CustomToolSource,
    /// The file.
    pub path: PathBuf,
}

/// The result of discovery: usable tools plus what the user must be told.
#[derive(Debug, Clone, Default)]
pub struct CustomToolDiscovery {
    /// Tools to register.
    pub tools: Vec<LoadedCustomTool>,
    /// Untrusted, invalid and shadowing files, one line each.
    pub warnings: Vec<String>,
}

impl CustomToolDiscovery {
    /// The definitions, for [`crate::registry::ToolRegistryBuilder::with_custom_tools`].
    pub fn specs(&self) -> Vec<CustomToolSpec> {
        self.tools.iter().map(|t| t.spec.clone()).collect()
    }
}

/// The candidate files of one tool directory, sorted.
pub fn tool_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            p.is_file()
                && p.extension().is_some_and(|e| e == "toml")
                && !name.starts_with('_')
                && !name.starts_with('.')
        })
        .collect();
    files.sort();
    files
}

/// Discover custom tools: the user directory, then the trusted project one.
///
/// Every accepted tool's capability is declared to the capability table.
pub fn discover(
    project_dir: Option<&Path>,
    user_dir: Option<&Path>,
    trust: &ExtensionTrust,
) -> CustomToolDiscovery {
    let mut discovery = CustomToolDiscovery::default();
    for (dir, source) in [
        (user_dir, CustomToolSource::User),
        (project_dir, CustomToolSource::Project),
    ] {
        let Some(dir) = dir else { continue };
        for path in tool_files(dir) {
            if let Err(refusal) = trust.permits(&path, TOOL_KIND) {
                discovery.warnings.push(format!(
                    "custom tool {} is not loaded: {} (review it, then run `xaft trust add {}`)",
                    path.display(),
                    refusal.reason,
                    path.display()
                ));
                continue;
            }
            let spec = match load_tool_file(&path) {
                Ok(spec) => spec,
                Err(errors) => {
                    discovery.warnings.push(format!(
                        "custom tool {} is invalid: {}",
                        path.display(),
                        errors.join("; ")
                    ));
                    continue;
                }
            };
            if let Some(existing) = discovery
                .tools
                .iter()
                .position(|t| t.spec.name == spec.name)
            {
                let shadowed = discovery.tools.remove(existing);
                discovery.warnings.push(format!(
                    "custom tool '{}' in {} shadows the one in {}",
                    spec.name,
                    path.display(),
                    shadowed.path.display()
                ));
            }
            discovery
                .tools
                .push(LoadedCustomTool { spec, source, path });
        }
    }
    for tool in &discovery.tools {
        declare_custom_capability(&tool.spec.name, tool.spec.capability);
    }
    discovery
}

/// [`discover`] for a session: `<working_dir>/.xaft/tools/` and
/// `~/.config/xaft/tools/`, judged by the user's trust store.
pub fn discover_default(working_dir: &Path) -> CustomToolDiscovery {
    let project = project_tools_dir(working_dir);
    let user = user_tools_dir();
    discover(Some(&project), user.as_deref(), &ExtensionTrust::for_user())
}

/// The outcome of one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolRun {
    /// The argv that ran.
    pub argv: Vec<String>,
    /// The exit code; `None` when killed (timeout, cancellation, signal).
    pub exit_code: Option<i32>,
    /// Bounded stdout.
    pub stdout: String,
    /// Bounded stderr, or why the program could not run.
    pub stderr: String,
    /// Whether the call succeeded (exit code 0).
    pub success: bool,
}

/// A custom tool bound to a working directory — what the registry holds.
#[derive(Debug, Clone)]
pub struct CustomTool {
    spec: CustomToolSpec,
    description: String,
    workspace_root: PathBuf,
}

impl CustomTool {
    /// Bind `spec` to `workspace_root`.
    pub fn new(spec: CustomToolSpec, workspace_root: impl Into<PathBuf>) -> Self {
        let description = format!(
            "{} (custom tool: runs `{}`)",
            spec.description,
            shell_split(&spec.command)
                .ok()
                .and_then(|w| w.into_iter().next())
                .unwrap_or_default()
        );
        Self {
            spec,
            description,
            workspace_root: workspace_root.into(),
        }
    }

    /// Erase for a tool list.
    pub fn erased(spec: CustomToolSpec, workspace_root: impl Into<PathBuf>) -> Arc<ErasedTool> {
        Arc::new(Self::new(spec, workspace_root)) as Arc<ErasedTool>
    }

    /// The definition.
    pub fn spec(&self) -> &CustomToolSpec {
        &self.spec
    }

    /// The argv a call with `input` runs, or why it cannot run.
    pub fn render(&self, input: &Value) -> Result<Vec<String>, String> {
        let mut values = BTreeMap::new();
        let mut missing = Vec::new();
        for p in &self.spec.params {
            let given = match input.get(&p.name) {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            match given.or_else(|| p.default.clone()) {
                Some(v) => {
                    values.insert(p.name.clone(), v);
                }
                None => missing.push(p.name.clone()),
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "{}() is missing required argument(s): {}",
                self.spec.name,
                missing.join(", ")
            ));
        }
        if let Some(obj) = input.as_object() {
            let unknown: Vec<&String> = obj
                .keys()
                .filter(|k| !self.spec.params.iter().any(|p| &p.name == *k))
                .collect();
            if !unknown.is_empty() {
                return Err(format!(
                    "{}() has no parameter(s): {}",
                    self.spec.name,
                    unknown
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        render_argv(&self.spec.command, &values, "command")
    }

    /// Run the tool once with `input`, honouring its timeout and `cancel`.
    pub async fn invoke(
        &self,
        input: &Value,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<CustomToolRun, String> {
        let argv = self.render(input)?;
        let mut command = tokio::process::Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&self.workspace_root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| format!("cannot run `{}`: {e}", argv[0]))?;
        let timeout = Duration::from_secs(self.spec.timeout_s);
        let wait = child.wait_with_output();
        let never = tokio_util::sync::CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        let output = tokio::select! {
            out = tokio::time::timeout(timeout, wait) => match out {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => return Err(format!("`{}` failed: {e}", argv[0])),
                Err(_) => {
                    return Ok(CustomToolRun {
                        argv,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: format!("timed out after {}s", timeout.as_secs()),
                        success: false,
                    });
                }
            },
            _ = cancel.cancelled() => return Err(format!("{}() was cancelled", self.spec.name)),
        };
        Ok(CustomToolRun {
            exit_code: output.status.code(),
            success: output.status.success(),
            stdout: bounded(&output.stdout),
            stderr: bounded(&output.stderr),
            argv,
        })
    }
}

/// At most [`MAX_OUTPUT_BYTES`] of `bytes` as text, marking a cut.
fn bounded(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_OUTPUT_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut text = String::from_utf8_lossy(&bytes[..MAX_OUTPUT_BYTES]).into_owned();
    text.push_str(&format!(
        "\n… [{} more bytes not shown]",
        bytes.len() - MAX_OUTPUT_BYTES
    ));
    text
}

#[async_trait::async_trait]
impl Tool for CustomTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &self.spec.params {
            let mut description = p.description.clone();
            if let Some(default) = &p.default {
                description.push_str(&format!(" (default: {default:?})"));
            }
            properties.insert(
                p.name.clone(),
                json!({ "type": "string", "description": description }),
            );
            if p.required() {
                required.push(json!(p.name));
            }
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("custom tool '{}' cancelled", self.spec.name),
            });
        }
        match self.invoke(&input, Some(&ctx.cancel_token)).await {
            Ok(run) if run.success => {
                let out = run.stdout.trim_end();
                Ok(ToolResult::ok(
                    if out.is_empty() { "(no output)" } else { out },
                    &ctx.tool_use_id,
                ))
            }
            Ok(run) => {
                let code = run
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "none".into());
                let mut text = format!("exit {code}: {}", run.stderr.trim());
                if !run.stdout.trim().is_empty() {
                    text.push_str(&format!("\nstdout:\n{}", run.stdout.trim_end()));
                }
                Ok(ToolResult::error(text, &ctx.tool_use_id))
            }
            Err(e) => Ok(ToolResult::error(e, &ctx.tool_use_id)),
        }
    }
}

/// Every file-format field, for authoring tools and help text.
pub const SCHEMA_DOC: &str = r#"xaft custom tool definition (TOML). One file per tool: .xaft/tools/<name>.toml (project, loaded only
once trusted with `xaft trust add`) or ~/.config/xaft/tools/<name>.toml (user, trusted by location).

  name = "recent_commits"          # optional; lower_snake_case; must equal the file stem; not a built-in tool name
  description = "..."              # required; shown to the model
  capability = "git_read"          # read | search | write | execute | git_read | git_write | network (default execute)
  command = "git log -n {count} --format=%s -- {path}"
                                   # ONE program and its arguments, no shell. Split into argv first (POSIX quoting:
                                   # '...' and "..."), then each {param} is replaced inside its word, so a value is
                                   # always exactly one argument. The program word may not contain a placeholder.
                                   # {{ and }} are literal braces. Every placeholder must be a declared param and
                                   # every param must be used.
  timeout_s = 30                   # seconds, 1..3600 (default 60)

  [[params]]                       # string parameters, in order (at most 32)
  name = "path"                    # identifier
  description = "..."              # shown to the model
  default = "."                    # optional; a param without a default is required

Calls run in the session's working directory with stdin closed; stdout (first 64 KiB) is the result, a non-zero
exit is an error with stderr. Every call asks the user for approval."#;

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
description = "Echo a greeting."
capability = "read"
command = "echo hello {who}"
timeout_s = 5

[[params]]
name = "who"
description = "Who to greet."
default = "world"
"#;

    fn spec(text: &str) -> CustomToolSpec {
        parse_tool_spec(text, Some("greet")).unwrap()
    }

    #[test]
    fn a_valid_file_parses_with_defaults_and_its_stem_as_name() {
        let s = spec(GOOD);
        assert_eq!(s.name, "greet");
        assert_eq!(s.capability, ToolCapability::Read);
        assert_eq!(s.timeout_s, 5);
        assert!(!s.params[0].required());
        let minimal =
            parse_tool_spec("description = \"d\"\ncommand = \"date\"\n", Some("clock")).unwrap();
        assert_eq!(
            minimal.capability,
            ToolCapability::Execute,
            "default capability"
        );
        assert_eq!(minimal.timeout_s, DEFAULT_TIMEOUT_S);
    }

    #[test]
    fn every_problem_is_reported() {
        let errors = parse_tool_spec(
            r#"
name = "Other"
capability = "teleport"
command = "{prog} --x {undeclared}"
timeout_s = 0
unknown_key = 1
"#,
            Some("greet"),
        )
        .unwrap_err();
        assert_eq!(errors.len(), 1, "unknown keys fail the parse: {errors:?}");
        assert!(errors[0].contains("unknown_key"), "{errors:?}");

        let errors = parse_tool_spec(
            r#"
name = "Other"
capability = "teleport"
command = "{prog} --x {undeclared}"
timeout_s = 0

[[params]]
name = "prog"

[[params]]
name = "unused"
"#,
            Some("greet"),
        )
        .unwrap_err();
        let text = errors.join("\n");
        for needle in [
            "must match the file name",
            "must be lower_snake_case",
            "description must not be empty",
            "capability 'teleport' is unknown",
            "timeout_s = 0",
            "program word",
            "{undeclared}",
            "'unused' is never used",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn a_builtin_name_is_refused() {
        let errors = parse_tool_spec("description = \"d\"\ncommand = \"ls\"\n", Some("read_file"))
            .unwrap_err();
        assert!(errors[0].contains("built-in tool"), "{errors:?}");
    }

    #[test]
    fn rendering_keeps_each_value_one_argument_and_checks_arguments() {
        let tool = CustomTool::new(spec(GOOD), ".");
        assert_eq!(
            tool.render(&json!({"who": "a b; rm -rf /"})).unwrap(),
            ["echo", "hello", "a b; rm -rf /"]
        );
        assert_eq!(tool.render(&json!({})).unwrap(), ["echo", "hello", "world"]);
        assert!(tool.render(&json!({"what": "x"})).is_err());

        let required = CustomTool::new(
            parse_tool_spec(
                "description = \"d\"\ncommand = \"echo {x}\"\n[[params]]\nname = \"x\"\n",
                Some("needs"),
            )
            .unwrap(),
            ".",
        );
        assert!(
            required
                .render(&json!({}))
                .unwrap_err()
                .contains("missing required argument(s): x")
        );
        assert_eq!(required.schema()["required"], json!(["x"]));
        assert!(required.requires_confirmation());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_runs_the_program_without_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CustomTool::new(spec(GOOD), dir.path());
        let ok = tool
            .call(json!({"who": "$(id)"}), &ToolContext::new("t"))
            .await
            .unwrap();
        assert!(!ok.is_error);
        assert_eq!(ok.content, "hello $(id)", "no shell expansion");

        let failing = CustomTool::new(
            parse_tool_spec(
                "description = \"d\"\ncommand = \"ls /definitely/not/here\"\n",
                Some("fails"),
            )
            .unwrap(),
            dir.path(),
        );
        let err = failing
            .call(json!({}), &ToolContext::new("t"))
            .await
            .unwrap();
        assert!(err.is_error);
        assert!(err.content.starts_with("exit "), "{}", err.content);

        let missing = CustomTool::new(
            parse_tool_spec(
                "description = \"d\"\ncommand = \"xaft-no-such-program-xyz\"\n",
                Some("absent"),
            )
            .unwrap(),
            dir.path(),
        );
        let err = missing
            .call(json!({}), &ToolContext::new("t"))
            .await
            .unwrap();
        assert!(err.content.contains("cannot run"), "{}", err.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_that_overruns_its_timeout_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CustomTool::new(
            parse_tool_spec(
                "description = \"d\"\ncommand = \"sleep 5\"\ntimeout_s = 1\n",
                Some("slow"),
            )
            .unwrap(),
            dir.path(),
        );
        let started = std::time::Instant::now();
        let run = tool.invoke(&json!({}), None).await.unwrap();
        assert!(!run.success);
        assert!(run.stderr.contains("timed out after 1s"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    fn policy(dir: &Path) -> ExtensionTrust {
        ExtensionTrust::new(dir.join("home/trust.json"), vec![dir.join("home")])
    }

    #[test]
    fn project_tools_load_only_when_trusted_and_user_tools_by_location() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let user = dir.path().join("home/tools");
        let project = dir.path().join("project/.xaft/tools");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(project.join("_drafts")).unwrap();
        std::fs::write(user.join("xaft_test_user_tool.toml"), GOOD).unwrap();
        std::fs::write(project.join("xaft_test_project_tool.toml"), GOOD).unwrap();
        std::fs::write(project.join("_draft.toml"), GOOD).unwrap();
        std::fs::write(project.join("_drafts/xaft_test_draft.toml"), GOOD).unwrap();

        let found = discover(Some(&project), Some(&user), &trust);
        let names: Vec<&str> = found.tools.iter().map(|t| t.spec.name.as_str()).collect();
        assert_eq!(names, ["xaft_test_user_tool"]);
        assert_eq!(found.tools[0].source, CustomToolSource::User);
        assert_eq!(found.warnings.len(), 1, "{:?}", found.warnings);
        assert!(
            found.warnings[0].contains("not trusted"),
            "{:?}",
            found.warnings
        );
        assert!(found.warnings[0].contains("xaft trust add"));
        assert_eq!(
            crate::capabilities::capability_of("xaft_test_user_tool"),
            ToolCapability::Read,
            "the declared capability classifies the tool"
        );
    }

    #[test]
    fn an_invalid_trusted_file_is_reported_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let trust = policy(dir.path());
        let user = dir.path().join("home/tools");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("broken.toml"), "command = \"\"\n").unwrap();
        let found = discover(None, Some(&user), &trust);
        assert!(found.tools.is_empty());
        assert!(
            found.warnings[0].contains("is invalid"),
            "{:?}",
            found.warnings
        );
    }
}
