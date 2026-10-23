//! The phased workflow engine — xaft's port of agenthicc's generic
//! `WorkflowRunner` (`agenthicc/workflows/default/runner.py`).
//!
//! A phased workflow is a declarative graph of phases
//! ([`xaft_config::types::PhaseDefinition`]). The engine runs it like this:
//!
//! ```text
//! phase = first phase (or the checkpointed resume point)
//! loop:
//!   enforce max_iterations / max_total_phase_runs
//!   checkpoint "resume here = phase"
//!   human phase    → ask the user to approve (headless: auto-approve)
//!   parallel group → run the phase and its parallel_with peers concurrently
//!   agent phase    → one agent turn; when the phase requires a transition
//!                    tool, continuation turns until one is called
//!   apply the command gate (require_successful_commands / require_readiness)
//!   choose the edge: transition target → on_reject (rejected) → next
//!   checkpoint "resume here = edge"
//! ```
//!
//! Every phase of a run shares one conversation (the same agent key in the
//! same conversation id), so a later phase sees everything an earlier one
//! did — agenthicc's shared `ShortTermMemory`. Parallel peers get their own
//! keys so they never race on one history.
//!
//! Phases change **only** through transition tools ([`tools`]); prose never
//! advances a workflow. The run's durable state ([`state::WorkflowRunState`])
//! is checkpointed after every transition, so a failed, cancelled or crashed
//! run resumes at the exact edge it had chosen.

pub mod groups;
pub mod prompt;
pub mod question;
pub mod script;
pub mod state;
pub mod tools;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agtrs_runtime::agent::Agent;
use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::{HandoffAgentStore, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::transport::ContentBlock;
use serde_json::{Value, json};
use tracing::{info, warn};

use xaft_agent::signals::{
    XaftWorkflowPhaseCompleted, XaftWorkflowPhaseStarted, XaftWorkflowRunCompleted,
    XaftWorkflowRunStarted,
};
use xaft_agents::named::NamedAgent;
use xaft_config::XaftConfig;
use xaft_config::types::{PhaseDefinition, WorkflowDefinition};
use xaft_tools::capabilities::{ToolCapability, capability_of};

use crate::dispatch::ModeToolFilter;
use crate::error::RuntimeError;

use self::groups::{
    AuthoringContext, RunData, artifact_tools, ask_user_tool, authoring_tools, goal_list_tools,
    workflow_state_tools,
};
use self::prompt::{
    CLARIFICATION_REMINDER, ContinuationKind, NON_TRANSITION_MUTATIONS, TemplateContext,
    default_continuation, mode_policy, parse_output_schema, render_template,
    role_default_capabilities, role_prompt, transition_instruction,
};
use self::question::UserQuestionGate;
use self::state::{
    CheckpointStore, PhaseOutput, PhaseRunRecord, RunStatus, WorkflowRunState,
    definition_fingerprint, snippet,
};
use self::tools::{
    BuiltinTransition, BuiltinTransitionTool, DeclaredTransitionTool, Edge, Outcome, PhaseControl,
    TransitionEnv,
};

/// Registry key every sequential phase runs under, so they share history.
const AGENT_KEY: &str = "workflow";

/// Continuation turns a phase gets when its `max_iterations` is unlimited —
/// agenthicc's `max_cont = 10` default.
const DEFAULT_CONTINUATIONS: i64 = 10;

/// Times a phase with unlimited `max_iterations` may be entered before the
/// run fails. agenthicc has no such cap and can loop forever when a review
/// never decides; this keeps an unattended run bounded. The run stays
/// resumable.
pub const SAFETY_MAX_PHASE_ENTRIES: u32 = 25;

/// Metadata key: the phase to run next, overriding the graph.
const META_NEXT: &str = "next_phase";
/// Metadata key: end the run after this phase.
const META_END: &str = "end_run";
/// Metadata key: the run exits early after this phase.
const META_EXIT: &str = "exit";

/// A workflow to run: its name and definition.
#[derive(Debug, Clone)]
pub struct PhasedWorkflow {
    /// Registry name.
    pub name: String,
    /// The phase graph and settings.
    pub definition: WorkflowDefinition,
}

/// Builds a provider for a model id, for per-phase model overrides.
pub type LlmForModel =
    Arc<dyn Fn(&str) -> Result<Arc<dyn LlmProvider>, RuntimeError> + Send + Sync>;

/// Everything the engine needs from its caller.
#[derive(Clone)]
pub struct PhasedDeps {
    /// The run's default provider.
    pub llm: Arc<dyn LlmProvider>,
    /// Provider factory for per-phase models; `None` uses `llm` everywhere.
    pub llm_for_model: Option<LlmForModel>,
    /// Signal bus for TUI / observability events.
    pub signals: Arc<SignalBus>,
    /// DI context for tools.
    pub resolve_ctx: Arc<injectable_rs_runtime::ResolveContext>,
    /// Read-only tools, *not* filtered by the session mode.
    pub read_tools: Vec<Arc<ErasedTool>>,
    /// Full tool set, *not* filtered by the session mode.
    pub write_tools: Vec<Arc<ErasedTool>>,
    /// The session mode's tool filter, applied to phases without a
    /// `mode_override`. A phase override replaces the session mode, as in
    /// agenthicc.
    pub session_tool_filter: Option<ModeToolFilter>,
    /// The session mode's approval filter (the TUI's Safe mode): matching
    /// tools need the user's approval before each call. Applied to phases
    /// without a `mode_override`, like `session_tool_filter`.
    pub session_approval_filter: Option<ModeToolFilter>,
    /// The session mode's system-prompt patch, for phases without an override.
    pub session_mode_patch: Option<String>,
    /// Project context every phase must see: AGENTS.md instructions and the
    /// skills section, as loaded by the runtime.
    pub project_context: Vec<String>,
    /// Persistent conversation store; `None` keeps history in memory.
    pub conversation_store: Option<Arc<dyn ConversationStore>>,
    /// Yes/no approvals (plan approval, human phases, gated transitions).
    pub approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// Free-text questions for `ask_user`.
    pub question_gate: Option<Arc<dyn UserQuestionGate>>,
    /// The run's working directory.
    pub working_dir: PathBuf,
    /// The owning session.
    pub session_id: String,
    /// Resolved @-mention blocks for the very first turn.
    pub user_parts: Option<Vec<ContentBlock>>,
    /// The session config (workflow discovery for authoring tools).
    pub config: XaftConfig,
    /// Where checkpoints go; `None` disables checkpointing.
    pub checkpoints: Option<CheckpointStore>,
}

/// The result of a run that did not fail.
#[derive(Debug, Clone)]
pub struct PhasedOutcome {
    /// The run's final message.
    pub content: String,
    /// The final durable state (also the last checkpoint).
    pub state: WorkflowRunState,
    /// Total agent turns across all phases.
    pub turns: usize,
}

/// Run `workflow` for `intent`, or continue `resume` from its checkpoint.
///
/// On failure the run is checkpointed as `failed` (resumable) and the error is
/// returned.
pub async fn run_phased_workflow(
    workflow: &PhasedWorkflow,
    intent: &str,
    deps: PhasedDeps,
    resume: Option<WorkflowRunState>,
) -> Result<PhasedOutcome, RuntimeError> {
    Engine::new(workflow, deps).run(intent, resume).await
}

struct Engine<'a> {
    wf: &'a PhasedWorkflow,
    deps: PhasedDeps,
    conv_store: Arc<dyn ConversationStore>,
    llm_cache: Mutex<HashMap<String, Arc<dyn LlmProvider>>>,
    turns: AtomicUsize,
    user_parts: Mutex<Option<Vec<ContentBlock>>>,
    /// agenthicc's `[WORKFLOW RESUME]` block, prepended to the first turn
    /// after a resume.
    resume_note: Mutex<Option<String>>,
}

