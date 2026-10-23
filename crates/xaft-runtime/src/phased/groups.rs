//! Workflow state shared across phases, and the named tool groups a phase can
//! add with `extra_tools = [...]`.
//!
//! agenthicc's custom runners keep typed state (goal lists, artifacts, draft
//! manifests) in their context dataclasses and expose it through
//! workflow-specific tools. A declarative workflow gets the same through
//! [`RunData`] — a key/value map persisted in every checkpoint — and these
//! groups:
//!
//! | group | tools |
//! |---|---|
//! | `goal_list` | `set_goals`, `append_goal`, `insert_goal`, `list_goals`, `complete_goal`, `skip_goal` |
//! | `workflow_state` | `set_state`, `get_state`, `list_state` |
//! | `artifacts` | `record_artifact`, `list_artifacts` |
//! | `ask_user` | `ask_user` |
//! | `workflow_authoring` | the `create_workflow` authoring surface (see [`AuthoringContext`]) |
//! | `ui_catalog` | `lookup_component`, `list_components` (see [`super::ui_catalog`]) |
//! | `ui_ux_pro_max` | `request_implementation_approval` (see [`super::ui_ux_pro_max`]) |
//!
//! Transition guards ([`check_guard`]) and effects ([`run_effect`]) read and
//! write the same map, so a phase's tools and its transitions agree on state.
//! Guards and effects that also need the call's arguments, or choose the edge,
//! are dispatched by [`super::hooks`].

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};

use xaft_config::XaftConfig;
use xaft_config::workflows::discovery::{
    PACKAGE_MANIFEST, is_hidden_entry, load_workflow_path, project_workflow_dir,
};
use xaft_config::workflows::validate::{
    KNOWN_AGENT_TYPES, KNOWN_TOOL_GROUPS, is_snake_identifier, validate_definition,
};
use xaft_config::workflows::{WorkflowRegistry, builtin};
use xaft_tools::capabilities::{READ_CAPS, ToolCapability, capability_of};

use super::question::UserQuestionGate;
use super::tools::SharedControl;

// ── RunData ────────────────────────────────────────────────────────────────

/// The run's key/value state, shared by every tool of every phase.
#[derive(Debug, Clone, Default)]
pub struct RunData {
    map: Arc<Mutex<BTreeMap<String, Value>>>,
    working_dir: Option<Arc<PathBuf>>,
    meta: Option<Arc<RunMeta>>,
}

/// Read-only facts about the run that hooks and tool groups may consult.
///
/// Not checkpointed: the engine rebuilds it from the current definition and
/// tool pool every time a run starts or resumes.
#[derive(Debug, Clone, Default)]
pub struct RunMeta {
    /// The durable run id (also the checkpoint file stem).
    pub run_id: String,
    /// The workflow's registry name.
    pub workflow: String,
    /// The run's intent.
    pub intent: String,
    /// The definition's free-form params (agenthicc's `WorkflowParams`), as
    /// JSON.
    pub params: serde_json::Map<String, Value>,
    /// Names of every tool the session can grant a phase.
    pub tool_names: Vec<String>,
}

impl RunData {
    /// Wrap a checkpointed map for a run in `working_dir`.
    pub fn new(map: BTreeMap<String, Value>, working_dir: &Path) -> Self {
        Self {
            map: Arc::new(Mutex::new(map)),
            working_dir: Some(Arc::new(working_dir.to_path_buf())),
            meta: None,
        }
    }

    /// Attach the run's metadata.
    pub fn with_meta(mut self, meta: RunMeta) -> Self {
        self.meta = Some(Arc::new(meta));
        self
    }

    /// The run id, when the engine attached run metadata.
    pub fn run_id(&self) -> Option<&str> {
        self.meta.as_deref().map(|m| m.run_id.as_str())
    }

    /// The run's intent, when the engine attached run metadata.
    pub fn intent(&self) -> &str {
        self.meta.as_deref().map_or("", |m| m.intent.as_str())
    }

    /// One workflow param.
    pub fn param(&self, key: &str) -> Option<&Value> {
        self.meta.as_deref().and_then(|m| m.params.get(key))
    }

    /// Whether the session offers a tool with this name.
    pub fn has_tool(&self, name: &str) -> bool {
        self.meta
            .as_deref()
            .is_some_and(|m| m.tool_names.iter().any(|t| t == name))
    }

    /// Read one key.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.map.lock().ok()?.get(key).cloned()
    }

    /// Write one key.
    pub fn set(&self, key: &str, value: Value) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(key.to_string(), value);
        }
    }

    /// A copy of the whole map, for checkpointing.
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        self.map.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// The run's working directory, if known.
    pub fn working_dir(&self) -> Option<&Path> {
        self.working_dir.as_deref().map(PathBuf::as_path)
    }

    fn goals(&self) -> Vec<Goal> {
        self.get("goals")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    fn set_goals(&self, goals: &[Goal]) {
        self.set("goals", serde_json::to_value(goals).unwrap_or(Value::Null));
    }

    fn truthy(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }
}

/// One entry of a `goal_list`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    /// Stable id, unique within the run.
    pub id: u32,
    /// What must be achieved.
    pub title: String,
    /// `pending`, `done` or `skipped`.
    pub status: String,
    /// Evidence (done) or reason (skipped).
    #[serde(default)]
    pub note: String,
}

