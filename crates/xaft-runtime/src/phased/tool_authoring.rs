//! `custom_tool_authoring` — make_xaft_tool's authoring surface
//! (agenthicc's `workflows/make_agenthicc_tool/runner.py`).
//!
//! agenthicc asks its VALIDATE agent to compile and import the generated
//! plugin. An xaft custom tool is data, so the same checks are deterministic
//! here and shared with the loader ([`xaft_tools::custom_tools`]):
//!
//! | tool | what it does |
//! |---|---|
//! | `describe_custom_tool_schema` | every field of a `.xaft/tools/<name>.toml` |
//! | `list_custom_tools` | the project and user tools, trusted or not |
//! | `custom_tool_draft_path(name)` | `.xaft/tools/_drafts/<name>.toml` — never loaded |
//! | `validate_custom_tool_draft(path)` | the loader's validation, the plan, dependencies on PATH |
//! | `smoke_test_custom_tool(path, args)` | one real invocation with sample arguments (approval first) |
//!
//! The `tool_draft_valid` guard holds only when the draft's *current* bytes
//! validated and passed a smoke test; the effects record the plan
//! (`record_tool_plan`), the draft (`record_tool_draft`) and publish it
//! atomically (`publish_custom_tool`), keeping any previous version under
//! `.xaft/tools/_backups/`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};

use xaft_tools::custom_tools::{
    CUSTOM_TOOL_CAPABILITIES, CustomTool, CustomToolSpec, SCHEMA_DOC, discover_default,
    is_builtin_tool_name, load_tool_file, parse_tool_spec, project_tools_dir, valid_tool_name,
};
use xaft_tools::trust::sha256_hex;

use super::groups::RunData;

/// RunData key: the recorded plan.
const PLAN_KEY: &str = "tool_plan";
/// RunData key: the draft and what its validation and smoke test found.
const DRAFT_KEY: &str = "tool_draft";

/// What the group may touch.
#[derive(Clone)]
pub struct ToolAuthoringContext {
    /// The run's working directory; drafts and publication live under it.
    pub working_dir: PathBuf,
}

/// Where make_xaft_tool keeps drafts: never loaded.
pub fn draft_dir(working_dir: &Path) -> PathBuf {
    project_tools_dir(working_dir).join("_drafts")
}

fn arg(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `path` resolved against `working_dir`, and refused unless it lies inside
/// the draft directory (compared component-wise, so `..` cannot climb out).
fn draft_path(working_dir: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(raw);
    let path = if raw.is_absolute() {
        raw
    } else {
        working_dir.join(raw)
    };
    fn normalise(p: &Path) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str().to_string_lossy().into_owned()),
            }
        }
        out
    }
    let (p, d) = (normalise(&path), normalise(&draft_dir(working_dir)));
    if p.len() == d.len() + 1 && p.starts_with(&d) {
        Ok(d.iter()
            .chain(p.last())
            .fold(PathBuf::new(), |acc, seg| acc.join(seg)))
    } else {
        Err(format!(
            "drafts must be files directly under {}; got {}",
            draft_dir(working_dir).display(),
            path.display()
        ))
    }
}