impl<'a> Engine<'a> {
    fn new(wf: &'a PhasedWorkflow, deps: PhasedDeps) -> Self {
        let conv_store = deps
            .conversation_store
            .clone()
            .unwrap_or_else(|| Arc::new(InMemoryConversationStore::new()));
        let user_parts = Mutex::new(deps.user_parts.clone());
        Self {
            wf,
            deps,
            conv_store,
            llm_cache: Mutex::new(HashMap::new()),
            turns: AtomicUsize::new(0),
            user_parts,
            resume_note: Mutex::new(None),
        }
    }

    fn def(&self) -> &WorkflowDefinition {
        &self.wf.definition
    }

    fn phase(&self, name: &str) -> Option<(usize, &PhaseDefinition)> {
        self.def()
            .phases
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name)
    }

    fn save(&self, state: &mut WorkflowRunState, data: &RunData) {
        state.data = data.snapshot();
        if let Some(store) = &self.deps.checkpoints {
            if let Err(e) = store.save(state) {
                warn!(run_id = %state.run_id, error = %e, "workflow checkpoint write failed");
            }
        }
    }

    async fn run(
        &self,
        intent: &str,
        resume: Option<WorkflowRunState>,
    ) -> Result<PhasedOutcome, RuntimeError> {
        let fingerprint = definition_fingerprint(self.def());
        let (mut state, resumed) = match resume {
            Some(mut s) => {
                if s.workflow_name != self.wf.name {
                    return Err(RuntimeError::Config(format!(
                        "run {} belongs to workflow '{}', not '{}'",
                        s.run_id, s.workflow_name, self.wf.name
                    )));
                }
                if s.definition_fingerprint != fingerprint {
                    warn!(
                        run_id = %s.run_id,
                        "workflow '{}' changed since this run was checkpointed; resuming with the current definition",
                        self.wf.name
                    );
                    s.definition_fingerprint = fingerprint.clone();
                }
                s.status = RunStatus::Running;
                s.error = None;
                (s, true)
            }
            None => (
                WorkflowRunState::new(
                    uuid::Uuid::new_v4().simple().to_string(),
                    &self.wf.name,
                    intent,
                    &self.deps.session_id,
                    &self.deps.working_dir,
                    fingerprint,
                ),
                false,
            ),
        };
        let data = RunData::new(state.data.clone(), &self.deps.working_dir);

        let start = if resumed {
            self.find_resume_phase(&state)
        } else {
            self.def().phases.first().map(|p| p.name.clone())
        };
        if resumed {
            // agenthicc's headless resume prompt, verbatim in shape. Text the
            // user submitted with the resume is their continuation.
            // A bare "continue" / "resume" (what the TUI asks the user to type
            // to fire an armed resume) carries no guidance.
            let continuation = intent.trim();
            let is_bare = ["continue", "resume", "go"]
                .iter()
                .any(|w| continuation.eq_ignore_ascii_case(w));
            let continuation =
                (!continuation.is_empty() && !is_bare && continuation != state.intent.trim())
                    .then(|| continuation.to_string());
            if let Some(c) = &continuation {
                let mut list = match data.get("continuations") {
                    Some(Value::Array(a)) => a,
                    _ => Vec::new(),
                };
                list.push(json!(c));
                data.set("continuations", Value::Array(list));
            }
            if let Some(phase) = &start {
                let note = format!(
                    "[WORKFLOW RESUME]\nWorkflow: {}\nCurrent phase: {phase}\nThe prior agent \
                     turn was interrupted safely; preserve completed work and continue from the \
                     saved phase.\nOriginal intent: {}\n{}",
                    self.wf.name,
                    state.intent,
                    continuation
                        .map(|c| format!("User continuation: {c}"))
                        .unwrap_or_else(|| "Continue.".to_string())
                );
                if let Ok(mut slot) = self.resume_note.lock() {
                    *slot = Some(note);
                }
            }
            if let Some(start) = &start {
                // An entry that never completed (the run died or failed inside
                // the phase) must not count twice: otherwise resuming a phase
                // with max_iterations = 1 could never succeed.
                let completed = state
                    .phase_history
                    .iter()
                    .filter(|r| &r.phase_name == start)
                    .count() as u32;
                if let Some(entries) = state.phase_iterations.get_mut(start) {
                    if *entries > completed {
                        *entries = completed;
                    }
                }
            }
        }

        info!(
            run_id = %state.run_id,
            workflow = %self.wf.name,
            resumed,
            start = ?start,
            "xaft: phased workflow starting"
        );
        self.deps
            .signals
            .emit(XaftWorkflowRunStarted {
                run_id: state.run_id.clone(),
                workflow_name: self.wf.name.clone(),
                intent: state.intent.clone(),
                phase_names: self.def().phases.iter().map(|p| p.name.clone()).collect(),
                resumed,
            })
            .await;
        self.save(&mut state, &data);

        let result = match start {
            Some(start) => self.phase_loop(&mut state, &data, start).await,
            None => Ok(RunStatus::Complete),
        };

        match result {
            Ok(status) => {
                state.status = status;
                state.next_phase = None;
                if state.final_text.is_none() {
                    state.final_text = state.last_output().map(|o| o.full_text.clone());
                }
                self.save(&mut state, &data);
                self.emit_run_completed(&state).await;
                Ok(PhasedOutcome {
                    content: state.final_text.clone().unwrap_or_default(),
                    state,
                    turns: self.turns.load(Ordering::Relaxed),
                })
            }
            Err(err) => {
                state.status = match &err {
                    RuntimeError::Cancelled(_) => RunStatus::Cancelled,
                    _ => RunStatus::Failed,
                };
                state.error = Some(err.to_string());
                self.save(&mut state, &data);
                self.emit_run_completed(&state).await;
                Err(err)
            }
        }
    }

    async fn emit_run_completed(&self, state: &WorkflowRunState) {
        self.deps
            .signals
            .emit(XaftWorkflowRunCompleted {
                run_id: state.run_id.clone(),
                workflow_name: self.wf.name.clone(),
                status: state.status.label().to_string(),
                phases_run: state.phase_history.len(),
                error: state.error.clone(),
            })
            .await;
    }

    /// Where a resumed run continues.
    ///
    /// `next_phase` is written as "resume here" when a phase is entered and
    /// replaced by the chosen edge when it completes, so it is always the
    /// right answer for a run that stopped mid-graph. A run that already
    /// reached its end has nothing left to do.
    fn find_resume_phase(&self, state: &WorkflowRunState) -> Option<String> {
        if state.status.is_terminal() {
            return None;
        }
        if let Some(next) = &state.next_phase {
            if self.phase(next).is_some() {
                return Some(next.clone());
            }
        }
        if state.phase_history.is_empty() {
            return self.def().phases.first().map(|p| p.name.clone());
        }
        None
    }

    async fn phase_loop(
        &self,
        state: &mut WorkflowRunState,
        data: &RunData,
        start: String,
    ) -> Result<RunStatus, RuntimeError> {
        let total_phases = self.def().phases.len();
        let mut processed_parallel: HashSet<String> = HashSet::new();
        let mut current = Some(start);

        while let Some(name) = current.clone() {
            let Some((index, spec)) = self.phase(&name) else {
                return Err(RuntimeError::Agent(format!(
                    "workflow '{}': unknown phase '{name}'",
                    self.wf.name
                )));
            };
            if processed_parallel.contains(&name) {
                current = spec.next.clone();
                continue;
            }

            // ── Iteration limits ──
            let count = state.phase_iterations.get(&name).copied().unwrap_or(0);
            let max_iterations = spec.max_iterations_or_unlimited();
            if max_iterations != -1 && i64::from(count) >= max_iterations {
                return Err(RuntimeError::Agent(format!(
                    "workflow '{}': phase '{name}' exceeded max_iterations={max_iterations}",
                    self.wf.name
                )));
            }
            if max_iterations == -1 && count >= SAFETY_MAX_PHASE_ENTRIES {
                return Err(RuntimeError::Agent(format!(
                    "workflow '{}': phase '{name}' was entered {count} times without converging \
                     (safety cap {SAFETY_MAX_PHASE_ENTRIES}; set max_iterations to choose a bound)",
                    self.wf.name
                )));
            }
            let max_total = self.def().max_total_phase_runs.unwrap_or(0);
            let total_runs: u32 = state.phase_iterations.values().sum();
            if max_total > 0 && total_runs >= max_total {
                return Err(RuntimeError::Agent(format!(
                    "workflow '{}' stopped after {total_runs} phase runs (limit: {max_total})",
                    self.wf.name
                )));
            }
            state.phase_iterations.insert(name.clone(), count + 1);
            state.current_phase = Some(name.clone());
            state.current_phase_index = index;
            state.next_phase = Some(name.clone());
            self.save(state, data);

            // ── Parallel group ──
            if !spec.parallel_with.is_empty() {
                let mut group: Vec<&PhaseDefinition> = vec![spec];
                for peer in &spec.parallel_with {
                    if let Some((_, p)) = self.phase(peer) {
                        group.push(p);
                    }
                }
                for p in &group {
                    self.emit_phase_started(state, p).await;
                }
                let snapshot = state.clone();
                let outputs = futures::future::join_all(
                    group
                        .iter()
                        .map(|p| self.run_phase_or_route_error(p, &snapshot, data, true)),
                )
                .await;
                let mut gate_error = None;
                for (p, out) in group.iter().zip(outputs) {
                    let out = self.apply_command_gate(p, out?, &mut gate_error);
                    if p.name != name {
                        *state.phase_iterations.entry(p.name.clone()).or_insert(0) += 1;
                    }
                    self.record(state, p, &out);
                    self.emit_phase_completed(state, p, &out, spec.next.clone())
                        .await;
                    processed_parallel.insert(p.name.clone());
                }
                current = spec.next.clone();
                state.next_phase = current.clone();
                self.save(state, data);
                if let Some(err) = gate_error {
                    return Err(RuntimeError::Agent(err));
                }
                continue;
            }

            // ── Single phase ──
            self.emit_phase_started(state, spec).await;
            let snapshot = state.clone();
            let output = self
                .run_phase_or_route_error(spec, &snapshot, data, false)
                .await?;
            let mut gate_error = None;
            let output = self.apply_command_gate(spec, output, &mut gate_error);
            self.record(state, spec, &output);

            let exit = output.metadata.get(META_EXIT) == Some(&json!(true));
            let next = if exit || gate_error.is_some() {
                None
            } else {
                determine_transition(spec, &output)
            };
            self.emit_phase_completed(state, spec, &output, next.clone())
                .await;
            state.next_phase = next.clone();
            self.save(state, data);

            if let Some(err) = gate_error {
                // The gate failed the phase; resume re-enters it.
                state.next_phase = Some(name.clone());
                return Err(RuntimeError::Agent(format!(
                    "workflow '{}': phase '{name}': {err}",
                    self.wf.name
                )));
            }
            if exit {
                state.final_text = Some(output.full_text.clone());
                return Ok(RunStatus::Exited);
            }
            current = next;
        }
        Ok(RunStatus::Complete)
    }

    /// Run a phase; an error is routed to `on_error` when the phase has one.
    async fn run_phase_or_route_error(
        &self,
        spec: &PhaseDefinition,
        state: &WorkflowRunState,
        data: &RunData,
        parallel: bool,
    ) -> Result<PhaseOutput, RuntimeError> {
        let t0 = Instant::now();
        match self.run_phase(spec, state, data, parallel).await {
            Ok(out) => Ok(out),
            Err(RuntimeError::Cancelled(reason)) => Err(RuntimeError::Cancelled(reason)),
            Err(err) => match &spec.on_error {
                Some(target) => {
                    warn!(phase = %spec.name, error = %err, on_error = %target, "phase failed; routing to on_error");
                    let mut metadata = BTreeMap::new();
                    metadata.insert("error".to_string(), json!(err.to_string()));
                    metadata.insert(META_NEXT.to_string(), json!(target));
                    Ok(PhaseOutput {
                        phase_name: spec.name.clone(),
                        role: spec.role().to_string(),
                        full_text: format!("Phase '{}' failed: {err}", spec.name),
                        approved: Some(false),
                        metadata,
                        duration_s: t0.elapsed().as_secs_f64(),
                        ..Default::default()
                    })
                }
                None => Err(err),
            },
        }
    }

    fn record(&self, state: &mut WorkflowRunState, spec: &PhaseDefinition, out: &PhaseOutput) {
        state.phase_history.push(PhaseRunRecord {
            phase_name: spec.name.clone(),
            role: spec.role().to_string(),
            approved: out.approved,
            output_summary: snippet(&out.full_text, 200),
            iteration: state.phase_iterations.get(&spec.name).copied().unwrap_or(1),
            duration_s: out.duration_s,
        });
        state.add_output(out.clone());
    }

    fn model_for(&self, spec: &PhaseDefinition) -> Option<String> {
        self.def()
            .model_for_phase(spec)
            .or_else(|| self.def().model.clone().filter(|m| !m.trim().is_empty()))
    }

    fn llm_for_phase(&self, spec: &PhaseDefinition) -> Result<Arc<dyn LlmProvider>, RuntimeError> {
        let (Some(model), Some(factory)) = (self.model_for(spec), &self.deps.llm_for_model) else {
            return Ok(Arc::clone(&self.deps.llm));
        };
        if let Some(cached) = self
            .llm_cache
            .lock()
            .ok()
            .and_then(|c| c.get(&model).cloned())
        {
            return Ok(cached);
        }
        let llm = factory(&model)?;
        if let Ok(mut cache) = self.llm_cache.lock() {
            cache.insert(model, Arc::clone(&llm));
        }
        Ok(llm)
    }

    async fn emit_phase_started(&self, state: &WorkflowRunState, spec: &PhaseDefinition) {
        let index = self.phase(&spec.name).map(|(i, _)| i).unwrap_or(0);
        self.deps
            .signals
            .emit(XaftWorkflowPhaseStarted {
                run_id: state.run_id.clone(),
                workflow_name: self.wf.name.clone(),
                phase_name: spec.name.clone(),
                phase_index: index,
                total_phases: self.def().phases.len(),
                iteration: state.phase_iterations.get(&spec.name).copied().unwrap_or(1),
                model: self.model_for(spec).unwrap_or_default(),
            })
            .await;
    }

    async fn emit_phase_completed(
        &self,
        state: &WorkflowRunState,
        spec: &PhaseDefinition,
        out: &PhaseOutput,
        next: Option<String>,
    ) {
        self.deps
            .signals
            .emit(XaftWorkflowPhaseCompleted {
                run_id: state.run_id.clone(),
                workflow_name: self.wf.name.clone(),
                phase_name: spec.name.clone(),
                role: spec.role().to_string(),
                approved: out.approved,
                transition: out.transition.clone(),
                next_phase: next,
                summary: snippet(&out.full_text, 200),
                duration_s: out.duration_s,
            })
            .await;
    }

    /// agenthicc's `_command_gate_error`, folded into the output.
    fn apply_command_gate(
        &self,
        spec: &PhaseDefinition,
        mut out: PhaseOutput,
        gate_error: &mut Option<String>,
    ) -> PhaseOutput {
        if !(spec.require_successful_commands || spec.require_readiness) {
            return out;
        }
        let outcomes: Vec<CommandOutcome> = out
            .metadata
            .get("command_outcomes")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        // The latest outcome per command line decides: a command that failed
        // and was re-run successfully does not fail the gate.
        let mut latest: BTreeMap<String, &CommandOutcome> = BTreeMap::new();
        for o in &outcomes {
            latest.insert(o.key.clone(), o);
        }
        let mut error = None;
        if spec.require_successful_commands {
            if let Some(failed) = latest.values().filter(|o| !o.ok).last() {
                error = Some(format!(
                    "required command did not succeed ({}): {}",
                    failed.tool,
                    snippet(&failed.summary, 400)
                ));
            }
        }
        if error.is_none() && spec.require_readiness && !latest.values().any(|o| o.ready) {
            error = Some("required service readiness was not established".to_string());
        }
        if let Some(err) = error {
            out.approved = Some(false);
            out.metadata
                .insert("command_gate_error".to_string(), json!(err.clone()));
            *gate_error = Some(err);
        }
        out
    }

    // ── One phase ─────────────────────────────────────────────────────────

    async fn run_phase(
        &self,
        spec: &PhaseDefinition,
        state: &WorkflowRunState,
        data: &RunData,
        parallel: bool,
    ) -> Result<PhaseOutput, RuntimeError> {
        if spec.is_human() {
            return Ok(self.run_human_phase(spec, state, data).await);
        }
        if spec.is_script() {
            return self.run_script_phase(spec, state, data).await;
        }
        let t0 = Instant::now();
        let iteration = state.phase_iterations.get(&spec.name).copied().unwrap_or(1);

        // ── Tools ──
        let control = Arc::new(Mutex::new(PhaseControl::default()));
        let env = TransitionEnv {
            control: Arc::clone(&control),
            data: data.clone(),
            approval_gate: self.deps.approval_gate.clone(),
            signals: Some(Arc::clone(&self.deps.signals)),
        };
        let recorder = CommandRecorder::default();
        let record_commands = spec.require_successful_commands || spec.require_readiness;

        let mut tools: Vec<Arc<ErasedTool>> = self
            .base_tools(spec)
            .into_iter()
            .map(|t| {
                if record_commands && capability_of(t.name()) == ToolCapability::Execute {
                    recorder.wrap(t)
                } else {
                    t
                }
            })
            .collect();

        for group in &spec.extra_tools {
            match group.as_str() {
                "goal_list" => tools.extend(goal_list_tools(data)),
                "workflow_state" => tools.extend(workflow_state_tools(data)),
                "artifacts" => tools.extend(artifact_tools(data)),
                "workflow_authoring" => {
                    let ctx = AuthoringContext {
                        working_dir: self.deps.working_dir.clone(),
                        config: self.deps.config.clone(),
                        available_tools: self.available_tools(),
                    };
                    tools.extend(authoring_tools(&ctx, data));
                }
                // `ask_user` is added to every agent phase below.
                _ => {}
            }
        }
        // agenthicc gives every generic phase the question tool.
        tools.push(ask_user_tool(self.deps.question_gate.clone()));

        let mut transition_names: Vec<String> = Vec::new();
        let kind = if spec.require_plan_finalization {
            Some(ContinuationKind::Plan)
        } else if spec.require_explicit_completion {
            Some(ContinuationKind::Completion)
        } else if spec.require_explicit_review {
            Some(ContinuationKind::Review)
        } else {
            None
        };
        let builtins: &[BuiltinTransition] = match kind {
            Some(ContinuationKind::Plan) => &[
                BuiltinTransition::RequestPlanApproval,
                BuiltinTransition::FinalizePlan,
                BuiltinTransition::ExitWorkflow,
            ],
            Some(ContinuationKind::Completion) => &[BuiltinTransition::MarkExecuteComplete],
            Some(ContinuationKind::Review) => &[
                BuiltinTransition::ApproveReview,
                BuiltinTransition::RejectReview,
            ],
            _ => &[],
        };
        for b in builtins {
            tools.push(BuiltinTransitionTool::erased(*b, env.clone()));
            transition_names.push(b.name().to_string());
        }
        for t in &spec.transitions {
            tools.push(DeclaredTransitionTool::erased(t.clone(), env.clone()));
            transition_names.push(t.tool.clone());
        }
        let kind =
            kind.or_else(|| (!spec.transitions.is_empty()).then_some(ContinuationKind::Declared));
        dedupe_by_name(&mut tools);
        let mutations: Vec<String> = NON_TRANSITION_MUTATIONS
            .iter()
            .filter(|m| tools.iter().any(|t| t.name() == **m))
            .map(|m| (*m).to_string())
            .collect();

        // ── Prompts ──
        let tctx = TemplateContext {
            state,
            data,
            phase: &spec.name,
            iteration,
        };
        let system_prompt = self.system_prompt(spec, &transition_names, &mutations, &tctx);
        let phase_text = if spec.prompt.trim().is_empty() {
            format!("{}\n\nTask: {}", state.context_block(), state.intent)
        } else {
            render_template(&spec.prompt, &tctx)
        };
        let continuation = kind.map(|k| {
            if spec.continuation_prompt.trim().is_empty() {
                default_continuation(k, &transition_names)
            } else {
                render_template(&spec.continuation_prompt, &tctx)
            }
        });

        // ── Agent ──
        let llm = self.llm_for_phase(spec)?;
        let agent_key = if parallel {
            format!("{AGENT_KEY}::{}", spec.name)
        } else {
            AGENT_KEY.to_string()
        };
        let agent = Arc::new(
            NamedAgent::new(
                spec.name.as_str(),
                system_prompt,
                spec.max_turns_or_default() as usize,
            )
            .with_tools(tools)
            .with_signals(Arc::clone(&self.deps.signals)),
        );
        let read_before_edit = Arc::new(xaft_tools::ReadBeforeEditHook::new())
            as Arc<dyn agtrs_runtime::tool_hooks::ToolHook>;
        let orchestrator = HandoffOrchestrator::builder()
            .agent(agent_key.as_str(), agent as Arc<dyn Agent>)
            .conv_store(Arc::clone(&self.conv_store))
            .agent_store(Arc::new(HandoffAgentStore::new()))
            .max_handoffs(1)
            .llm(llm)
            .resolve_ctx(Arc::clone(&self.deps.resolve_ctx))
            .with_approval_gate_opt(self.deps.approval_gate.clone())
            .with_global_tool_hook(read_before_edit)
            .build();

        // ── Turns ──
        let max_attempts = match kind {
            Some(_) => {
                let n = spec.max_iterations_or_unlimited();
                if n > 0 { n } else { DEFAULT_CONTINUATIONS }
            }
            None => 1,
        };
        let mut texts: Vec<String> = Vec::new();
        for attempt in 1..=max_attempts {
            let mut message = if attempt == 1 {
                phase_text.clone()
            } else {
                continuation.clone().unwrap_or_default()
            };
            if attempt == 1 && !parallel {
                if let Some(note) = self.resume_note.lock().ok().and_then(|mut n| n.take()) {
                    message = format!("{note}\n\n{message}");
                }
            }
            let user_parts = if attempt == 1 && !parallel {
                self.user_parts.lock().ok().and_then(|mut p| p.take())
            } else {
                None
            };
            let mut context_state = std::collections::HashMap::new();
            context_state.insert(
                "conversation_id".to_string(),
                json!(state.conversation_id.clone()),
            );
            let params = HandoffRunParams {
                message,
                user_parts,
                conversation_id: state.conversation_id.clone(),
                initial_agent: agent_key.clone(),
                context_state,
                #[cfg(feature = "axum")]
                extensions: Default::default(),
                signals: Some(Arc::clone(&self.deps.signals)),
                max_handoffs_override: None,
                dynamic_registry: None,
            };
            match orchestrator.run(params).await {
                Ok(result) => {
                    self.turns.fetch_add(result.turns, Ordering::Relaxed);
                    if !result.content.trim().is_empty() {
                        texts.push(result.content);
                    }
                }
                Err(AgtrsError::MaxTurnsExceeded) => {
                    // The turn ran out of sub-turns. Not a failure by itself:
                    // the continuation loop re-prompts, as agenthicc's does.
                    warn!(phase = %spec.name, attempt, "phase turn hit max_turns");
                }
                Err(AgtrsError::Cancelled { reason }) => {
                    return Err(RuntimeError::Cancelled(reason));
                }
                Err(e) => {
                    return Err(RuntimeError::Agent(format!("phase '{}': {e}", spec.name)));
                }
            }
            let decided = control
                .lock()
                .map(|c| c.decision.is_some())
                .unwrap_or(false);
            if decided || kind.is_none() {
                break;
            }
        }

        // ── Output ──
        let collected = texts.join("\n\n");
        let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
        let outcomes = recorder.outcomes();
        if !outcomes.is_empty() {
            metadata.insert("command_outcomes".to_string(), json!(outcomes));
        }
        let decision = control.lock().ok().and_then(|c| c.decision.clone());
        let duration_s = t0.elapsed().as_secs_f64();
        let base = PhaseOutput {
            phase_name: spec.name.clone(),
            role: spec.role().to_string(),
            duration_s,
            ..Default::default()
        };

        if let Some(d) = decision {
            match &d.edge {
                Edge::Phase(p) => {
                    metadata.insert(META_NEXT.to_string(), json!(p));
                }
                Edge::End => {
                    metadata.insert(META_END.to_string(), json!(true));
                }
                Edge::Default => {}
            }
            let mut full_text = if d.text.trim().is_empty() {
                collected.clone()
            } else {
                d.text.clone()
            };
            if d.outcome == Outcome::Exit {
                metadata.insert(META_EXIT.to_string(), json!(true));
                if !collected.trim().is_empty() {
                    full_text = collected.clone();
                }
            }
            return Ok(PhaseOutput {
                full_text,
                structured: Some(d.args.clone()),
                approved: Some(d.outcome != Outcome::Rejected),
                transition: Some(d.tool.clone()),
                metadata,
                ..base
            });
        }

        match kind {
            Some(ContinuationKind::Declared) => Err(RuntimeError::Agent(format!(
                "phase '{}' ended without calling any of its transition tools ({}) after {} turn(s)",
                spec.name,
                transition_names.join(", "),
                max_attempts
            ))),
            Some(ContinuationKind::Review) => {
                // No verdict: retry the review phase itself, not the phase
                // before it (agenthicc's `__next_phase__ = spec.name`).
                metadata.insert(META_NEXT.to_string(), json!(spec.name));
                Ok(PhaseOutput {
                    full_text: collected,
                    approved: Some(false),
                    metadata,
                    ..base
                })
            }
            Some(_) => Ok(PhaseOutput {
                full_text: collected,
                approved: Some(false),
                metadata,
                ..base
            }),
            None => {
                let structured = parse_output_schema(&collected, spec.output_schema.as_deref());
                let incomplete = structured
                    .as_ref()
                    .and_then(|s| s.get("incomplete"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if incomplete {
                    metadata.insert(META_NEXT.to_string(), json!(spec.name));
                }
                let approved = if incomplete {
                    Some(false)
                } else {
                    structured
                        .as_ref()
                        .and_then(|s| s.get("approved"))
                        .and_then(Value::as_bool)
                };
                Ok(PhaseOutput {
                    full_text: collected,
                    structured,
                    approved,
                    metadata,
                    ..base
                })
            }
        }
    }

    /// agenthicc's `_run_human_phase`.
    async fn run_human_phase(
        &self,
        spec: &PhaseDefinition,
        state: &WorkflowRunState,
        data: &RunData,
    ) -> PhaseOutput {
        let t0 = Instant::now();
        let base = PhaseOutput {
            phase_name: spec.name.clone(),
            role: "human".to_string(),
            ..Default::default()
        };
        let Some(gate) = &self.deps.approval_gate else {
            warn!(phase = %spec.name, "human phase in headless mode — auto-approving");
            return PhaseOutput {
                full_text: "[auto-approved: no approval service]".to_string(),
                approved: Some(true),
                duration_s: t0.elapsed().as_secs_f64(),
                ..base
            };
        };
        let iteration = state.phase_iterations.get(&spec.name).copied().unwrap_or(1);
        let question = if spec.prompt.trim().is_empty() {
            format!(
                "Approve the work so far and continue the '{}' workflow?",
                self.wf.name
            )
        } else {
            render_template(
                &spec.prompt,
                &TemplateContext {
                    state,
                    data,
                    phase: &spec.name,
                    iteration,
                },
            )
        };
        let prior = state
            .last_output()
            .map(|o| snippet(&o.full_text, 2000))
            .unwrap_or_default();
        let input = json!({ "phase": spec.name, "question": question, "plan": prior });
        let approved = tools::ask_user_approval(
            Some(gate),
            Some(&self.deps.signals),
            "human_review",
            &uuid::Uuid::new_v4().to_string(),
            &input,
        )
        .await;
        let mut text = if approved {
            "Approved by the user.".to_string()
        } else {
            "Rejected by the user.".to_string()
        };
        if !approved {
            if let Some(q) = &self.deps.question_gate {
                if let Some(answers) = q.ask(&["What should change?".to_string()]).await {
                    if let Some(a) = answers.first().filter(|a| !a.trim().is_empty()) {
                        text = format!("Rejected by the user: {a}");
                    }
                }
            }
        }
        PhaseOutput {
            full_text: text,
            approved: Some(approved),
            duration_s: t0.elapsed().as_secs_f64(),
            ..base
        }
    }

    /// The unfiltered tool pool: the full set plus any read-only tool it lacks.
    fn tool_pool(&self) -> Vec<Arc<ErasedTool>> {
        let mut pool = self.deps.write_tools.clone();
        for t in &self.deps.read_tools {
            if !pool.iter().any(|p| p.name() == t.name()) {
                pool.push(Arc::clone(t));
            }
        }
        pool
    }

    fn available_tools(&self) -> Vec<(String, String)> {
        self.tool_pool()
            .iter()
            .map(|t| (t.name().to_string(), t.description().to_string()))
            .collect()
    }

    /// agenthicc's `_filter_tools`: mode policy, then capabilities, then the
    /// explicit allowlist.
    fn base_tools(&self, spec: &PhaseDefinition) -> Vec<Arc<ErasedTool>> {
        let policy = mode_policy(spec.mode_override.as_deref());
        let allowed: Option<Vec<ToolCapability>> = match &spec.allowed_capabilities {
            Some(names) => Some(
                names
                    .iter()
                    .filter_map(|n| ToolCapability::parse(n))
                    .collect(),
            ),
            None => role_default_capabilities(spec.role()),
        };
        let session_filter = if spec.mode_override.is_none() {
            self.deps.session_tool_filter.clone()
        } else {
            None
        };
        // Approval gating: the phase's own Safe override, else the session's
        // Safe filter for a phase that follows the session mode.
        let approval_filter: Option<ModeToolFilter> = if policy.approval_gated {
            Some(Arc::new(|name: &str| !capability_of(name).is_read_only()))
        } else if spec.mode_override.is_none() {
            self.deps.session_approval_filter.clone()
        } else {
            None
        };
        let tools: Vec<Arc<ErasedTool>> = self
            .tool_pool()
            .into_iter()
            .filter(|t| {
                let name = t.name();
                let cap = capability_of(name);
                if let Some(filter) = &session_filter {
                    if !filter(name) {
                        return false;
                    }
                }
                if policy.read_only && !cap.is_read_only() {
                    return false;
                }
                if let Some(allowed) = &allowed {
                    if !allowed.contains(&cap) {
                        return false;
                    }
                }
                if let Some(list) = &spec.tools {
                    if !list.iter().any(|n| n == name) {
                        return false;
                    }
                }
                true
            })
            .collect();
        match approval_filter {
            Some(filter) => crate::mode_gate::require_approval(tools, &filter),
            None => tools,
        }
    }

    fn system_prompt(
        &self,
        spec: &PhaseDefinition,
        transitions: &[String],
        mutations: &[String],
        tctx: &TemplateContext<'_>,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        let policy = mode_policy(spec.mode_override.as_deref());
        if spec.mode_override.is_some() {
            if !policy.patch.is_empty() {
                parts.push(policy.patch.to_string());
            }
        } else if let Some(patch) = &self.deps.session_mode_patch {
            if !patch.trim().is_empty() {
                parts.push(patch.clone());
            }
        }
        let index = self.phase(&spec.name).map(|(i, _)| i + 1).unwrap_or(1);
        parts.push(format!(
            "[WORKFLOW]\nYou are running phase '{}' ({index}/{}) of the '{}' workflow. Working \
             directory: {}.",
            spec.name,
            self.def().phases.len(),
            self.wf.name,
            self.deps.working_dir.display()
        ));
        let role_part = if spec.system_prompt.trim().is_empty() {
            if spec.role() == "custom" {
                String::new()
            } else {
                role_prompt(spec.role()).to_string()
            }
        } else {
            render_template(&spec.system_prompt, tctx)
        };
        if !role_part.trim().is_empty() {
            parts.push(role_part);
        }
        for block in &self.deps.project_context {
            if !block.trim().is_empty() {
                parts.push(block.clone());
            }
        }
        parts.push(CLARIFICATION_REMINDER.to_string());
        parts.push(transition_instruction(&spec.name, transitions, mutations));
        parts.join("\n\n")
    }
}

/// agenthicc's `_determine_transition`, extended with transition edges.
fn determine_transition(spec: &PhaseDefinition, out: &PhaseOutput) -> Option<String> {
    if out.metadata.get(META_END) == Some(&json!(true)) {
        return None;
    }
    if let Some(Value::String(next)) = out.metadata.get(META_NEXT) {
        return Some(next.clone());
    }
    if out.approved == Some(false) {
        if let Some(reject) = &spec.on_reject {
            return Some(reject.clone());
        }
    }
    spec.next.clone()
}

fn dedupe_by_name(tools: &mut Vec<Arc<ErasedTool>>) {
    let mut seen: HashSet<String> = HashSet::new();
    tools.retain(|t| seen.insert(t.name().to_string()));
}

// ── Command outcome recording ──────────────────────────────────────────────

/// One shell command a phase ran, for the command gate.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandOutcome {
    /// The tool that ran it.
    pub tool: String,
    /// The command line (or tool name when there is none) — the dedupe key.
    pub key: String,
    /// Whether it succeeded.
    pub ok: bool,
    /// Whether it established service readiness.
    pub ready: bool,
    /// First lines of its result.
    pub summary: String,
}

