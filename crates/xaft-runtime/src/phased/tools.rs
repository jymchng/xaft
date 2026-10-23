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
//!   name, parameters, edge, guard and effect come from the definition.
//!
//! The first accepted transition wins; any later call in the same phase run
//! is refused, so an agent cannot transition twice.

use std::sync::{Arc, Mutex};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::signals::{SignalBus, ToolPendingApproval};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};

use xaft_config::types::TransitionDefinition;

use super::groups::{RunData, check_guard, run_effect};

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
    fn already_decided(&self) -> Option<String> {
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
}

const STOP_NOTE: &str = "Phase transition accepted. Stop now — do not call any more tools; the workflow runner takes over.";

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
            properties.insert(
                p.name.clone(),
                json!({ "type": "string", "description": p.description }),
            );
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
        for p in &self.def.params {
            let value = string_arg(&input, &p.name);
            if p.required && value.trim().is_empty() {
                missing.push(p.name.clone());
            }
            args.insert(p.name.clone(), Value::String(value));
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
        let args = Value::Object(args);

        if let Some(guard) = &self.def.guard {
            if let Err(unmet) = check_guard(guard, &self.env.data, &self.env.control) {
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
        if let Some(effect) = &self.def.effect {
            match run_effect(effect, &self.env.data, &args) {
                Ok(note) => effect_note = note,
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

        let outcome = match self.def.outcome_or_default() {
            "rejected" => Outcome::Rejected,
            "exit" => Outcome::Exit,
            _ => Outcome::Approved,
        };
        let edge = if self.def.end || outcome == Outcome::Exit {
            Edge::End
        } else if let Some(next) = &self.def.next {
            Edge::Phase(next.clone())
        } else {
            Edge::Default
        };
        let text = summarize_args(&args);
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

/// Render transition arguments as the phase's output text.
fn summarize_args(args: &Value) -> String {
    match args {
        Value::Object(map) if map.len() == 1 => map
            .values()
            .next()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
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