/// Render a goal list for prompts and tool results.
pub fn render_goals(goals: &[Goal]) -> String {
    if goals.is_empty() {
        return "(no goals recorded)".to_string();
    }
    goals
        .iter()
        .map(|g| {
            let mark = match g.status.as_str() {
                "done" => "[x]",
                "skipped" => "[-]",
                _ => "[ ]",
            };
            if g.note.is_empty() {
                format!("{mark} #{} {}", g.id, g.title)
            } else {
                format!("{mark} #{} {} — {}", g.id, g.title, g.note)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The goal list stored in `data`, rendered.
pub fn goals_block(data: &RunData) -> String {
    render_goals(&data.goals())
}

/// The first pending goal, rendered, or empty.
pub fn next_goal(data: &RunData) -> String {
    data.goals()
        .into_iter()
        .find(|g| g.status == "pending")
        .map(|g| format!("#{} {}", g.id, g.title))
        .unwrap_or_default()
}

// ── Guards and effects ─────────────────────────────────────────────────────

/// Check a transition guard. `Err` explains what is still missing.
pub fn check_guard(guard: &str, data: &RunData, control: &SharedControl) -> Result<(), String> {
    match guard {
        "goals_complete" => {
            let goals = data.goals();
            if goals.is_empty() {
                return Err("no goals are recorded; call set_goals() first".into());
            }
            let open: Vec<String> = goals
                .iter()
                .filter(|g| g.status == "pending")
                .map(|g| format!("#{} {}", g.id, g.title))
                .collect();
            if open.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "{} goal(s) are still pending: {}",
                    open.len(),
                    open.join("; ")
                ))
            }
        }
        // The branch partner of goals_complete: "verified, and there is more
        // work" — so a looping phase can choose its edge by which guard holds.
        "goals_pending" => {
            if data.goals().iter().any(|g| g.status == "pending") {
                Ok(())
            } else {
                Err("no goal is pending; the goal list is complete".into())
            }
        }
        "has_artifacts" => match data.get("artifacts") {
            Some(Value::Array(a)) if !a.is_empty() => Ok(()),
            _ => Err("no artifacts are recorded; call record_artifact() first".into()),
        },
        "plan_approved" => {
            let in_phase = control.lock().map(|c| c.plan_approved).unwrap_or(false);
            if in_phase || data.truthy("plan_approved") {
                Ok(())
            } else {
                Err("the plan has not been approved; call request_plan_approval() first".into())
            }
        }
        "draft_valid" => {
            if data.truthy("draft_valid") {
                Ok(())
            } else {
                Err("the workflow draft has not passed validation; run \
                     validate_workflow_draft(path) and fix every error first"
                    .into())
            }
        }
        other => Err(format!("unknown guard '{other}'")),
    }
}

/// Perform a transition effect. `Ok` carries a note for the agent.
pub fn run_effect(effect: &str, data: &RunData, args: &Value) -> Result<String, String> {
    match effect {
        "publish_workflow_draft" => publish_workflow_draft(data, args),
        other => Err(format!("unknown effect '{other}'")),
    }
}

/// Re-validate the recorded draft and publish it atomically.
fn publish_workflow_draft(data: &RunData, args: &Value) -> Result<String, String> {
    let wd = data
        .working_dir()
        .ok_or("the run has no working directory")?
        .to_path_buf();
    let draft = data
        .get("draft_path")
        .and_then(|v| v.as_str().map(PathBuf::from))
        .ok_or("no validated draft is recorded; run validate_workflow_draft(path) first")?;
    let name = data
        .get("draft_name")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or("the validated draft has no name")?;
    if let Some(requested) = args.get("name").and_then(Value::as_str) {
        if !requested.trim().is_empty() && requested.trim() != name {
            return Err(format!(
                "the validated draft is '{name}', not '{requested}'; validate the draft you \
                 mean to publish"
            ));
        }
    }

    // Validation is repeated at the publication boundary: the draft may have
    // been edited after validate_workflow_draft() ran.
    let loaded = load_workflow_path(&draft).map_err(|e| e.to_string())?;
    if loaded.name != name {
        return Err(format!(
            "the draft now declares name '{}', not '{name}'; re-run validate_workflow_draft()",
            loaded.name
        ));
    }
    let report = validate_definition(&name, &loaded.definition);
    if !report.is_ok() {
        data.set("draft_valid", json!(false));
        return Err(format!(
            "the draft no longer validates:\n{}",
            report.render()
        ));
    }

    let dest_dir = project_workflow_dir(&wd);
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let is_package = draft.is_dir();
    let dest = if is_package {
        dest_dir.join(&name)
    } else {
        dest_dir.join(format!("{name}.toml"))
    };
    let staging = dest_dir.join(format!(
        "_staging-{name}-{}{}",
        std::process::id(),
        if is_package { "" } else { ".toml" }
    ));
    if staging.exists() {
        remove_path(&staging).map_err(|e| e.to_string())?;
    }
    copy_path(&draft, &staging).map_err(|e| format!("cannot stage the draft: {e}"))?;

    // Any previous version — including the other shape (file vs package) —
    // is moved aside, so the published name resolves to exactly one entry.
    let backups = dest_dir.join("_backups");
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    for existing in [dest_dir.join(&name), dest_dir.join(format!("{name}.toml"))] {
        if existing.exists() {
            std::fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
            let file_name = existing
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workflow");
            std::fs::rename(&existing, backups.join(format!("{stamp}-{file_name}")))
                .map_err(|e| format!("cannot back up {}: {e}", existing.display()))?;
        }
    }
    std::fs::rename(&staging, &dest).map_err(|e| format!("cannot publish: {e}"))?;

    data.set("published_path", json!(dest.display().to_string()));
    data.set("published_name", json!(name));
    Ok(format!(
        "Published workflow '{name}' to {}. Run it with `xaft workflows run {name} --intent \"...\"` \
         or `/workflow {name}`.",
        dest.display()
    ))
}

fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

// ── A small helper for data-backed tools ───────────────────────────────────

type Handler = dyn Fn(&Value) -> Result<String, String> + Send + Sync;

/// A tool defined by a name, a description, string parameters and a closure.
struct FnTool {
    name: &'static str,
    description: &'static str,
    params: &'static [(&'static str, &'static str, bool)],
    handler: Box<Handler>,
}

impl FnTool {
    fn erased(
        name: &'static str,
        description: &'static str,
        params: &'static [(&'static str, &'static str, bool)],
        handler: impl Fn(&Value) -> Result<String, String> + Send + Sync + 'static,
    ) -> Arc<ErasedTool> {
        Arc::new(Self {
            name,
            description,
            params,
            handler: Box::new(handler),
        }) as Arc<ErasedTool>
    }
}

#[async_trait::async_trait]
impl Tool for FnTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, desc, req) in self.params {
            properties.insert(
                (*name).to_string(),
                json!({ "type": "string", "description": desc }),
            );
            if *req {
                required.push(json!(name));
            }
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        for (name, _, req) in self.params {
            let blank = input
                .get(*name)
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty());
            if *req && blank {
                return Ok(ToolResult::error(
                    format!("{}() requires a non-empty '{name}' argument.", self.name),
                    &ctx.tool_use_id,
                ));
            }
        }
        Ok(match (self.handler)(&input) {
            Ok(text) => ToolResult::ok(text, &ctx.tool_use_id),
            Err(err) => ToolResult::error(err, &ctx.tool_use_id),
        })
    }
}

