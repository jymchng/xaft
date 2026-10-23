//! Phase-transition tools.
//!
//! In agenthicc a phase changes only when a transition tool call succeeds —
//! "prose such as 'done' never advances the workflow". The same contract
//! holds here: every tool in this module writes a [`PhaseDecision`] into
//! state shared with the engine, and the engine reads that decision after the
//! agent's turn ends.
//!
//! Two kinds exist:
//!
//! * the **built-in** tools selected by a phase's `require_*` flags —
//!   `request_plan_approval` / `finalize_plan` / `exit_workflow`,
//!   `mark_execute_complete`, `approve_review` / `reject_review` (the names
//!   agenthicc's `code_plan/phase_tools.py` uses);
//! * one [`DeclaredTransitionTool`] per `[[phases.transitions]]` entry, whose
//!   name, typed parameters, edge, guard and effect come from the definition.
//!   Guards and effects are dispatched by [`super::hooks`]; an effect may
//!   replace the declared edge with one it computes (a component loop, a
//!   route profile, a validated re-entry), as agenthicc's runners do.
//!
//! The first accepted transition wins; any later call in the same phase run
//! is refused, so an agent cannot transition twice.

use std::sync::{Arc, Mutex};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::signals::{SignalBus, ToolPendingApproval};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};

use xaft_config::types::{TransitionDefinition, TransitionParam};

use super::groups::RunData;
use super::hooks::{HookContext, Route, apply_transition_effect, check_transition_guard};

/// How a transition resolves the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The phase's work is accepted.
    Approved,
    /// The phase's work is rejected (`on_reject` applies).
    Rejected,
    /// The workflow ends now.
    Exit,
}

/// Where the workflow goes after the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edge {
    /// Follow the phase's own `next` / `on_reject`.
    Default,
    /// Enter this phase.
    Phase(String),
    /// End the workflow.
    End,
}

/// The decision a transition tool recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseDecision {
    /// The tool that decided.
    pub tool: String,
    /// The verdict.
    pub outcome: Outcome,
    /// The chosen edge.
    pub edge: Edge,
    /// The text that becomes the phase output.
    pub text: String,
    /// The tool's arguments, as structured output.
    pub args: Value,
}

/// Mutable per-phase-run state shared by the transition tools and the engine.
#[derive(Debug, Default)]
pub struct PhaseControl {
    /// The accepted decision, once one is made.
    pub decision: Option<PhaseDecision>,
    /// Whether `request_plan_approval` was approved in this phase run.
    pub plan_approved: bool,
    /// The plan text the user approved.
    pub approved_plan: Option<String>,
}

/// Shared handle to a phase run's [`PhaseControl`].
pub type SharedControl = Arc<Mutex<PhaseControl>>;

/// Everything a transition tool needs besides its own definition.
#[derive(Clone)]
pub struct TransitionEnv {
    /// The phase run's decision slot.
    pub control: SharedControl,
    /// The run's workflow data (goals, drafts, …).
    pub data: RunData,
    /// Interactive approver, when a user is present.
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// The bus the approval dialog listens on (see [`ask_user_approval`]).
    pub signals: Option<Arc<SignalBus>>,
    /// The phase this run belongs to, for hooks that act per phase.
    pub phase: String,
}

/// Ask the user to approve `tool` with `input`, the way the agtrs executor
/// does for a tool that requires confirmation.
///
/// The executor emits [`ToolPendingApproval`] *before* calling
/// [`ApprovalGate::request`]; the TUI opens its approval dialog on that signal
/// and the gate only waits for the answer. A direct `request()` without the
/// signal would show no dialog and time out as a rejection, so every
/// engine-initiated approval goes through here. With no gate (headless) the
/// call is approved.
pub async fn ask_user_approval(
    gate: Option<&Arc<dyn ApprovalGate>>,
    signals: Option<&Arc<SignalBus>>,
    tool: &str,
    tool_use_id: &str,
    input: &Value,
) -> bool {
    let Some(gate) = gate else {
        return true;
    };
    if let Some(bus) = signals {
        bus.emit(ToolPendingApproval {
            agent_id: "workflow".to_string(),
            agent_run_id: String::new(),
            tool_name: tool.to_string(),
            tool_use_id: tool_use_id.to_string(),
            input: input.clone(),
        })
        .await;
    }
    gate.request(tool, tool_use_id, input).await
}

impl TransitionEnv {
    /// The tool that already transitioned this phase run, if any.
    pub(crate) fn already_decided(&self) -> Option<String> {
        let guard = self.control.lock().ok()?;
        guard.decision.as_ref().map(|d| d.tool.clone())
    }