#[derive(Clone, Default)]
struct CommandRecorder {
    outcomes: Arc<Mutex<Vec<CommandOutcome>>>,
}

impl CommandRecorder {
    fn wrap(&self, inner: Arc<ErasedTool>) -> Arc<ErasedTool> {
        Arc::new(RecordingTool {
            inner,
            sink: Arc::clone(&self.outcomes),
        }) as Arc<ErasedTool>
    }

    fn outcomes(&self) -> Vec<CommandOutcome> {
        self.outcomes.lock().map(|o| o.clone()).unwrap_or_default()
    }
}

/// Delegates to a shell tool and records whether the command succeeded.
struct RecordingTool {
    inner: Arc<ErasedTool>,
    sink: Arc<Mutex<Vec<CommandOutcome>>>,
}

#[async_trait::async_trait]
impl Tool for RecordingTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn schema(&self) -> Value {
        self.inner.schema()
    }

    fn requires_confirmation(&self) -> bool {
        self.inner.requires_confirmation()
    }

    fn parallel_safe(&self) -> bool {
        self.inner.parallel_safe()
    }

    fn cache_ttl(&self) -> Option<std::time::Duration> {
        self.inner.cache_ttl()
    }

    fn hooks(&self) -> Vec<Arc<dyn agtrs_runtime::tool_hooks::ToolHook>> {
        self.inner.hooks()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let key = ["command", "cmd", "script", "code", "terminal_id"]
            .iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
            .unwrap_or(self.inner.name())
            .to_string();
        let result = self.inner.call(input, ctx).await;
        let (ok, summary) = match &result {
            Ok(r) => (
                !r.is_error,
                r.content.lines().take(6).collect::<Vec<_>>().join("\n"),
            ),
            Err(e) => (false, e.to_string()),
        };
        let ready = ok && self.inner.name() == "wait_terminal_ready";
        if let Ok(mut sink) = self.sink.lock() {
            sink.push(CommandOutcome {
                tool: self.inner.name().to_string(),
                key,
                ok,
                ready,
                summary,
            });
        }
        result
    }
}

#[cfg(test)]
mod engine_tests;