fn arg(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

// ── goal_list ──────────────────────────────────────────────────────────────

/// The `goal_list` group.
pub fn goal_list_tools(data: &RunData) -> Vec<Arc<ErasedTool>> {
    let d = data.clone();
    let set_goals = FnTool::erased(
        "set_goals",
        "Record the ordered goal list for this workflow, one goal per line. Replaces the \
         list only while no goal has been completed.",
        &[("goals", "One goal per line, in execution order.", true)],
        move |input| {
            let existing = d.goals();
            if existing.iter().any(|g| g.status != "pending") {
                return Err(
                    "goals are already in progress; use append_goal() or insert_goal()".into(),
                );
            }
            let goals: Vec<Goal> = arg(input, "goals")
                .lines()
                .map(|l| l.trim().trim_start_matches(['-', '*']).trim())
                .filter(|l| !l.is_empty())
                .enumerate()
                .map(|(i, title)| Goal {
                    id: i as u32 + 1,
                    title: title.to_string(),
                    status: "pending".into(),
                    note: String::new(),
                })
                .collect();
            if goals.is_empty() {
                return Err("no goals given".into());
            }
            d.set_goals(&goals);
            Ok(format!(
                "Recorded {} goal(s):\n{}",
                goals.len(),
                render_goals(&goals)
            ))
        },
    );

    let d = data.clone();
    let append_goal = FnTool::erased(
        "append_goal",
        "Add newly discovered concrete work to the end of the goal list. Does not change \
         phase.",
        &[("goal", "The new goal.", true)],
        move |input| {
            let mut goals = d.goals();
            let id = goals.iter().map(|g| g.id).max().unwrap_or(0) + 1;
            goals.push(Goal {
                id,
                title: arg(input, "goal"),
                status: "pending".into(),
                note: String::new(),
            });
            d.set_goals(&goals);
            Ok(format!("Appended goal #{id}.\n{}", render_goals(&goals)))
        },
    );

    let d = data.clone();
    let insert_goal = FnTool::erased(
        "insert_goal",
        "Insert newly discovered work before an existing pending goal. Does not change phase.",
        &[
            (
                "before_id",
                "Id of the goal the new goal must precede.",
                true,
            ),
            ("goal", "The new goal.", true),
        ],
        move |input| {
            let mut goals = d.goals();
            let before: u32 = arg(input, "before_id")
                .trim_start_matches('#')
                .parse()
                .map_err(|_| "before_id must be a goal id number".to_string())?;
            let pos = goals
                .iter()
                .position(|g| g.id == before)
                .ok_or_else(|| format!("no goal #{before}"))?;
            let id = goals.iter().map(|g| g.id).max().unwrap_or(0) + 1;
            goals.insert(
                pos,
                Goal {
                    id,
                    title: arg(input, "goal"),
                    status: "pending".into(),
                    note: String::new(),
                },
            );
            d.set_goals(&goals);
            Ok(format!(
                "Inserted goal #{id} before #{before}.\n{}",
                render_goals(&goals)
            ))
        },
    );

    let d = data.clone();
    let list_goals = FnTool::erased(
        "list_goals",
        "Show the goal list with each goal's status.",
        &[],
        move |_| Ok(render_goals(&d.goals())),
    );

    let mark = |name: &'static str,
                description: &'static str,
                note_param: &'static str,
                status: &'static str,
                data: RunData| {
        let params: &'static [(&'static str, &'static str, bool)] = if note_param == "evidence" {
            &[
                ("id", "The goal id.", true),
                ("evidence", "Concrete evidence the goal is achieved.", true),
            ]
        } else {
            &[
                ("id", "The goal id.", true),
                ("reason", "Why the goal no longer applies.", true),
            ]
        };
        FnTool::erased(name, description, params, move |input| {
            let mut goals = data.goals();
            let id: u32 = arg(input, "id")
                .trim_start_matches('#')
                .parse()
                .map_err(|_| "id must be a goal id number".to_string())?;
            let goal = goals
                .iter_mut()
                .find(|g| g.id == id)
                .ok_or_else(|| format!("no goal #{id}"))?;
            goal.status = status.to_string();
            goal.note = arg(input, note_param);
            data.set_goals(&goals);
            let remaining = goals.iter().filter(|g| g.status == "pending").count();
            Ok(format!(
                "Goal #{id} marked {status}. {remaining} goal(s) remain.\n{}",
                render_goals(&goals)
            ))
        })
    };

    vec![
        set_goals,
        append_goal,
        insert_goal,
        list_goals,
        mark(
            "complete_goal",
            "Mark a goal done, with evidence. Does not change phase.",
            "evidence",
            "done",
            data.clone(),
        ),
        mark(
            "skip_goal",
            "Mark a goal skipped because it no longer applies, with the reason.",
            "reason",
            "skipped",
            data.clone(),
        ),
    ]
}