    fn decide(&self, decision: PhaseDecision) {
        if let Ok(mut guard) = self.control.lock() {
            if guard.decision.is_none() {
                guard.decision = Some(decision);
            }
        }
    }

    /// Record a transition decided by a workflow tool group — a *group
    /// transition*, such as `dispatch_parallel_workers` or a `goal_list` phase
    /// control of the active goal. Returns `false` (and records nothing) when
    /// the phase already transitioned: the first transition wins.
    pub(crate) fn decide_group_transition(&self, decision: PhaseDecision) -> bool {
        let Ok(mut guard) = self.control.lock() else {
            return false;
        };
        if guard.decision.is_some() {
            return false;
        }
        guard.decision = Some(decision);
        true
    }
}

/// What a tool result says after an accepted transition.
pub(crate) const STOP_NOTE: &str = "Phase transition accepted. Stop now — do not call any more tools; the workflow runner takes over.";

fn string_arg(input: &Value, key: &str) -> String {
    match input.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn refuse_if_decided(env: &TransitionEnv, ctx: &ToolContext) -> Option<ToolResult> {
    env.already_decided().map(|tool| {
        ToolResult::error(
            format!(
                "This phase already transitioned via {tool}(). Stop now; the runner takes over."
            ),
            &ctx.tool_use_id,
        )
    })
}

// ── Built-in transition tools ──────────────────────────────────────────────

/// The built-in tools a phase's `require_*` flag selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTransition {
    /// `request_plan_approval(plan)` — asks the user to approve a plan.
    RequestPlanApproval,
    /// `finalize_plan(plan)` — ends a planning phase with the approved plan.
    FinalizePlan,
    /// `exit_workflow(reason)` — ends the whole run early.
    ExitWorkflow,
    /// `mark_execute_complete(summary)` — ends an execution phase.
    MarkExecuteComplete,
    /// `approve_review(summary)` — accepts the reviewed work.
    ApproveReview,
    /// `reject_review(reason)` — rejects it (`on_reject` applies).
    RejectReview,
}

impl BuiltinTransition {
    /// The tool name.
    pub fn name(self) -> &'static str {
        match self {
            Self::RequestPlanApproval => "request_plan_approval",
            Self::FinalizePlan => "finalize_plan",
            Self::ExitWorkflow => "exit_workflow",
            Self::MarkExecuteComplete => "mark_execute_complete",
            Self::ApproveReview => "approve_review",
            Self::RejectReview => "reject_review",
        }
    }

    /// Whether calling it ends the phase (`request_plan_approval` does not).
    pub fn is_transition(self) -> bool {
        !matches!(self, Self::RequestPlanApproval)
    }

    fn description(self) -> &'static str {
        match self {
            Self::RequestPlanApproval => {
                "Present the complete plan to the user for approval. Returns whether it \
                 was approved. Call finalize_plan(plan) after approval."
            }
            Self::FinalizePlan => {
                "Finalize the approved plan and end the planning phase. Only call this \
                 after request_plan_approval() approved the plan."
            }
            Self::ExitWorkflow => {
                "End the workflow now because the request does not fit it. Explain why \
                 and what the user should do instead."
            }
            Self::MarkExecuteComplete => {
                "Mark the implementation complete and end the execution phase. Call only \
                 when ALL planned work is done."
            }
            Self::ApproveReview => {
                "Approve the reviewed work: tests pass and the changes are correct. Ends \
                 the review phase."
            }
            Self::RejectReview => {
                "Reject the reviewed work and send it back for fixes. Explain exactly what \
                 must change."
            }
        }
    }

    fn param(self) -> (&'static str, &'static str) {
        match self {
            Self::RequestPlanApproval | Self::FinalizePlan => {
                ("plan", "The complete plan, in markdown.")
            }
            Self::ExitWorkflow => (
                "reason",
                "Why the workflow is ending and what to do instead.",
            ),
            Self::MarkExecuteComplete | Self::ApproveReview => {
                ("summary", "A brief summary of the work.")
            }
            Self::RejectReview => ("reason", "The issues that must be fixed."),
        }
    }
}

/// One built-in transition tool bound to a phase run.
pub struct BuiltinTransitionTool {
    kind: BuiltinTransition,
    env: TransitionEnv,
}

impl BuiltinTransitionTool {
    /// Bind `kind` to a phase run.
    pub fn new(kind: BuiltinTransition, env: TransitionEnv) -> Self {
        Self { kind, env }
    }