/// The planned tool name, if a plan was recorded.
fn planned_name(data: &RunData) -> Option<String> {
    data.get(PLAN_KEY).and_then(|p| {
        p.get("tool_name")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn planned_dependencies(data: &RunData) -> Vec<String> {
    data.get(PLAN_KEY)
        .and_then(|p| p.get("dependencies").cloned())
        .and_then(|d| serde_json::from_value(d).ok())
        .unwrap_or_default()
}

/// Whether `program` resolves on PATH (or is an existing relative/absolute
/// path).
fn on_path(program: &str, working_dir: &Path) -> bool {
    if program.contains('/') {
        let p = Path::new(program);
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            working_dir.join(p)
        };
        return p.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// The `custom_tool_authoring` tools.
pub(crate) fn authoring_tools(ctx: &ToolAuthoringContext, data: &RunData) -> Vec<Arc<ErasedTool>> {
    [
        Kind::Describe,
        Kind::List,
        Kind::DraftPath,
        Kind::Validate,
        Kind::Smoke,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(AuthoringTool {
            kind,
            working_dir: ctx.working_dir.clone(),
            data: data.clone(),
        }) as Arc<ErasedTool>
    })
    .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Describe,
    List,
    DraftPath,
    Validate,
    Smoke,
}

struct AuthoringTool {
    kind: Kind,
    working_dir: PathBuf,
    data: RunData,
}

impl AuthoringTool {
    fn list(&self) -> String {
        let found = discover_default(&self.working_dir);
        let mut lines: Vec<String> = found
            .tools
            .iter()
            .map(|t| {
                format!(
                    "{} [{}, {}] — {} ({})",
                    t.spec.name,
                    t.source.as_str(),
                    t.spec.capability,
                    t.spec.description,
                    t.path.display()
                )
            })
            .collect();
        lines.extend(found.warnings.iter().map(|w| format!("not loaded: {w}")));
        if lines.is_empty() {
            "(no custom tools yet)".into()
        } else {
            lines.join("\n")
        }
    }

    fn draft_path_for(&self, name: &str) -> Result<String, String> {
        if !valid_tool_name(name) {
            return Err(format!("'{name}' is not a lower_snake_case tool name"));
        }
        if is_builtin_tool_name(name) {
            return Err(format!("'{name}' is a built-in tool; choose another name"));
        }
        let dir = draft_dir(&self.working_dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(format!(
            "Write the custom tool to: {}",
            dir.join(format!("{name}.toml")).display()
        ))
    }

    fn validate(&self, raw: &str) -> Result<String, String> {
        let path = draft_path(&self.working_dir, raw)?;
        self.data.set(
            DRAFT_KEY,
            json!({ "path": path.display().to_string(), "valid": false }),
        );
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut errors: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let spec = match load_tool_file(&path) {
            Ok(spec) => Some(spec),
            Err(found) => {
                errors.extend(found);
                None
            }
        };
        if let (Some(spec), Some(planned)) = (&spec, planned_name(&self.data)) {
            if spec.name != planned {
                errors.push(format!(
                    "the draft defines '{}', but the plan is '{planned}'",
                    spec.name
                ));
            }
        }
        if let Some(spec) = &spec {
            if let Some(planned_cap) = self.data.get(PLAN_KEY).and_then(|p| {
                p.get("capability")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }) {
                if planned_cap != spec.capability.as_str() {
                    notes.push(format!(
                        "note: capability is '{}' but the plan said '{planned_cap}'",
                        spec.capability
                    ));
                }
            }
            if let Some(program) = xaft_tools::command_template::shell_split(&spec.command)
                .ok()
                .and_then(|w| w.into_iter().next())
            {
                if !on_path(&program, &self.working_dir) {
                    errors.push(format!(
                        "the program '{program}' is not on PATH (or not a file in the project)"
                    ));
                }
            }
        }
        for dependency in planned_dependencies(&self.data) {
            if !on_path(&dependency, &self.working_dir) {
                errors.push(format!("dependency '{dependency}' is not on PATH"));
            }
        }
        let valid = errors.is_empty();
        self.data.set(
            DRAFT_KEY,
            json!({
                "path": path.display().to_string(),
                "valid": valid,
                "sha256": sha256_hex(&bytes),
            }),
        );
        let mut lines = Vec::new();
        if let Some(spec) = &spec {
            lines.push(format!(
                "Custom tool '{}' — capability {}, timeout {}s, params: {}",
                spec.name,
                spec.capability,
                spec.timeout_s,
                if spec.params.is_empty() {
                    "(none)".to_string()
                } else {
                    spec.params
                        .iter()
                        .map(|p| {
                            if p.required() {
                                p.name.clone()
                            } else {
                                format!("{}={:?}", p.name, p.default.clone().unwrap_or_default())
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        lines.extend(errors.iter().map(|e| format!("error: {e}")));
        lines.extend(notes);
        lines.push(if valid {
            "RESULT: valid — run smoke_test_custom_tool(path, args) next.".into()
        } else {
            "RESULT: invalid — fix every error and validate again.".into()
        });
        Ok(lines.join("\n"))
    }

    async fn smoke(&self, raw: &str, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = draft_path(&self.working_dir, raw)?;
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let sha = sha256_hex(&bytes);
        let validated = self.data.get(DRAFT_KEY).is_some_and(|d| {
            d.get("valid") == Some(&json!(true))
                && d.get("sha256").and_then(Value::as_str) == Some(sha.as_str())
                && d.get("path").and_then(Value::as_str) == Some(&path.display().to_string())
        });
        if !validated {
            return Err(
                "the draft has not passed validate_custom_tool_draft(path) in its current form; \
                 validate it first"
                    .into(),
            );
        }
        let spec: CustomToolSpec = load_tool_file(&path).map_err(|e| e.join("; "))?;
        let input = match args {
            Value::Object(_) => args.clone(),
            Value::String(s) if s.trim().is_empty() => json!({}),
            Value::String(s) => serde_json::from_str::<Value>(s)
                .ok()
                .filter(Value::is_object)
                .ok_or("args must be a JSON object of parameter values")?,
            Value::Null => json!({}),
            _ => return Err("args must be a JSON object of parameter values".into()),
        };
        let tool = CustomTool::new(spec, self.working_dir.clone());
        let run = tool.invoke(&input, Some(&ctx.cancel_token)).await?;
        let mut record = self.data.get(DRAFT_KEY).unwrap_or_else(|| json!({}));
        record["smoke_sha256"] = json!(sha);
        record["smoke_ok"] = json!(run.success);
        self.data.set(DRAFT_KEY, record);
        let code = run
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".into());
        let report = format!(
            "argv: {:?}\nexit code: {code}\nstdout:\n{}\nstderr:\n{}",
            run.argv,
            snippet(&run.stdout, 4000),
            snippet(&run.stderr, 2000)
        );
        if run.success {
            Ok(format!("SMOKE TEST: passed\n{report}"))
        } else {
            Err(format!("SMOKE TEST: failed\n{report}"))
        }
    }
}

fn snippet(text: &str, max: usize) -> String {
    let trimmed = text.trim_end();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}…")
}

#[async_trait::async_trait]
impl Tool for AuthoringTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        match self.kind {
            Kind::Describe => "describe_custom_tool_schema",
            Kind::List => "list_custom_tools",
            Kind::DraftPath => "custom_tool_draft_path",
            Kind::Validate => "validate_custom_tool_draft",
            Kind::Smoke => "smoke_test_custom_tool",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Describe => "Describe every field of an xaft custom tool definition.",
            Kind::List => {
                "List the custom tools of this project and user, with any that are not loaded \
                 and why."
            }
            Kind::DraftPath => {
                "Return the draft path to write a new custom tool to (under \
                 .xaft/tools/_drafts/, which is never loaded until published)."
            }
            Kind::Validate => {
                "Validate a custom tool draft deterministically: the loader's checks, the \
                 recorded plan, and that its program and dependencies are on PATH."
            }
            Kind::Smoke => {
                "Run a validated custom tool draft once with sample arguments and report its \
                 exit code and output. The user approves the run first."
            }
        }
    }

    fn schema(&self) -> Value {
        let props = match self.kind {
            Kind::Describe | Kind::List => json!({}),
            Kind::DraftPath => json!({
                "name": { "type": "string", "description": "The tool's lower_snake_case name." }
            }),
            Kind::Validate => json!({
                "path": { "type": "string", "description": "Path of the draft file." }
            }),
            Kind::Smoke => json!({
                "path": { "type": "string", "description": "Path of the validated draft file." },
                "args": {
                    "type": "object",
                    "description": "Sample parameter values, e.g. {\"path\": \"src\"}."
                }
            }),
        };
        let required: Vec<&str> = match self.kind {
            Kind::Describe | Kind::List => vec![],
            Kind::DraftPath => vec!["name"],
            Kind::Validate => vec!["path"],
            Kind::Smoke => vec!["path", "args"],
        };
        json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        // Running a draft executes a program with the user's permissions.
        self.kind == Kind::Smoke
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let outcome = match self.kind {
            Kind::Describe => Ok(SCHEMA_DOC.to_string()),
            Kind::List => Ok(self.list()),
            Kind::DraftPath => self.draft_path_for(&arg(&input, "name")),
            Kind::Validate => self.validate(&arg(&input, "path")),
            Kind::Smoke => {
                let args = input.get("args").cloned().unwrap_or(Value::Null);
                self.smoke(&arg(&input, "path"), &args, ctx).await
            }
        };
        Ok(match outcome {
            Ok(text) => ToolResult::ok(text, &ctx.tool_use_id),
            Err(text) => ToolResult::error(text, &ctx.tool_use_id),
        })
    }
}

// ── Guard and effects ──────────────────────────────────────────────────────

/// `tool_draft_valid`: the recorded draft's current bytes validated and
/// passed a smoke test.
pub(crate) fn draft_valid(data: &RunData) -> Result<(), String> {
    let record = data
        .get(DRAFT_KEY)
        .ok_or("no draft has been validated; run validate_custom_tool_draft(path) first")?;
    let path = record
        .get("path")
        .and_then(Value::as_str)
        .ok_or("no draft has been validated; run validate_custom_tool_draft(path) first")?;
    let bytes = std::fs::read(path).map_err(|e| format!("the draft {path} is unreadable: {e}"))?;
    let sha = sha256_hex(&bytes);
    if record.get("valid") != Some(&json!(true))
        || record.get("sha256").and_then(Value::as_str) != Some(&sha)
    {
        return Err(
            "the draft has not passed validate_custom_tool_draft(path) in its current \
                    form; validate it again"
                .into(),
        );
    }
    if record.get("smoke_ok") != Some(&json!(true))
        || record.get("smoke_sha256").and_then(Value::as_str) != Some(&sha)
    {
        return Err(
            "the draft has not passed smoke_test_custom_tool(path, args) in its current \
                    form; run the smoke test"
                .into(),
        );
    }
    Ok(())
}

/// Run one of make_xaft_tool's effects.
pub(crate) fn run_effect(effect: &str, data: &RunData, args: &Value) -> Result<String, String> {
    match effect {
        "record_tool_plan" => record_tool_plan(data, args),
        "record_tool_draft" => record_tool_draft(data, args),
        "publish_custom_tool" => publish_custom_tool(data),
        other => Err(format!("unknown effect '{other}'")),
    }
}

/// agenthicc's `submit_tool_plan` checks, for an xaft custom tool.
fn record_tool_plan(data: &RunData, args: &Value) -> Result<String, String> {
    let name = arg(args, "tool_name");
    if name.is_empty() {
        return Err(
            "tool_name must not be empty. Call submit_tool_plan() with a descriptive \
                    tool name."
                .into(),
        );
    }
    if !valid_tool_name(&name) {
        return Err(format!(
            "tool_name '{name}' is not a valid tool name. Use lowercase snake_case, e.g. \
             'project_status'."
        ));
    }
    if is_builtin_tool_name(&name) {
        return Err(format!(
            "tool_name '{name}' is a built-in tool. Choose a name no built-in tool uses."
        ));
    }
    let description = arg(args, "description");
    if description.is_empty() {
        return Err(
            "description must not be empty. Provide a one-line description for the \
                    tool schema."
                .into(),
        );
    }
    let capability = arg(args, "capability");
    if !CUSTOM_TOOL_CAPABILITIES.contains(&capability.as_str()) {
        return Err(format!(
            "Unsupported capability '{capability}'. Use only: {}.",
            CUSTOM_TOOL_CAPABILITIES.join(", ")
        ));
    }
    let mut parameters = Vec::new();
    if let Some(items) = args.get("parameters").and_then(Value::as_array) {
        for raw in items {
            let Some(obj) = raw.as_object() else { continue };
            let pname = obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !xaft_tools::command_template::valid_placeholder_name(&pname) {
                continue;
            }
            let mut param = json!({
                "name": pname,
                "description": obj.get("description").and_then(Value::as_str).unwrap_or_default().trim(),
            });
            if let Some(default) = obj.get("default") {
                let default = match default {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                if !default.is_empty() {
                    param["default"] = json!(default);
                }
            }
            parameters.push(param);
        }
    }
    let dependencies: Vec<String> = args
        .get("dependencies")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let count = parameters.len();
    data.set(
        PLAN_KEY,
        json!({
            "tool_name": name,
            "description": description,
            "parameters": parameters,
            "capability": capability,
            "dependencies": dependencies,
        }),
    );
    data.set(DRAFT_KEY, Value::Null);
    Ok(format!(
        "Tool plan recorded: '{name}' with {count} params, capability {capability}. The \
         generate phase starts next."
    ))
}

/// agenthicc's `confirm_generation_complete`: the draft must exist at the
/// planned draft path.
fn record_tool_draft(data: &RunData, args: &Value) -> Result<String, String> {
    let wd = data
        .working_dir()
        .ok_or("the run has no working directory")?
        .to_path_buf();
    let name = planned_name(data).ok_or("no tool plan is recorded")?;
    let raw = arg(args, "file_path");
    let path = draft_path(&wd, &raw)
        .map_err(|e| format!("{e}. Write the draft to .xaft/tools/_drafts/{name}.toml."))?;
    if path.file_name().and_then(|n| n.to_str()) != Some(&format!("{name}.toml")) {
        return Err(format!(
            "file_path must be the planned draft .xaft/tools/_drafts/{name}.toml, not {}",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!(
            "no draft exists at {}. Write the custom tool there first.",
            path.display()
        ));
    }
    data.set("tool_draft_path", json!(path.display().to_string()));
    Ok(format!(
        "Generation confirmed at '{}'. The validate phase starts next.",
        path.display()
    ))
}

/// Re-validate the draft and publish it atomically into `.xaft/tools/`.
///
/// What is published is the bytes read here, checked against the digests the
/// validation and the smoke test recorded, so an edit between the guard and
/// the copy cannot slip through.
fn publish_custom_tool(data: &RunData) -> Result<String, String> {
    draft_valid(data)?;
    let wd = data
        .working_dir()
        .ok_or("the run has no working directory")?
        .to_path_buf();
    let record = data
        .get(DRAFT_KEY)
        .ok_or("no validated draft is recorded")?;
    let draft = record
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or("no validated draft is recorded")?;
    let bytes = std::fs::read(&draft)
        .map_err(|e| format!("the draft {} is unreadable: {e}", draft.display()))?;
    let sha = sha256_hex(&bytes);
    if record.get("sha256").and_then(Value::as_str) != Some(sha.as_str())
        || record.get("smoke_sha256").and_then(Value::as_str) != Some(sha.as_str())
    {
        return Err(
            "the draft changed after it was validated and smoke-tested; validate and \
             smoke-test it again"
                .into(),
        );
    }
    let text = String::from_utf8(bytes).map_err(|_| "the draft is not UTF-8 text".to_string())?;
    let stem = draft.file_stem().and_then(|s| s.to_str());
    let spec = parse_tool_spec(&text, stem)
        .map_err(|e| format!("the draft no longer validates: {}", e.join("; ")))?;
    if let Some(planned) = planned_name(data) {
        if spec.name != planned {
            return Err(format!(
                "the draft defines '{}', not the planned '{planned}'",
                spec.name
            ));
        }
    }
    let dest_dir = project_tools_dir(&wd);
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(format!("{}.toml", spec.name));
    let staging = dest_dir.join(format!(
        "_staging-{}-{}.toml",
        spec.name,
        std::process::id()
    ));
    std::fs::write(&staging, text.as_bytes())
        .map_err(|e| format!("cannot stage the draft: {e}"))?;
    if dest.exists() {
        let backups = dest_dir.join("_backups");
        std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        std::fs::rename(&dest, backups.join(format!("{stamp}-{}.toml", spec.name)))
            .map_err(|e| format!("cannot back up {}: {e}", dest.display()))?;
    }
    std::fs::rename(&staging, &dest).map_err(|e| format!("cannot publish: {e}"))?;
    let shown = dest
        .strip_prefix(&wd)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| dest.display().to_string());
    data.set_state_values([
        ("published_tool_path", shown.clone()),
        ("published_tool_name", spec.name.clone()),
    ]);
    Ok(format!(
        "Published custom tool '{}' to {shown}. It loads in new runs once trusted: \
         `xaft trust add {shown}`.",
        spec.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new("t")
    }

    fn setup() -> (tempfile::TempDir, RunData, Vec<Arc<ErasedTool>>) {
        let tmp = tempfile::tempdir().unwrap();
        let data = RunData::new(Default::default(), tmp.path());
        let tools = authoring_tools(
            &ToolAuthoringContext {
                working_dir: tmp.path().to_path_buf(),
            },
            &data,
        );
        (tmp, data, tools)
    }

    fn tool<'a>(tools: &'a [Arc<ErasedTool>], name: &str) -> &'a Arc<ErasedTool> {
        tools.iter().find(|t| t.name() == name).unwrap()
    }

    const DRAFT: &str = r#"
name = "xaft_test_greeter"
description = "Greet someone."
capability = "read"
command = "echo hello {who}"

[[params]]
name = "who"
description = "Who."
"#;

    fn plan(data: &RunData) {
        run_effect(
            "record_tool_plan",
            data,
            &json!({
                "tool_name": "xaft_test_greeter",
                "description": "Greet someone.",
                "parameters": [{"name": "who", "description": "Who."}, {"name": "bad name"}, 7],
                "capability": "read",
                "dependencies": ["echo"],
            }),
        )
        .unwrap();
    }

    #[test]
    fn the_plan_is_validated_like_agenthiccs_submit_tool_plan() {
        let (_tmp, data, _) = setup();
        for (args, needle) in [
            (
                json!({"tool_name": "", "description": "d", "capability": "read"}),
                "must not be empty",
            ),
            (
                json!({"tool_name": "Bad-Name", "description": "d", "capability": "read"}),
                "not a valid tool name",
            ),
            (
                json!({"tool_name": "read_file", "description": "d", "capability": "read"}),
                "built-in",
            ),
            (
                json!({"tool_name": "ok_tool", "description": " ", "capability": "read"}),
                "description",
            ),
            (
                json!({"tool_name": "ok_tool", "description": "d", "capability": "teleport"}),
                "Unsupported capability",
            ),
        ] {
            let err = run_effect("record_tool_plan", &data, &args).unwrap_err();
            assert!(err.contains(needle), "{args}: {err}");
        }
        plan(&data);
        let recorded = data.get(PLAN_KEY).unwrap();
        assert_eq!(
            recorded["parameters"].as_array().unwrap().len(),
            1,
            "invalid params dropped"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn validate_smoke_and_publish_round_trip() {
        let (tmp, data, tools) = setup();
        plan(&data);
        let path_msg = tool(&tools, "custom_tool_draft_path")
            .call(json!({"name": "xaft_test_greeter"}), &ctx())
            .await
            .unwrap();
        assert!(!path_msg.is_error, "{}", path_msg.content);
        let draft = draft_dir(tmp.path()).join("xaft_test_greeter.toml");
        std::fs::write(&draft, DRAFT).unwrap();

        let err = run_effect(
            "record_tool_draft",
            &data,
            &json!({"file_path": ".xaft/tools/other.toml", "summary": "s"}),
        )
        .unwrap_err();
        assert!(err.contains("drafts must be files directly under"), "{err}");
        run_effect(
            "record_tool_draft",
            &data,
            &json!({"file_path": ".xaft/tools/_drafts/xaft_test_greeter.toml", "summary": "s"}),
        )
        .unwrap();

        assert!(draft_valid(&data).is_err(), "nothing validated yet");
        let smoke_first = tool(&tools, "smoke_test_custom_tool")
            .call(
                json!({"path": draft.display().to_string(), "args": {"who": "x"}}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(smoke_first.is_error, "validation comes first");

        let report = tool(&tools, "validate_custom_tool_draft")
            .call(json!({"path": draft.display().to_string()}), &ctx())
            .await
            .unwrap();
        assert!(
            report.content.contains("RESULT: valid"),
            "{}",
            report.content
        );
        assert!(draft_valid(&data).unwrap_err().contains("smoke test"));

        let smoke = tool(&tools, "smoke_test_custom_tool")
            .call(
                json!({"path": draft.display().to_string(), "args": {"who": "world"}}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!smoke.is_error, "{}", smoke.content);
        assert!(smoke.content.contains("hello world"), "{}", smoke.content);
        assert!(tool(&tools, "smoke_test_custom_tool").requires_confirmation());
        draft_valid(&data).unwrap();

        // Editing the draft after its checks invalidates them.
        std::fs::write(&draft, DRAFT.replace("Greet someone.", "Greet.")).unwrap();
        assert!(draft_valid(&data).is_err());
        std::fs::write(&draft, DRAFT).unwrap();
        draft_valid(&data).unwrap();

        let note = run_effect("publish_custom_tool", &data, &json!({})).unwrap();
        assert!(
            note.contains("xaft trust add .xaft/tools/xaft_test_greeter.toml"),
            "{note}"
        );
        let published = project_tools_dir(tmp.path()).join("xaft_test_greeter.toml");
        assert!(published.is_file());
        assert_eq!(
            data.state_value("published_tool_path"),
            ".xaft/tools/xaft_test_greeter.toml"
        );
        // The published file is exactly what the loader accepts.
        assert_eq!(
            load_tool_file(&published).unwrap().name,
            "xaft_test_greeter"
        );

        // Publishing again keeps a backup of the previous version.
        run_effect("publish_custom_tool", &data, &json!({})).unwrap();
        assert_eq!(
            std::fs::read_dir(project_tools_dir(tmp.path()).join("_backups"))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn validation_reports_plan_mismatches_and_missing_programs() {
        let (tmp, data, tools) = setup();
        plan(&data);
        std::fs::create_dir_all(draft_dir(tmp.path())).unwrap();
        let draft = draft_dir(tmp.path()).join("xaft_test_greeter.toml");
        std::fs::write(
            &draft,
            DRAFT
                .replace("name = \"xaft_test_greeter\"\n", "")
                .replace("echo hello", "xaft-no-such-program-xyz hello"),
        )
        .unwrap();
        let report = tool(&tools, "validate_custom_tool_draft")
            .call(json!({"path": draft.display().to_string()}), &ctx())
            .await
            .unwrap();
        assert!(
            report.content.contains("RESULT: invalid"),
            "{}",
            report.content
        );
        assert!(
            report.content.contains("is not on PATH"),
            "{}",
            report.content
        );
        let outside = tool(&tools, "validate_custom_tool_draft")
            .call(json!({"path": "/etc/passwd"}), &ctx())
            .await
            .unwrap();
        assert!(outside.is_error);
    }
}