// ── workflow_state ─────────────────────────────────────────────────────────

/// The `workflow_state` group.
pub fn workflow_state_tools(data: &RunData) -> Vec<Arc<ErasedTool>> {
    fn state_map(d: &RunData) -> serde_json::Map<String, Value> {
        match d.get("state") {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        }
    }
    let d = data.clone();
    let set_state = FnTool::erased(
        "set_state",
        "Store a named value that later phases can read (and prompts can reference as \
         {state.<key>}). Survives resume.",
        &[
            ("key", "lower_snake_case key.", true),
            ("value", "The value to store.", true),
        ],
        move |input| {
            let key = arg(input, "key");
            if !is_snake_identifier(&key) {
                return Err("key must be lower_snake_case".into());
            }
            let mut m = state_map(&d);
            m.insert(key.clone(), json!(arg(input, "value")));
            d.set("state", Value::Object(m));
            Ok(format!("Stored state '{key}'."))
        },
    );
    let d = data.clone();
    let get_state = FnTool::erased(
        "get_state",
        "Read a value stored with set_state().",
        &[("key", "The key.", true)],
        move |input| {
            let key = arg(input, "key");
            state_map(&d)
                .get(&key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("no state '{key}'"))
        },
    );
    let d = data.clone();
    let list_state = FnTool::erased(
        "list_state",
        "List every stored state key with its value.",
        &[],
        move |_| {
            let m = state_map(&d);
            if m.is_empty() {
                return Ok("(no state stored)".into());
            }
            Ok(m.iter()
                .map(|(k, v)| format!("{k} = {}", v.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join("\n"))
        },
    );
    vec![set_state, get_state, list_state]
}

// ── artifacts ──────────────────────────────────────────────────────────────

/// The `artifacts` group.
pub fn artifact_tools(data: &RunData) -> Vec<Arc<ErasedTool>> {
    let d = data.clone();
    let record = FnTool::erased(
        "record_artifact",
        "Record a file this workflow produced, so later phases and the final summary can \
         reference it.",
        &[
            (
                "path",
                "Path of the produced file, relative to the working directory.",
                true,
            ),
            ("description", "What the file is.", true),
        ],
        move |input| {
            let mut list = match d.get("artifacts") {
                Some(Value::Array(a)) => a,
                _ => Vec::new(),
            };
            let path = arg(input, "path");
            list.retain(|a| a.get("path").and_then(Value::as_str) != Some(path.as_str()));
            list.push(json!({ "path": path, "description": arg(input, "description") }));
            let n = list.len();
            d.set("artifacts", Value::Array(list));
            Ok(format!("Recorded artifact {path} ({n} total)."))
        },
    );
    let d = data.clone();
    let list = FnTool::erased(
        "list_artifacts",
        "List the files this workflow has recorded.",
        &[],
        move |_| Ok(artifacts_block(&d)),
    );
    vec![record, list]
}

/// The recorded artifacts, rendered.
pub fn artifacts_block(data: &RunData) -> String {
    match data.get("artifacts") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|x| {
                format!(
                    "- {} — {}",
                    x.get("path").and_then(Value::as_str).unwrap_or_default(),
                    x.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "(no artifacts recorded)".into(),
    }
}

// ── ask_user ───────────────────────────────────────────────────────────────

/// The `ask_user` tool.
///
/// With no interactive user (headless runs) it answers `timed_out = true`,
/// exactly the signal agenthicc's prompt tells the agent to handle: make the
/// safest best-effort decision and record the assumption.
pub fn ask_user_tool(gate: Option<Arc<dyn UserQuestionGate>>) -> Arc<ErasedTool> {
    Arc::new(AskUserTool { gate }) as Arc<ErasedTool>
}

struct AskUserTool {
    gate: Option<Arc<dyn UserQuestionGate>>,
}

#[async_trait::async_trait]
impl Tool for AskUserTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user one or more focused questions about goals, constraints, priorities or \
         acceptance criteria when the answer cannot be discovered from the project. Returns \
         the answers, or timed_out=true when nobody answered."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The questions, each self-contained."
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let questions: Vec<String> = match input.get("questions") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|q| q.as_str().map(str::trim).map(str::to_string))
                .filter(|q| !q.is_empty())
                .collect(),
            Some(Value::String(s)) => s
                .lines()
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        if questions.is_empty() {
            return Ok(ToolResult::error(
                "ask_user() needs at least one question.",
                &ctx.tool_use_id,
            ));
        }
        let answers = match &self.gate {
            Some(gate) => gate.ask(&questions).await,
            None => None,
        };
        let body = match answers {
            Some(answers) => json!({
                "timed_out": false,
                "answers": questions.iter().zip(answers.iter().chain(std::iter::repeat(&String::new())))
                    .map(|(q, a)| json!({ "question": q, "answer": a }))
                    .collect::<Vec<_>>(),
            }),
            None => json!({
                "timed_out": true,
                "note": "No answer arrived. Make the safest best-effort decision, record the \
                         assumption, and do not pretend the user answered."
            }),
        };
        Ok(ToolResult::ok(body.to_string(), &ctx.tool_use_id))
    }
}

// ── workflow_authoring ─────────────────────────────────────────────────────