    /// Erase for a tool list.
    pub fn erased(kind: BuiltinTransition, env: TransitionEnv) -> Arc<ErasedTool> {
        Arc::new(Self::new(kind, env)) as Arc<ErasedTool>
    }
}

#[async_trait::async_trait]
impl Tool for BuiltinTransitionTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        self.kind.description()
    }

    fn schema(&self) -> Value {
        let (param, desc) = self.kind.param();
        json!({
            "type": "object",
            "properties": { param: { "type": "string", "description": desc } },
            "required": [param],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if let Some(refusal) = refuse_if_decided(&self.env, ctx) {
            return Ok(refusal);
        }
        let (param, _) = self.kind.param();
        let text = string_arg(&input, param);
        if text.trim().is_empty() {
            return Ok(ToolResult::error(
                format!(
                    "{}() requires a non-empty '{param}' argument.",
                    self.kind.name()
                ),
                &ctx.tool_use_id,
            ));
        }

        let decision = |outcome: Outcome, edge: Edge| PhaseDecision {
            tool: self.kind.name().to_string(),
            outcome,
            edge,
            text: text.clone(),
            args: json!({ param: text.clone() }),
        };

        match self.kind {
            BuiltinTransition::RequestPlanApproval => {
                let approved = ask_user_approval(
                    self.env.approval_gate.as_ref(),
                    self.env.signals.as_ref(),
                    self.kind.name(),
                    &ctx.tool_use_id,
                    &json!({ "plan": text }),
                )
                .await;
                if let Ok(mut c) = self.env.control.lock() {
                    c.plan_approved = approved;
                    c.approved_plan = approved.then(|| text.clone());
                }
                if approved {
                    self.env.data.set("plan_approved", json!(true));
                    let how = if self.env.approval_gate.is_some() {
                        "The user approved the plan."
                    } else {
                        "No interactive user is attached, so the plan was auto-approved."
                    };
                    Ok(ToolResult::ok(
                        format!("{how} Now call finalize_plan(plan) with the approved plan."),
                        &ctx.tool_use_id,
                    ))
                } else {
                    Ok(ToolResult::error(
                        "The user did not approve the plan. Revise it — ask_user for what \
                         should change if it is unclear — and call request_plan_approval() again.",
                        &ctx.tool_use_id,
                    ))
                }
            }
            BuiltinTransition::FinalizePlan => {
                let approved = self
                    .env
                    .control
                    .lock()
                    .map(|c| c.plan_approved)
                    .unwrap_or(false);
                if !approved && self.env.approval_gate.is_some() {
                    return Ok(ToolResult::error(
                        "finalize_plan() requires an approved plan: call \
                         request_plan_approval(plan) first.",
                        &ctx.tool_use_id,
                    ));
                }
                self.env.data.set("plan", json!(text));
                self.env.decide(decision(Outcome::Approved, Edge::Default));
                Ok(ToolResult::ok(STOP_NOTE, &ctx.tool_use_id))
            }
            BuiltinTransition::ExitWorkflow => {
                self.env.decide(decision(Outcome::Exit, Edge::End));
                Ok(ToolResult::ok(
                    "The workflow will end after this turn. Give the user a one-paragraph \
                     explanation and stop.",
                    &ctx.tool_use_id,
                ))
            }
            BuiltinTransition::MarkExecuteComplete | BuiltinTransition::ApproveReview => {
                self.env.decide(decision(Outcome::Approved, Edge::Default));
                Ok(ToolResult::ok(STOP_NOTE, &ctx.tool_use_id))
            }
            BuiltinTransition::RejectReview => {
                self.env.decide(decision(Outcome::Rejected, Edge::Default));
                Ok(ToolResult::ok(STOP_NOTE, &ctx.tool_use_id))
            }
        }
    }
}

// ── Declared transition tools ──────────────────────────────────────────────

/// A `[[phases.transitions]]` entry bound to a phase run.
pub struct DeclaredTransitionTool {
    def: TransitionDefinition,
    description: String,
    env: TransitionEnv,
}

impl DeclaredTransitionTool {
    /// Bind `def` to a phase run.
    pub fn new(def: TransitionDefinition, env: TransitionEnv) -> Self {
        let mut description = def.description.trim().to_string();
        if description.is_empty() {
            description = format!("Transition tool {}.", def.tool);
        }
        description.push_str(" Calling it ends the current phase.");
        Self {
            def,
            description,
            env,
        }
    }

    /// Erase for a tool list.
    pub fn erased(def: TransitionDefinition, env: TransitionEnv) -> Arc<ErasedTool> {
        Arc::new(Self::new(def, env)) as Arc<ErasedTool>
    }
}

#[async_trait::async_trait]
impl Tool for DeclaredTransitionTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        &self.def.tool
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &self.def.params {
            properties.insert(p.name.clone(), param_schema(p));
            if p.required {
                required.push(Value::String(p.name.clone()));
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
        if let Some(refusal) = refuse_if_decided(&self.env, ctx) {
            return Ok(refusal);
        }

        let mut args = serde_json::Map::new();
        let mut missing = Vec::new();
        let mut invalid = Vec::new();
        for p in &self.def.params {
            match param_value(p, &input) {
                Ok(Some(value)) => {
                    args.insert(p.name.clone(), value);
                }
                Ok(None) => {
                    if p.required {
                        missing.push(p.name.clone());
                    }
                    // An absent optional string or list still gets an empty
                    // value, so effects read it without a presence check; an
                    // absent flag, number or object is left out.
                    match p.json_type().0 {
                        "string" => {
                            args.insert(p.name.clone(), Value::String(String::new()));
                        }
                        "array" => {
                            args.insert(p.name.clone(), Value::Array(Vec::new()));
                        }
                        _ => {}
                    }
                }
                Err(e) => invalid.push(format!("{}: {e}", p.name)),
            }
        }
        if !missing.is_empty() {
            return Ok(ToolResult::error(
                format!(
                    "{}() is missing required argument(s): {}",
                    self.def.tool,
                    missing.join(", ")
                ),
                &ctx.tool_use_id,
            ));
        }
        if !invalid.is_empty() {
            return Ok(ToolResult::error(
                format!(
                    "{}() rejected its arguments: {}",
                    self.def.tool,
                    invalid.join("; ")
                ),
                &ctx.tool_use_id,
            ));
        }
        let args = Value::Object(args);
        let hook = HookContext {
            data: &self.env.data,
            control: &self.env.control,
            args: &args,
            phase: &self.env.phase,
            tool: &self.def.tool,
        };

        if let Some(guard) = &self.def.guard {
            if let Err(unmet) = check_transition_guard(guard, &hook) {
                return Ok(ToolResult::error(
                    format!("{}() is not available yet: {unmet}", self.def.tool),
                    &ctx.tool_use_id,
                ));
            }
        }

        if self.def.require_user_approval
            && !ask_user_approval(
                self.env.approval_gate.as_ref(),
                self.env.signals.as_ref(),
                &self.def.tool,
                &ctx.tool_use_id,
                &args,
            )
            .await
        {
            return Ok(ToolResult::error(
                format!(
                    "The user declined {}(). Continue the phase: address their concerns \
                     (ask_user if unclear) before calling it again.",
                    self.def.tool
                ),
                &ctx.tool_use_id,
            ));
        }

        let mut effect_note = String::new();
        let mut route = None;
        if let Some(effect) = &self.def.effect {
            match apply_transition_effect(effect, &hook) {
                Ok(out) => {
                    effect_note = out.note;
                    route = out.route;
                }
                Err(err) => {
                    return Ok(ToolResult::error(
                        format!(
                            "{}() could not complete its effect '{effect}': {err}",
                            self.def.tool
                        ),
                        &ctx.tool_use_id,
                    ));
                }
            }
        }

        let mut outcome = match self.def.outcome_or_default() {
            "rejected" => Outcome::Rejected,
            "exit" => Outcome::Exit,
            _ => Outcome::Approved,
        };
        let edge = match route {
            // An effect chose the edge (a loop cursor, a route profile, a
            // validated re-entry): it replaces the declared one.
            Some(Route::Phase(p)) => Edge::Phase(p),
            Some(Route::End) => Edge::End,
            Some(Route::Exit) => {
                outcome = Outcome::Exit;
                Edge::End
            }
            None if self.def.end || outcome == Outcome::Exit => Edge::End,
            None => match &self.def.next {
                Some(next) => Edge::Phase(next.clone()),
                None => Edge::Default,
            },
        };
        let text = summarize_args(&args, &self.def.params);
        self.env.decide(PhaseDecision {
            tool: self.def.tool.clone(),
            outcome,
            edge,
            text,
            args,
        });
        let note = if effect_note.is_empty() {
            STOP_NOTE.to_string()
        } else {
            format!("{effect_note}\n{STOP_NOTE}")
        };
        Ok(ToolResult::ok(note, &ctx.tool_use_id))
    }
}

/// The JSON schema of one declared parameter.
fn param_schema(p: &TransitionParam) -> Value {
    let mut schema = match p.json_type() {
        ("boolean", _) => json!({ "type": "boolean" }),
        ("integer", _) => json!({ "type": "integer" }),
        ("object", _) => json!({ "type": "object" }),
        ("array", items) => {
            let items = match items.unwrap_or("string") {
                "object" => json!({ "type": "object" }),
                "any" => json!({}),
                _ => json!({ "type": "string" }),
            };
            json!({ "type": "array", "items": items })
        }
        _ => json!({ "type": "string" }),
    };
    schema["description"] = json!(p.description);
    schema
}

/// Read and normalise one declared parameter from a call's input.
///
/// `Ok(None)` is "not supplied": absent, a blank string, or a list with no
/// usable item (so a required list must not be empty). Models sometimes send
/// a flag or a list as text, so `"true"` reads as a boolean, `"3"` as an
/// integer, a JSON array or object inside a string as itself, and a list of
/// strings may also arrive as newline-separated text; anything else of the
/// wrong type is refused rather than coerced, and the call is rejected with
/// the reason.
fn param_value(p: &TransitionParam, input: &Value) -> Result<Option<Value>, String> {
    let raw = input.get(&p.name).filter(|v| !v.is_null());
    match p.json_type() {
        ("boolean", _) => match raw {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(Value::Bool(*b))),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "" => Ok(None),
                "true" | "yes" | "1" => Ok(Some(Value::Bool(true))),
                "false" | "no" | "0" => Ok(Some(Value::Bool(false))),
                _ => Err(format!("must be true or false, got {s:?}")),
            },
            Some(other) => Err(format!("must be true or false, got {other}")),
        },
        ("integer", _) => match raw {
            None => Ok(None),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => {
                Ok(Some(Value::Number(n.clone())))
            }
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => s
                .trim()
                .parse::<i64>()
                .map(|n| Some(json!(n)))
                .map_err(|_| format!("must be an integer, got {s:?}")),
            Some(other) => Err(format!("must be an integer, got {other}")),
        },
        ("object", _) => {
            let parsed = match raw {
                None => return Ok(None),
                Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
                Some(Value::String(s)) => serde_json::from_str::<Value>(s.trim())
                    .map_err(|_| "must be a JSON object".to_string())?,
                Some(other) => other.clone(),
            };
            if parsed.is_object() {
                Ok(Some(parsed))
            } else {
                Err("must be a JSON object".to_string())
            }
        }
        ("array", items) => {
            let items = items.unwrap_or("string");
            let list: Vec<Value> = match raw {
                None => return Ok(None),
                Some(Value::Array(a)) => a.clone(),
                Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s.trim()) {
                    Ok(Value::Array(a)) => a,
                    _ if items == "string" => s.lines().map(|l| json!(l)).collect(),
                    _ => return Err("must be a JSON array".to_string()),
                },
                Some(other) if items == "string" => vec![other.clone()],
                Some(_) => return Err("must be a JSON array".to_string()),
            };
            let list: Vec<Value> = match items {
                // Each item as trimmed text; blank items are dropped.
                "string" => list
                    .into_iter()
                    .map(|v| match v {
                        Value::String(s) => s.trim().to_string(),
                        other => other.to_string(),
                    })
                    .filter(|s| !s.is_empty())
                    .map(Value::String)
                    .collect(),
                "object" => list.into_iter().filter(Value::is_object).collect(),
                _ => list,
            };
            Ok((!list.is_empty()).then_some(Value::Array(list)))
        }
        _ => {
            let value = string_arg(input, &p.name);
            Ok((!value.trim().is_empty()).then_some(Value::String(value)))
        }
    }
}