/// What the authoring tools may inspect.
#[derive(Clone)]
pub struct AuthoringContext {
    /// The run's working directory (drafts and publication live under it).
    pub working_dir: PathBuf,
    /// The session config, for workflow discovery.
    pub config: XaftConfig,
    /// `(name, description)` of every tool this session can grant a phase.
    pub available_tools: Vec<(String, String)>,
}

/// Where `create_workflow` keeps drafts: never discovered by the registry.
pub fn draft_dir(working_dir: &Path) -> PathBuf {
    project_workflow_dir(working_dir).join("_drafts")
}

const SCHEMA_DOC: &str = r#"xaft workflow definition (TOML). A file in .xaft/workflows/<name>.toml (or <name>/workflow.toml) is one workflow.

Top level:
  name = "lower_snake_case"        # optional in files (defaults to the file stem)
  description = "one line"
  mode = "phased"                  # runs the phase graph (implied when [[phases]] exist and mode is unset)
  mode_bindings = ["plan"]         # modes this workflow is the default for (auto/yolo, plan/review, safe/ask)
  max_total_phase_runs = 0         # hard cap on phase runs across the run (0 = none)
  model = "..."                    # model for every phase
  phase_models = { execute = "..." }  # per-phase models; `<phase>_model = "..."` also works
  stable_system_prompt = """..."""  # workflow contract placed at the top of every phase's system prompt

[[phases]] — one per phase, the first one starts the run:
  name = "plan"                    # lower_snake_case, unique
  agent_type = "auto"              # auto | planner | executor | reviewer | explorer | verifier | custom | human | script
  system_prompt = """..."""        # the phase's role prompt (or system_prompt_file = "x.md" in a package)
  prompt = """..."""               # the phase's task message; placeholders: {intent} {intent_head} {intent_tail}
                                   #   (the intent before / after its first '|') {phase} {workflow}
                                   #   {context} {iteration} {previous_output} {output.<phase>}
                                   #   {data.<phase>.<arg>} {state.<key>} {goals} {next_goal} {artifacts} {working_dir}
                                   #   for a human phase this is the question put to the user
  mode_override = "plan"           # plan/review/safe = read-only tools; auto/yolo = all tools
  allowed_capabilities = ["read", "search", "git_read"]   # read search write execute git_read git_write network memory
  tools = ["read_file", "grep"]    # optional exact allowlist applied after capabilities
  extra_tools = ["goal_list"]      # goal_list | workflow_state | artifacts | ask_user | workflow_authoring
                                   #   | ui_catalog | ui_ux_pro_max
  max_turns = 20                   # LLM sub-turns per phase run
  max_turns_param = "max_phase_turns"  # a workflow param whose positive integer replaces max_turns
  model_param = "study_model"      # a workflow param holding this phase's model (shared agenthicc knobs)
  max_iterations = -1              # times the phase may be entered (-1 unlimited); also caps continuation turns
  max_attempts = 5                 # continuation turns per phase run, without capping entries (overrides the above)
  next = "execute"                 # phase after success; omit to end the workflow
  on_reject = "plan"               # phase after a rejection
  on_error = "recover"             # phase after an error (otherwise the run fails and can be resumed)
  output_schema = "free_text"      # plan | review_result | free_text | json
  require_plan_finalization = true # tools: request_plan_approval(plan), finalize_plan(plan), exit_workflow(reason)
  require_explicit_completion = true  # tool: mark_execute_complete(summary)
  require_explicit_review = true   # tools: approve_review(summary), reject_review(reason)
  parallel_with = ["other_phase"]  # run sibling phases concurrently, then follow this phase's next
  terminal_wait_policy = "foreground"  # or "background"
  command_lifecycle = "oneshot"    # or "service" (needs background)
  require_successful_commands = false  # fail the phase if a shell command it ran failed
  continuation_prompt = """..."""  # sent when a turn ends without a required transition
  command = "cargo test"           # script phases only: run with sh -c, no model involved
  timeout_s = 600                  # script phases only
                                   # the command sees XAFT_WORKFLOW, XAFT_RUN_ID, XAFT_PHASE, XAFT_ITERATION,
                                   # XAFT_INTENT, XAFT_WORKING_DIR, XAFT_RUN_FILE (checkpoint JSON) and
                                   # XAFT_PREVIOUS_OUTPUT; a JSON object on stdout (or its last line) may set
                                   # approved, summary, next (phase or "end"), exit and state; otherwise exit
                                   # code 0 = approved, non-zero = rejected (on_reject, else the phase fails)

[[phases.transitions]] — custom transition tools for the phase above:
  tool = "submit_design"           # lower_snake_case, not a built-in transition name
  description = "..."              # when the model should call it
  params = [{ name = "design", description = "...", required = true, type = "string" }]
                                   # type: string | boolean | string_list | object_list
  next = "build"                   # target phase (default: the phase's next / on_reject)
  end = false                      # end the workflow instead
  outcome = "approved"             # approved | rejected | exit
  require_user_approval = false    # ask the user first
  guard = "goals_complete"         # goals_complete | goals_pending | has_artifacts | plan_approved | draft_valid
                                   #   | mobile_verified | implementation_plan_approved
  effect = "publish_workflow_draft"  # deterministic side effect on acceptance; the built-in site workflows'
                                   #   effects (copy_website, site_imitate, ui_ux_pro_max)
                                   #   also validate arguments and may choose the edge

Rules: a phase changes only when a transition tool succeeds — prose never advances it. A phase without
require_* flags or transitions runs one turn and follows `next`. A cycle of plain `next` edges needs
max_iterations or max_total_phase_runs."#;

const TEMPLATE: &str = r#"name = "my_workflow"
description = "Explore → Implement → Verify"
mode = "phased"

[[phases]]
name = "explore"
agent_type = "explorer"
max_turns = 15
next = "implement"
mode_override = "plan"
extra_tools = ["ask_user", "workflow_state"]
system_prompt = """
You are in the EXPLORE phase. Read the relevant code and record what you learn with
set_state(key, value). Ask the user about anything that would change the approach."""
prompt = """
{context}

Task: {intent}"""

[[phases.transitions]]
tool = "submit_findings"
description = "Exploration is complete; hand the findings to implementation."
params = [{ name = "findings", description = "What was learned and where the change goes." }]

[[phases]]
name = "implement"
agent_type = "executor"
max_turns = 40
next = "verify"
mode_override = "auto"
require_explicit_completion = true
system_prompt = """
You are in the IMPLEMENT phase. Use the findings: {output.explore}
Call mark_execute_complete(summary) when everything is implemented."""

[[phases]]
name = "verify"
agent_type = "reviewer"
max_turns = 10
max_iterations = 5
on_reject = "implement"
require_explicit_review = true
system_prompt = """
You are in the VERIFY phase. Run the tests. approve_review(summary) when they pass,
reject_review(reason) otherwise."""
"#;

/// The `workflow_authoring` group.
pub fn authoring_tools(ctx: &AuthoringContext, data: &RunData) -> Vec<Arc<ErasedTool>> {
    let mut tools = Vec::new();

    tools.push(FnTool::erased(
        "describe_workflow_schema",
        "Describe every field of an xaft workflow definition and its semantics.",
        &[],
        |_| Ok(SCHEMA_DOC.to_string()),
    ));

    tools.push(FnTool::erased(
        "list_agent_roles",
        "List the agent roles a phase can use and their default tool capabilities.",
        &[],
        |_| {
            Ok(KNOWN_AGENT_TYPES
                .iter()
                .map(|role| {
                    let caps = match super::prompt::role_default_capabilities(role) {
                        Some(c) => c.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", "),
                        None => "all (within the phase's mode)".to_string(),
                    };
                    format!(
                        "{role}: {} — default capabilities: {caps}",
                        super::prompt::role_summary(role)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        },
    ));

    let available = ctx.available_tools.clone();
    tools.push(FnTool::erased(
        "list_tool_capabilities",
        "List the tool capabilities a phase can allow, with the tools in each.",
        &[],
        move |_| {
            let mut out = Vec::new();
            for cap in ToolCapability::ALL {
                if cap == ToolCapability::Undeclared {
                    continue;
                }
                let members: Vec<&str> = available
                    .iter()
                    .filter(|(n, _)| capability_of(n) == cap)
                    .map(|(n, _)| n.as_str())
                    .collect();
                out.push(format!(
                    "{} — {}{}",
                    cap.as_str(),
                    cap.description(),
                    if members.is_empty() {
                        String::new()
                    } else {
                        format!("\n    tools: {}", members.join(", "))
                    }
                ));
            }
            out.push(format!(
                "Read-only roles default to: {}",
                READ_CAPS
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            Ok(out.join("\n"))
        },
    ));

    tools.push(FnTool::erased(
        "list_tool_groups",
        "List the named tool groups a phase can add with extra_tools.",
        &[],
        |_| {
            Ok(KNOWN_TOOL_GROUPS
                .iter()
                .map(|g| format!("{g}: {}", group_summary(g)))
                .collect::<Vec<_>>()
                .join("\n"))
        },
    ));

    let available = ctx.available_tools.clone();
    tools.push(FnTool::erased(
        "list_available_tools",
        "List every tool this session can grant a phase, with its capability.",
        &[],
        move |_| {
            Ok(available
                .iter()
                .map(|(n, d)| {
                    format!(
                        "{n} [{}] — {}",
                        capability_of(n),
                        d.lines().next().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        },
    ));

    tools.push(FnTool::erased(
        "show_workflow_template",
        "Show a complete, valid workflow template to adapt.",
        &[],
        |_| Ok(TEMPLATE.to_string()),
    ));

    let (cfg, wd) = (ctx.config.clone(), ctx.working_dir.clone());
    tools.push(FnTool::erased(
        "show_example_workflow",
        "Show the full TOML of an existing workflow (built-in or custom), e.g. code_plan or \
         create_workflow.",
        &[("name", "The workflow name.", true)],
        move |input| {
            let name = arg(input, "name");
            if let Some(src) = builtin::builtin_source(&name) {
                return Ok(src.to_string());
            }
            let registry = WorkflowRegistry::discover(&cfg, &wd);
            let entry = registry.resolve(&name).map_err(|e| e.to_string())?;
            match &entry.path {
                Some(p) if p.is_dir() => {
                    std::fs::read_to_string(p.join(PACKAGE_MANIFEST)).map_err(|e| e.to_string())
                }
                Some(p) => std::fs::read_to_string(p).map_err(|e| e.to_string()),
                None => toml::to_string_pretty(&entry.definition)
                    .or_else(|_| serde_json::to_string_pretty(&entry.definition))
                    .map_err(|e| e.to_string()),
            }
        },
    ));

    let (cfg, wd) = (ctx.config.clone(), ctx.working_dir.clone());
    tools.push(FnTool::erased(
        "list_workflows",
        "List every workflow available in this project, with its source and phases.",
        &[],
        move |_| {
            let registry = WorkflowRegistry::discover(&cfg, &wd);
            Ok(registry
                .entries()
                .iter()
                .map(|e| {
                    format!(
                        "{} [{}] {} — phases: {}",
                        e.name,
                        e.source.label(),
                        if e.description.is_empty() {
                            "no description"
                        } else {
                            &e.description
                        },
                        e.phase_path()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        },
    ));

    let (wd, d) = (ctx.working_dir.clone(), data.clone());
    tools.push(FnTool::erased(
        "draft_path_for",
        "Return the draft path to write a new workflow to (under .xaft/workflows/_drafts/, \
         which is never loaded until published).",
        &[("name", "The new workflow's lower_snake_case name.", true)],
        move |input| {
            let name = arg(input, "name");
            if !is_snake_identifier(&name) {
                return Err(format!("'{name}' is not a lower_snake_case workflow name"));
            }
            let path = draft_dir(&wd).join(format!("{name}.toml"));
            std::fs::create_dir_all(draft_dir(&wd)).map_err(|e| e.to_string())?;
            d.set("draft_name", json!(name));
            Ok(format!(
                "Write the workflow to: {}\n(For a package with prompt files, use the directory {} \
                 containing workflow.toml instead.)",
                path.display(),
                draft_dir(&wd).join(&name).display()
            ))
        },
    ));

    let (wd, d) = (ctx.working_dir.clone(), data.clone());
    tools.push(FnTool::erased(
        "validate_workflow_draft",
        "Load and validate a workflow draft deterministically. Reports every error and \
         warning; a draft with errors cannot be published.",
        &[("path", "Path of the draft file or package directory.", true)],
        move |input| {
            let raw = PathBuf::from(arg(input, "path"));
            let path = if raw.is_absolute() { raw } else { wd.join(raw) };
            let drafts = draft_dir(&wd);
            if !is_inside(&path, &drafts) {
                return Err(format!(
                    "drafts must live under {}; got {}",
                    drafts.display(),
                    path.display()
                ));
            }
            d.set("draft_valid", json!(false));
            let loaded = load_workflow_path(&path).map_err(|e| format!("load failed: {e}"))?;
            let report = validate_definition(&loaded.name, &loaded.definition);
            let mut lines = vec![format!(
                "Workflow '{}' — {} phase(s): {}",
                loaded.name,
                loaded.definition.phases.len(),
                loaded
                    .definition
                    .phases
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            )];
            if !loaded.definition.is_phased() {
                lines.push(
                    "error: the draft is not a phased workflow (set mode = \"phased\" and \
                     declare [[phases]])"
                        .into(),
                );
            }
            if builtin::builtin_source(&loaded.name).is_some()
                || ["standard", "dynamic", "meta"].contains(&loaded.name.as_str())
            {
                lines.push(format!(
                    "warning: '{}' shadows a built-in workflow of the same name",
                    loaded.name
                ));
            }
            lines.push(report.render());
            let ok = report.is_ok() && loaded.definition.is_phased();
            d.set("draft_valid", json!(ok));
            d.set("draft_path", json!(path.display().to_string()));
            d.set("draft_name", json!(loaded.name));
            lines.push(if ok {
                "RESULT: valid — the draft can be published.".into()
            } else {
                "RESULT: invalid — fix every error and validate again.".into()
            });
            Ok(lines.join("\n"))
        },
    ));

    tools
}

/// One-line description of a tool group.
pub fn group_summary(group: &str) -> &'static str {
    match group {
        "goal_list" => {
            "an ordered goal list: set_goals, append_goal, insert_goal, list_goals, \
             complete_goal, skip_goal; guards goals_complete and goals_pending"
        }
        "workflow_state" => "named values shared across phases: set_state, get_state, list_state",
        "artifacts" => {
            "files the workflow produced: record_artifact, list_artifacts; guard has_artifacts"
        }
        "ask_user" => "ask_user(questions) for requirements clarification",
        "workflow_authoring" => {
            "the create_workflow surface: schema, roles, capabilities, tool groups, templates, \
             examples, drafts and validation; guard draft_valid, effect publish_workflow_draft"
        }
        "ui_catalog" => super::ui_catalog::GROUP_SUMMARY,
        "ui_ux_pro_max" => super::ui_ux_pro_max::GROUP_SUMMARY,
        _ => "unknown group",
    }
}

/// Whether `path` is inside `dir`, comparing normalised components so `..`
/// cannot climb out.
fn is_inside(path: &Path, dir: &Path) -> bool {
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
    let (p, d) = (normalise(path), normalise(dir));
    p.len() > d.len()
        && p.starts_with(&d)
        && !p[d.len()..]
            .iter()
            .any(|seg| is_hidden_entry(seg) && seg != "_drafts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phased::tools::PhaseControl;

    fn ctx() -> ToolContext {
        ToolContext::new("t")
    }

    fn tool<'a>(tools: &'a [Arc<ErasedTool>], name: &str) -> &'a Arc<ErasedTool> {
        tools.iter().find(|t| t.name() == name).unwrap()
    }

    #[tokio::test]
    async fn goal_list_lifecycle_and_guard() {
        let data = RunData::default();
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        let tools = goal_list_tools(&data);
        assert!(check_guard("goals_complete", &data, &control).is_err());

        tool(&tools, "set_goals")
            .call(json!({"goals": "- write code\n- write tests"}), &ctx())
            .await
            .unwrap();
        tool(&tools, "insert_goal")
            .call(json!({"before_id": "2", "goal": "refactor"}), &ctx())
            .await
            .unwrap();
        let listed = tool(&tools, "list_goals")
            .call(json!({}), &ctx())
            .await
            .unwrap();
        assert_eq!(
            listed.content,
            "[ ] #1 write code\n[ ] #3 refactor\n[ ] #2 write tests"
        );
        assert_eq!(next_goal(&data), "#1 write code");

        for id in ["1", "3"] {
            tool(&tools, "complete_goal")
                .call(json!({"id": id, "evidence": "done"}), &ctx())
                .await
                .unwrap();
        }
        let err = check_guard("goals_complete", &data, &control).unwrap_err();
        assert!(err.contains("#2 write tests"), "{err}");
        assert!(check_guard("goals_pending", &data, &control).is_ok());
        tool(&tools, "skip_goal")
            .call(json!({"id": "#2", "reason": "covered"}), &ctx())
            .await
            .unwrap();
        assert!(check_guard("goals_complete", &data, &control).is_ok());
        assert!(check_guard("goals_pending", &data, &control).is_err());

        let r = tool(&tools, "set_goals")
            .call(json!({"goals": "again"}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error, "cannot replace goals already in progress");
    }

    #[tokio::test]
    async fn state_and_artifacts_round_trip() {
        let data = RunData::default();
        let state = workflow_state_tools(&data);
        tool(&state, "set_state")
            .call(json!({"key": "stack", "value": "rust"}), &ctx())
            .await
            .unwrap();
        let got = tool(&state, "get_state")
            .call(json!({"key": "stack"}), &ctx())
            .await
            .unwrap();
        assert_eq!(got.content, "rust");
        let bad = tool(&state, "set_state")
            .call(json!({"key": "Bad Key", "value": "x"}), &ctx())
            .await
            .unwrap();
        assert!(bad.is_error);

        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        assert!(check_guard("has_artifacts", &data, &control).is_err());
        let arts = artifact_tools(&data);
        tool(&arts, "record_artifact")
            .call(json!({"path": "out.md", "description": "report"}), &ctx())
            .await
            .unwrap();
        assert!(check_guard("has_artifacts", &data, &control).is_ok());
        assert_eq!(artifacts_block(&data), "- out.md — report");
    }

    #[tokio::test]
    async fn ask_user_without_a_user_times_out() {
        let t = ask_user_tool(None);
        let r = t
            .call(json!({"questions": ["Which database?"]}), &ctx())
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["timed_out"], json!(true));
    }

    const DRAFT: &str = r#"
name = "tiny"
description = "Tiny."
mode = "phased"

[[phases]]
name = "only"
"#;

    fn authoring(wd: &Path) -> (Vec<Arc<ErasedTool>>, RunData) {
        let data = RunData::new(BTreeMap::new(), wd);
        let ctx = AuthoringContext {
            working_dir: wd.to_path_buf(),
            config: XaftConfig::default(),
            available_tools: vec![("read_file".into(), "Read a file.".into())],
        };
        (authoring_tools(&ctx, &data), data)
    }

    #[tokio::test]
    async fn draft_validate_publish_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let (tools, data) = authoring(tmp.path());
        tool(&tools, "draft_path_for")
            .call(json!({"name": "tiny"}), &ctx())
            .await
            .unwrap();
        let draft = draft_dir(tmp.path()).join("tiny.toml");
        std::fs::write(&draft, DRAFT).unwrap();

        let report = tool(&tools, "validate_workflow_draft")
            .call(json!({"path": draft.display().to_string()}), &ctx())
            .await
            .unwrap();
        assert!(
            report.content.contains("RESULT: valid"),
            "{}",
            report.content
        );
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        assert!(check_guard("draft_valid", &data, &control).is_ok());

        let note = run_effect("publish_workflow_draft", &data, &json!({"name": "tiny"})).unwrap();
        assert!(note.contains("Published workflow 'tiny'"));
        let published = project_workflow_dir(tmp.path()).join("tiny.toml");
        assert!(published.is_file());

        // The published workflow is immediately discoverable and runnable.
        let registry = WorkflowRegistry::discover(&XaftConfig::default(), tmp.path());
        assert!(registry.get("tiny").unwrap().is_phased());
        // Drafts never are.
        assert!(registry.names().iter().all(|n| *n != "_drafts"));

        // Publishing again backs the previous version up.
        run_effect("publish_workflow_draft", &data, &json!({})).unwrap();
        let backups = project_workflow_dir(tmp.path()).join("_backups");
        assert_eq!(std::fs::read_dir(backups).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn an_invalid_draft_cannot_be_published() {
        let tmp = tempfile::tempdir().unwrap();
        let (tools, data) = authoring(tmp.path());
        std::fs::create_dir_all(draft_dir(tmp.path())).unwrap();
        let draft = draft_dir(tmp.path()).join("bad.toml");
        std::fs::write(&draft, "[[phases]]\nname = \"a\"\nnext = \"ghost\"\n").unwrap();
        let report = tool(&tools, "validate_workflow_draft")
            .call(json!({"path": draft.display().to_string()}), &ctx())
            .await
            .unwrap();
        assert!(
            report.content.contains("RESULT: invalid"),
            "{}",
            report.content
        );
        let control: SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
        assert!(check_guard("draft_valid", &data, &control).is_err());
        assert!(run_effect("publish_workflow_draft", &data, &json!({})).is_err());
    }

    #[tokio::test]
    async fn drafts_outside_the_draft_dir_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (tools, _) = authoring(tmp.path());
        let r = tool(&tools, "validate_workflow_draft")
            .call(
                json!({"path": format!("{}/.xaft/workflows/_drafts/../../../etc/x.toml", tmp.path().display())}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("drafts must live under"));
    }

    #[tokio::test]
    async fn examples_and_listings_cover_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let (tools, _) = authoring(tmp.path());
        let ex = tool(&tools, "show_example_workflow")
            .call(json!({"name": "code_plan"}), &ctx())
            .await
            .unwrap();
        assert!(ex.content.contains("require_plan_finalization"));
        let list = tool(&tools, "list_workflows")
            .call(json!({}), &ctx())
            .await
            .unwrap();
        assert!(list.content.contains("create_workflow [builtin]"));
        let template = tool(&tools, "show_workflow_template")
            .call(json!({}), &ctx())
            .await
            .unwrap();
        let def =
            xaft_config::workflows::discovery::parse_workflow_toml(&template.content).unwrap();
        assert!(
            validate_definition("my_workflow", &def).is_ok(),
            "the template itself must validate"
        );
    }
}