/// Render one argument value as text: a list as `- item` lines, an object
/// item as `key: value; key: value`.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => format!("- {s}"),
                Value::Object(map) => format!(
                    "- {}",
                    map.iter()
                        .map(|(k, v)| format!(
                            "{k}: {}",
                            v.as_str().map_or(v.to_string(), str::to_string)
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
                other => format!("- {other}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Render transition arguments as the phase's output text, in declaration
/// order: a single argument as itself, several as `name: value` lines (a list
/// as `name:` followed by its items).
fn summarize_args(args: &Value, params: &[TransitionParam]) -> String {
    let Value::Object(map) = args else {
        return args.to_string();
    };
    if map.len() == 1 {
        return map.values().next().map(render_value).unwrap_or_default();
    }
    let line = |k: &str, v: &Value| match v {
        Value::Array(_) => format!("{k}:\n{}", render_value(v)),
        _ => format!("{k}: {}", render_value(v)),
    };
    let mut lines: Vec<String> = params
        .iter()
        .filter_map(|p| map.get(&p.name).map(|v| line(&p.name, v)))
        .collect();
    for (k, v) in map {
        if !params.iter().any(|p| &p.name == k) {
            lines.push(line(k, v));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::approval::{AlwaysReject, AutoApprove};
    use xaft_config::types::TransitionParam;

    fn env(gate: Option<Arc<dyn ApprovalGate>>) -> TransitionEnv {
        TransitionEnv {
            control: Arc::new(Mutex::new(PhaseControl::default())),
            data: RunData::default(),
            approval_gate: gate,
            signals: None,
            phase: "test".into(),
        }
    }

    /// The TUI opens its dialog on `ToolPendingApproval`; the gate only waits.
    /// Without the signal an interactive approval would time out as a refusal.
    #[tokio::test]
    async fn an_engine_approval_announces_itself_before_asking_the_gate() {
        let bus = Arc::new(SignalBus::new());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let s = Arc::clone(&seen);
        bus.on::<ToolPendingApproval>(move |ev| {
            s.lock()
                .unwrap()
                .push((ev.tool_name.clone(), ev.tool_use_id.clone()));
        })
        .await;
        let mut e = env(Some(Arc::new(AutoApprove)));
        e.signals = Some(Arc::clone(&bus));
        let request = BuiltinTransitionTool::new(BuiltinTransition::RequestPlanApproval, e);
        let r = request.call(json!({"plan": "p"}), &ctx()).await.unwrap();
        assert!(!r.is_error);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            *seen.lock().unwrap(),
            [("request_plan_approval".to_string(), "t1".to_string())]
        );
    }

    fn ctx() -> ToolContext {
        ToolContext::new("t1")
    }

    fn decision(env: &TransitionEnv) -> Option<PhaseDecision> {
        env.control.lock().unwrap().decision.clone()
    }

    #[tokio::test]
    async fn finalize_plan_requires_approval_when_a_user_is_attached() {
        let e = env(Some(Arc::new(AutoApprove)));
        let finalize = BuiltinTransitionTool::new(BuiltinTransition::FinalizePlan, e.clone());
        let r = finalize.call(json!({"plan": "p"}), &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(decision(&e).is_none());

        let request = BuiltinTransitionTool::new(BuiltinTransition::RequestPlanApproval, e.clone());
        let r = request.call(json!({"plan": "p"}), &ctx()).await.unwrap();
        assert!(!r.is_error, "{}", r.content);
        let r = finalize.call(json!({"plan": "p"}), &ctx()).await.unwrap();
        assert!(!r.is_error);
        let d = decision(&e).unwrap();
        assert_eq!(d.outcome, Outcome::Approved);
        assert_eq!(d.text, "p");
        assert_eq!(e.data.get("plan"), Some(json!("p")));
    }

    #[tokio::test]
    async fn a_rejected_plan_is_reported_and_finalize_stays_blocked() {
        let e = env(Some(Arc::new(AlwaysReject)));
        let request = BuiltinTransitionTool::new(BuiltinTransition::RequestPlanApproval, e.clone());
        let r = request.call(json!({"plan": "p"}), &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("did not approve"));
        let finalize = BuiltinTransitionTool::new(BuiltinTransition::FinalizePlan, e.clone());
        assert!(
            finalize
                .call(json!({"plan": "p"}), &ctx())
                .await
                .unwrap()
                .is_error
        );
    }

    #[tokio::test]
    async fn headless_plans_are_auto_approved() {
        let e = env(None);
        let finalize = BuiltinTransitionTool::new(BuiltinTransition::FinalizePlan, e.clone());
        assert!(
            !finalize
                .call(json!({"plan": "p"}), &ctx())
                .await
                .unwrap()
                .is_error
        );
        assert!(decision(&e).is_some());
    }

    #[tokio::test]
    async fn the_first_transition_wins() {
        let e = env(None);
        let approve = BuiltinTransitionTool::new(BuiltinTransition::ApproveReview, e.clone());
        let reject = BuiltinTransitionTool::new(BuiltinTransition::RejectReview, e.clone());
        approve
            .call(json!({"summary": "ok"}), &ctx())
            .await
            .unwrap();
        let r = reject.call(json!({"reason": "no"}), &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(
            r.content
                .contains("already transitioned via approve_review")
        );
        assert_eq!(decision(&e).unwrap().outcome, Outcome::Approved);
    }

    #[tokio::test]
    async fn an_empty_argument_is_refused() {
        let e = env(None);
        let t = BuiltinTransitionTool::new(BuiltinTransition::MarkExecuteComplete, e.clone());
        assert!(
            t.call(json!({"summary": "  "}), &ctx())
                .await
                .unwrap()
                .is_error
        );
        assert!(decision(&e).is_none());
    }

    #[tokio::test]
    async fn exit_workflow_ends_the_run() {
        let e = env(None);
        let t = BuiltinTransitionTool::new(BuiltinTransition::ExitWorkflow, e.clone());
        t.call(json!({"reason": "not code"}), &ctx()).await.unwrap();
        let d = decision(&e).unwrap();
        assert_eq!(d.outcome, Outcome::Exit);
        assert_eq!(d.edge, Edge::End);
    }

    fn declared(tool: &str) -> TransitionDefinition {
        TransitionDefinition {
            tool: tool.into(),
            description: "Submit it.".into(),
            params: vec![TransitionParam {
                name: "body".into(),
                ..Default::default()
            }],
            next: Some("build".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_declared_transition_records_its_edge_and_arguments() {
        let e = env(None);
        let t = DeclaredTransitionTool::new(declared("submit_spec"), e.clone());
        assert_eq!(t.schema()["required"], json!(["body"]));
        let r = t.call(json!({"body": "spec"}), &ctx()).await.unwrap();
        assert!(!r.is_error);
        let d = decision(&e).unwrap();
        assert_eq!(d.edge, Edge::Phase("build".into()));
        assert_eq!(d.args, json!({"body": "spec"}));
        assert_eq!(d.text, "spec");
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_refused() {
        let e = env(None);
        let t = DeclaredTransitionTool::new(declared("submit_spec"), e.clone());
        let r = t.call(json!({}), &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("body"));
    }

    #[tokio::test]
    async fn a_user_can_decline_a_declared_transition() {
        let mut def = declared("ship_it");
        def.require_user_approval = true;
        let e = env(Some(Arc::new(AlwaysReject)));
        let t = DeclaredTransitionTool::new(def, e.clone());
        let r = t.call(json!({"body": "x"}), &ctx()).await.unwrap();
        assert!(r.is_error);
        assert!(decision(&e).is_none());
    }

    #[tokio::test]
    async fn an_unmet_guard_blocks_the_transition() {
        let mut def = declared("finish");
        def.guard = Some("goals_complete".into());
        let e = env(None);
        let t = DeclaredTransitionTool::new(def, e.clone());
        let r = t.call(json!({"body": "x"}), &ctx()).await.unwrap();
        assert!(r.is_error, "no goals recorded yet");
        assert!(decision(&e).is_none());
    }

    fn typed(name: &str, kind: &str, required: bool) -> TransitionParam {
        TransitionParam {
            name: name.into(),
            description: format!("{name} value"),
            required,
            kind: Some(kind.into()),
            items: None,
        }
    }

    #[tokio::test]
    async fn typed_parameters_get_typed_schemas_and_coerced_arguments() {
        let def = TransitionDefinition {
            tool: "submit_plan".into(),
            description: "Submit.".into(),
            params: vec![
                TransitionParam {
                    name: "plan".into(),
                    ..Default::default()
                },
                typed("components", "string_list", true),
                typed("auth_required", "boolean", false),
                typed("issues", "object_list", false),
            ],
            next: Some("build".into()),
            ..Default::default()
        };
        let e = env(None);
        let t = DeclaredTransitionTool::new(def, e.clone());
        let schema = t.schema();
        assert_eq!(schema["properties"]["components"]["type"], "array");
        assert_eq!(
            schema["properties"]["components"]["items"]["type"],
            "string"
        );
        assert_eq!(schema["properties"]["auth_required"]["type"], "boolean");
        assert_eq!(schema["properties"]["issues"]["items"]["type"], "object");
        assert_eq!(schema["required"], json!(["plan", "components"]));

        // An empty required list is missing.
        let r = t
            .call(json!({"plan": "p", "components": []}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(r.content.contains("components"), "{}", r.content);

        // A JSON-encoded array string is accepted, items trimmed.
        let r = t
            .call(
                json!({
                    "plan": "p",
                    "components": "[\" Header | sticky \", \"\"]",
                    "auth_required": "true",
                    "issues": [{"page": "/", "issue": "x"}, 3]
                }),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let d = decision(&e).unwrap();
        assert_eq!(d.args["components"], json!(["Header | sticky"]));
        assert_eq!(d.args["auth_required"], json!(true));
        assert_eq!(d.args["issues"], json!([{"page": "/", "issue": "x"}]));
        assert!(
            d.text.contains("components:\n- Header | sticky"),
            "{}",
            d.text
        );
    }

    #[tokio::test]
    async fn an_absent_optional_boolean_is_left_out_of_the_arguments() {
        let def = TransitionDefinition {
            tool: "submit".into(),
            description: "d".into(),
            params: vec![typed("flag", "boolean", false)],
            ..Default::default()
        };
        let e = env(None);
        DeclaredTransitionTool::new(def, e.clone())
            .call(json!({}), &ctx())
            .await
            .unwrap();
        assert_eq!(decision(&e).unwrap().args, json!({}));
    }

    #[tokio::test]
    async fn typed_parameters_are_advertised_and_normalised() {
        let def = TransitionDefinition {
            tool: "verify_goal".into(),
            description: "Verdict.".into(),
            params: vec![
                TransitionParam {
                    name: "satisfied".into(),
                    kind: Some("boolean".into()),
                    ..Default::default()
                },
                TransitionParam {
                    name: "evidence".into(),
                    ..Default::default()
                },
                TransitionParam {
                    name: "files".into(),
                    kind: Some("array".into()),
                    required: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let t = DeclaredTransitionTool::new(def.clone(), env(None));
        let schema = t.schema();
        assert_eq!(schema["properties"]["satisfied"]["type"], json!("boolean"));
        assert_eq!(
            schema["properties"]["files"]["items"]["type"],
            json!("string")
        );
        assert_eq!(schema["required"], json!(["satisfied", "evidence"]));

        // A flag sent as text and a list sent as JSON text are accepted.
        let e = env(None);
        let r = DeclaredTransitionTool::new(def.clone(), e.clone())
            .call(
                json!({"satisfied": "false", "evidence": "tests fail", "files": "[\"a.rs\"]"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!r.is_error, "{}", r.content);
        let d = decision(&e).unwrap();
        assert_eq!(
            d.args,
            json!({"satisfied": false, "evidence": "tests fail", "files": ["a.rs"]})
        );
        assert_eq!(
            d.text,
            "satisfied: false\nevidence: tests fail\nfiles:\n- a.rs"
        );

        // A wrong type is refused, and nothing is decided.
        let e = env(None);
        let r = DeclaredTransitionTool::new(def.clone(), e.clone())
            .call(json!({"satisfied": "maybe", "evidence": "x"}), &ctx())
            .await
            .unwrap();
        assert!(r.is_error);
        assert!(
            r.content.contains("satisfied: must be true or false"),
            "{}",
            r.content
        );
        assert!(decision(&e).is_none());

        // An omitted required flag is missing.
        let e = env(None);
        let r = DeclaredTransitionTool::new(def, e.clone())
            .call(json!({"evidence": "x"}), &ctx())
            .await
            .unwrap();
        assert!(
            r.content
                .contains("missing required argument(s): satisfied")
        );
    }

    #[tokio::test]
    async fn a_group_transition_is_refused_once_the_phase_has_decided() {
        let e = env(None);
        let first = PhaseDecision {
            tool: "dispatch_parallel_workers".into(),
            outcome: Outcome::Approved,
            edge: Edge::Default,
            text: "dispatched".into(),
            args: json!({}),
        };
        assert!(e.decide_group_transition(first.clone()));
        assert!(!e.decide_group_transition(PhaseDecision {
            tool: "other".into(),
            ..first
        }));
        assert_eq!(
            e.already_decided().as_deref(),
            Some("dispatch_parallel_workers")
        );
    }

    #[tokio::test]
    async fn rejected_and_exit_outcomes_map_to_edges() {
        let mut rej = declared("bounce");
        rej.outcome = Some("rejected".into());
        rej.next = None;
        let e = env(None);
        DeclaredTransitionTool::new(rej, e.clone())
            .call(json!({"body": "x"}), &ctx())
            .await
            .unwrap();
        let d = decision(&e).unwrap();
        assert_eq!((d.outcome, d.edge), (Outcome::Rejected, Edge::Default));

        let mut exit = declared("quit");
        exit.outcome = Some("exit".into());
        let e = env(None);
        DeclaredTransitionTool::new(exit, e.clone())
            .call(json!({"body": "x"}), &ctx())
            .await
            .unwrap();
        assert_eq!(decision(&e).unwrap().edge, Edge::End);
    }
}
