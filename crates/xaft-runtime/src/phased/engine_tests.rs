//! Scripted end-to-end tests of the phased engine.
//!
//! Every test drives a real `HandoffOrchestrator` over agtrs's
//! `MockTransport`: queued completions stand in for the model, so transition
//! tools, continuation turns, checkpoints and resume all run for real. A
//! transition costs two queued responses — the tool call, then the closing
//! text the agent produces after the tool result.

use std::sync::Mutex as StdMutex;

use agtrs_runtime::approval::{AlwaysReject, AutoApprove};
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use serde_json::json;
use tempfile::TempDir;

use xaft_config::types::{TransitionDefinition, TransitionParam, WorkflowMode};

use super::*;

// ── Fixtures ────────────────────────────────────────────────────────────────

fn phase(name: &str, next: Option<&str>) -> PhaseDefinition {
    PhaseDefinition {
        name: name.into(),
        next: next.map(str::to_string),
        ..Default::default()
    }
}

fn workflow(name: &str, phases: Vec<PhaseDefinition>) -> PhasedWorkflow {
    PhasedWorkflow {
        name: name.into(),
        definition: WorkflowDefinition {
            mode: Some(WorkflowMode::Phased),
            phases,
            ..Default::default()
        },
    }
}

/// A tool with a fixed name and result — enough for filtering and gates.
struct FakeTool {
    name: &'static str,
    fail: bool,
    calls: Arc<StdMutex<Vec<Value>>>,
}

impl FakeTool {
    fn erased(name: &'static str) -> Arc<ErasedTool> {
        Self::with(name, false).0
    }

    fn with(name: &'static str, fail: bool) -> (Arc<ErasedTool>, Arc<StdMutex<Vec<Value>>>) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        (
            Arc::new(Self {
                name,
                fail,
                calls: Arc::clone(&calls),
            }) as Arc<ErasedTool>,
            calls,
        )
    }
}

#[async_trait::async_trait]
impl Tool for FakeTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "fake"
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}})
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let fail_this =
            self.fail && input.get("command").and_then(Value::as_str) != Some("passing");
        self.calls.lock().unwrap().push(input);
        Ok(if fail_this {
            ToolResult::error("COMMAND FAILED — exit code 1", &ctx.tool_use_id)
        } else {
            ToolResult::ok("Exit code: 0 (success)", &ctx.tool_use_id)
        })
    }
}

struct Harness {
    tmp: TempDir,
    transport: Arc<MockTransport>,
    deps: PhasedDeps,
}

impl Harness {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let transport = Arc::new(MockTransport::new());
        let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(Arc::clone(&transport)));
        let deps = PhasedDeps {
            llm,
            llm_for_model: None,
            signals: Arc::new(SignalBus::new()),
            resolve_ctx: Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
                injectable_rs_runtime::EmptySingletonStore,
            ))),
            read_tools: vec![
                FakeTool::erased("read_file"),
                FakeTool::erased("git_status"),
            ],
            write_tools: vec![
                FakeTool::erased("read_file"),
                FakeTool::erased("git_status"),
                FakeTool::erased("write_file"),
                FakeTool::erased("bash_exec"),
            ],
            session_tool_filter: None,
            session_mode_patch: None,
            project_context: Vec::new(),
            conversation_store: None,
            approval_gate: None,
            question_gate: None,
            working_dir: tmp.path().to_path_buf(),
            session_id: "sess".into(),
            user_parts: None,
            config: XaftConfig::default(),
            checkpoints: Some(CheckpointStore::for_working_dir(tmp.path())),
        };
        Self {
            tmp,
            transport,
            deps,
        }
    }

    async fn text(&self, t: &str) {
        self.transport.queue_text(t).await;
    }

    /// A transition call followed by the agent's closing text.
    async fn call(&self, tool: &str, input: Value) {
        self.transport.queue_tool_call(tool, input).await;
        self.transport.queue_text("ok").await;
    }

    fn store(&self) -> CheckpointStore {
        CheckpointStore::for_working_dir(self.tmp.path())
    }
}

fn history(state: &WorkflowRunState) -> Vec<&str> {
    state
        .phase_history
        .iter()
        .map(|r| r.phase_name.as_str())
        .collect()
}

// ── Graph execution ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_linear_workflow_runs_every_phase_and_checkpoints_completion() {
    let h = Harness::new();
    h.text("A done").await;
    h.text("B done").await;
    let wf = workflow("linear", vec![phase("a", Some("b")), phase("b", None)]);

    let out = run_phased_workflow(&wf, "do it", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(out.content, "B done");
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(history(&out.state), ["a", "b"]);
    let saved = h.store().load(&out.state.run_id).unwrap();
    assert_eq!(saved.status, RunStatus::Complete);
    assert_eq!(saved.next_phase, None);
}

#[tokio::test]
async fn code_plan_loops_through_a_rejected_review() {
    let h = Harness::new();
    // plan
    h.call("finalize_plan", json!({"plan": "1. do x"})).await;
    // execute
    h.call("mark_execute_complete", json!({"summary": "did x"}))
        .await;
    // review rejects → execute again
    h.call("reject_review", json!({"reason": "tests fail"}))
        .await;
    h.call("mark_execute_complete", json!({"summary": "fixed"}))
        .await;
    // review approves → summarize
    h.call("approve_review", json!({"summary": "good"})).await;
    h.text("Summary: x is done").await;

    let def = xaft_config::workflows::builtin::builtin_phased_definitions()
        .into_iter()
        .find(|(n, _)| n == "code_plan")
        .unwrap()
        .1;
    let wf = PhasedWorkflow {
        name: "code_plan".into(),
        definition: def,
    };
    let out = run_phased_workflow(&wf, "add x", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "plan",
            "execute",
            "review",
            "execute",
            "review",
            "summarize"
        ]
    );
    assert_eq!(out.content, "Summary: x is done");
    let rejected = &out.state.phase_history[2];
    assert_eq!(rejected.approved, Some(false));
    assert_eq!(out.state.phase_outputs["plan"].full_text, "1. do x");
    assert_eq!(
        out.state.phase_outputs["review"].transition.as_deref(),
        Some("approve_review")
    );
    assert_eq!(out.state.phase_iterations["execute"], 2);
}

#[tokio::test]
async fn a_turn_without_the_completion_call_gets_a_continuation_turn() {
    let h = Harness::new();
    h.text("still working").await;
    h.call("mark_execute_complete", json!({"summary": "done now"}))
        .await;
    let mut exec = phase("execute", None);
    exec.require_explicit_completion = true;
    let wf = workflow("cont", vec![exec]);

    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(out.state.phase_history.len(), 1, "one phase run, two turns");
    assert_eq!(out.state.phase_outputs["execute"].approved, Some(true));
    assert_eq!(out.state.phase_outputs["execute"].full_text, "done now");
}

#[tokio::test]
async fn declared_transitions_follow_their_edge_and_carry_arguments() {
    let h = Harness::new();
    h.call("submit_design", json!({"design": "use redis"}))
        .await;
    h.text("built with {output.design}").await;

    let mut design = phase("design", Some("unused"));
    design.transitions = vec![TransitionDefinition {
        tool: "submit_design".into(),
        description: "Submit.".into(),
        params: vec![TransitionParam {
            name: "design".into(),
            ..Default::default()
        }],
        next: Some("build".into()),
        ..Default::default()
    }];
    let mut build = phase("build", None);
    build.prompt = "Implement: {data.design.design}".into();
    let wf = workflow("declared", vec![design, phase("unused", None), build]);

    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(history(&out.state), ["design", "build"]);
    assert_eq!(
        out.state.phase_outputs["design"].structured,
        Some(json!({"design": "use redis"}))
    );
}

#[tokio::test]
async fn a_failed_run_is_checkpointed_and_resumes_at_the_failed_phase() {
    let h = Harness::new();
    // `gather` never calls its transition within its two attempts → fails.
    h.text("A").await;
    h.text("thinking").await;
    h.text("still thinking").await;

    let mut gather = phase("gather", None);
    gather.max_iterations = Some(2);
    gather.transitions = vec![TransitionDefinition {
        tool: "done_gathering".into(),
        description: "Finish.".into(),
        next: Some("report".into()),
        ..Default::default()
    }];
    let wf = workflow(
        "resumable",
        vec![
            phase("intro", Some("gather")),
            gather,
            phase("report", None),
        ],
    );

    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("done_gathering"), "{err}");

    let failed = h
        .store()
        .latest_resumable(Some("resumable"))
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(failed.next_phase.as_deref(), Some("gather"));
    assert_eq!(history(&failed), ["intro"]);

    // Resume: only `gather` and `report` run; `intro` is not repeated.
    h.call("done_gathering", json!({})).await;
    h.text("final report").await;
    let out = run_phased_workflow(&wf, "ignored", h.deps.clone(), Some(failed.clone()))
        .await
        .unwrap();
    assert_eq!(out.state.run_id, failed.run_id, "same durable run");
    assert_eq!(history(&out.state), ["intro", "gather", "report"]);
    assert_eq!(out.content, "final report");
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(out.state.intent, "t", "the checkpointed intent wins");
}

#[tokio::test]
async fn a_completed_run_has_nothing_to_resume() {
    let h = Harness::new();
    h.text("only").await;
    let wf = workflow("once", vec![phase("only", None)]);
    let done = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap()
        .state;
    let again = run_phased_workflow(&wf, "t", h.deps.clone(), Some(done))
        .await
        .unwrap();
    assert_eq!(history(&again.state), ["only"], "no phase re-ran");
}

#[tokio::test]
async fn resuming_another_workflows_run_is_refused() {
    let h = Harness::new();
    h.text("x").await;
    let a = workflow("a_flow", vec![phase("only", None)]);
    let state = run_phased_workflow(&a, "t", h.deps.clone(), None)
        .await
        .unwrap()
        .state;
    let b = workflow("b_flow", vec![phase("only", None)]);
    assert!(
        run_phased_workflow(&b, "t", h.deps.clone(), Some(state))
            .await
            .is_err()
    );
}

// ── Human phases, limits, exit ──────────────────────────────────────────────

#[tokio::test]
async fn a_headless_human_phase_auto_approves() {
    let h = Harness::new();
    h.text("draft").await;
    h.text("final").await;
    let mut gate = phase("approve", Some("finish"));
    gate.agent_type = "human".into();
    gate.on_reject = Some("draft".into());
    let wf = workflow(
        "human",
        vec![phase("draft", Some("approve")), gate, phase("finish", None)],
    );
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(history(&out.state), ["draft", "approve", "finish"]);
    assert_eq!(
        out.state.phase_outputs["approve"].full_text,
        "[auto-approved: no approval service]"
    );
}

#[tokio::test]
async fn a_human_rejection_follows_on_reject_until_max_iterations() {
    let mut h = Harness::new();
    h.deps.approval_gate = Some(Arc::new(AlwaysReject));
    h.text("draft 1").await;
    h.text("draft 2").await;
    let mut draft = phase("draft", Some("approve"));
    draft.max_iterations = Some(2);
    let mut gate = phase("approve", None);
    gate.agent_type = "human".into();
    gate.on_reject = Some("draft".into());
    let wf = workflow("human_reject", vec![draft, gate]);

    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("max_iterations=2"), "{err}");
}

#[tokio::test]
async fn max_total_phase_runs_caps_the_run() {
    let h = Harness::new();
    for _ in 0..3 {
        h.text("x").await;
    }
    let mut a = phase("a", Some("b"));
    a.max_iterations = Some(99);
    let wf = PhasedWorkflow {
        name: "capped".into(),
        definition: WorkflowDefinition {
            mode: Some(WorkflowMode::Phased),
            max_total_phase_runs: Some(3),
            phases: vec![a, phase("b", Some("a"))],
            ..Default::default()
        },
    };
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("limit: 3"), "{err}");
}

#[tokio::test]
async fn exit_workflow_ends_the_run_early() {
    let h = Harness::new();
    h.transport
        .queue_tool_call("exit_workflow", json!({"reason": "not a coding task"}))
        .await;
    h.text("This is a question, not a change request.").await;
    let mut plan = phase("plan", Some("execute"));
    plan.require_plan_finalization = true;
    let wf = workflow("exits", vec![plan, phase("execute", None)]);
    let out = run_phased_workflow(&wf, "what is 2+2", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(out.state.status, RunStatus::Exited);
    assert_eq!(history(&out.state), ["plan"]);
    assert_eq!(out.content, "This is a question, not a change request.");
}

#[tokio::test]
async fn an_error_routes_to_on_error_when_declared() {
    let h = Harness::new();
    // `fragile` never calls its transition → error → on_error `recover`.
    h.text("nope").await;
    h.text("recovered").await;
    let mut fragile = phase("fragile", Some("unused"));
    fragile.max_iterations = Some(1);
    fragile.on_error = Some("recover".into());
    fragile.transitions = vec![TransitionDefinition {
        tool: "finish".into(),
        description: "d".into(),
        ..Default::default()
    }];
    let wf = workflow(
        "errs",
        vec![fragile, phase("unused", None), phase("recover", None)],
    );
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(history(&out.state), ["fragile", "recover"]);
    assert!(
        out.state.phase_outputs["fragile"]
            .metadata
            .contains_key("error")
    );
}

// ── Parallel phases ─────────────────────────────────────────────────────────

#[tokio::test]
async fn parallel_peers_run_together_and_are_not_repeated() {
    let h = Harness::new();
    h.text("peer output").await;
    h.text("peer output").await;
    h.text("joined").await;
    let mut left = phase("left", Some("join"));
    left.parallel_with = vec!["right".into()];
    let wf = workflow(
        "fanout",
        vec![left, phase("right", Some("join")), phase("join", None)],
    );
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    let mut ran = history(&out.state);
    ran[..2].sort_unstable();
    assert_eq!(ran, ["left", "right", "join"]);
    assert_eq!(out.content, "joined");
}

// ── Command gate ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_failed_required_command_fails_the_phase() {
    let mut h = Harness::new();
    let (shell, _) = FakeTool::with("bash_exec", true);
    h.deps.write_tools = vec![shell];
    h.transport
        .queue_tool_call("bash_exec", json!({"command": "cargo test"}))
        .await;
    h.text("tests ran").await;
    let mut verify = phase("verify", None);
    verify.require_successful_commands = true;
    let wf = workflow("gated", vec![verify]);
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("required command did not succeed"),
        "{err}"
    );
    let saved = h.store().latest_resumable(Some("gated")).unwrap().unwrap();
    assert_eq!(saved.next_phase.as_deref(), Some("verify"));
}

#[tokio::test]
async fn a_command_that_later_succeeds_passes_the_gate() {
    let mut h = Harness::new();
    let (shell, calls) = FakeTool::with("bash_exec", true);
    h.deps.write_tools = vec![shell];
    h.transport
        .queue_tool_call("bash_exec", json!({"command": "passing"}))
        .await;
    h.text("green").await;
    let mut verify = phase("verify", None);
    verify.require_successful_commands = true;
    let wf = workflow("gated_ok", vec![verify]);
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "the wrapped tool really ran"
    );
}

// ── Tool selection ──────────────────────────────────────────────────────────

fn tool_names(engine: &Engine<'_>, spec: &PhaseDefinition) -> Vec<String> {
    let mut names: Vec<String> = engine
        .base_tools(spec)
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn tools_follow_mode_role_capabilities_and_allowlist() {
    let h = Harness::new();
    let wf = workflow("tools", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());

    let auto = phase("x", None);
    assert_eq!(
        tool_names(&engine, &auto),
        ["bash_exec", "git_status", "read_file", "write_file"]
    );

    let mut plan = phase("x", None);
    plan.mode_override = Some("plan".into());
    assert_eq!(tool_names(&engine, &plan), ["git_status", "read_file"]);

    let mut reviewer = phase("x", None);
    reviewer.agent_type = "reviewer".into();
    assert_eq!(tool_names(&engine, &reviewer), ["git_status", "read_file"]);

    let mut writer = phase("x", None);
    writer.allowed_capabilities = Some(vec!["write".into()]);
    assert_eq!(tool_names(&engine, &writer), ["write_file"]);

    let mut listed = phase("x", None);
    listed.tools = Some(vec!["read_file".into(), "bash_exec".into()]);
    assert_eq!(tool_names(&engine, &listed), ["bash_exec", "read_file"]);
}

#[test]
fn the_session_filter_applies_only_without_a_phase_override() {
    let mut h = Harness::new();
    h.deps.session_tool_filter = Some(Arc::new(|name: &str| name == "read_file"));
    let wf = workflow("tools", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());

    assert_eq!(tool_names(&engine, &phase("x", None)), ["read_file"]);
    let mut yolo = phase("x", None);
    yolo.mode_override = Some("auto".into());
    assert_eq!(
        tool_names(&engine, &yolo),
        ["bash_exec", "git_status", "read_file", "write_file"],
        "a phase mode override replaces the session mode, as in agenthicc"
    );
}

// ── Workflow state through tools ────────────────────────────────────────────

#[tokio::test]
async fn a_goals_complete_guard_holds_until_every_goal_is_done() {
    let h = Harness::new();
    // finish refused (no goals) → set goals → complete → finish accepted.
    h.transport.queue_tool_call("finish", json!({})).await;
    h.transport
        .queue_tool_call("set_goals", json!({"goals": "one\ntwo"}))
        .await;
    h.transport
        .queue_tool_call("complete_goal", json!({"id": "1", "evidence": "e"}))
        .await;
    h.transport
        .queue_tool_call("complete_goal", json!({"id": "2", "evidence": "e"}))
        .await;
    h.call("finish", json!({})).await;

    let mut work = phase("work", None);
    work.extra_tools = vec!["goal_list".into()];
    work.transitions = vec![TransitionDefinition {
        tool: "finish".into(),
        description: "All goals done.".into(),
        guard: Some("goals_complete".into()),
        ..Default::default()
    }];
    let wf = workflow("goals", vec![work]);
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        out.state.phase_outputs["work"].transition.as_deref(),
        Some("finish")
    );
    let goals = out.state.data["goals"].as_array().unwrap();
    assert!(goals.iter().all(|g| g["status"] == "done"));
}

#[tokio::test]
async fn plan_approval_goes_through_the_approval_gate() {
    let mut h = Harness::new();
    h.deps.approval_gate = Some(Arc::new(AutoApprove));
    h.transport
        .queue_tool_call("finalize_plan", json!({"plan": "p"}))
        .await;
    h.transport
        .queue_tool_call("request_plan_approval", json!({"plan": "p"}))
        .await;
    h.call("finalize_plan", json!({"plan": "p"})).await;
    let mut plan = phase("plan", None);
    plan.require_plan_finalization = true;
    let wf = workflow("approved_plan", vec![plan]);
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(out.state.phase_outputs["plan"].approved, Some(true));
    assert_eq!(out.state.data.get("plan_approved"), Some(&json!(true)));
}

// ── Models and signals ──────────────────────────────────────────────────────

#[tokio::test]
async fn per_phase_models_are_built_once_per_model() {
    let mut h = Harness::new();
    let requested = Arc::new(StdMutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&requested);
    let llm = Arc::clone(&h.deps.llm);
    h.deps.llm_for_model = Some(Arc::new(move |model: &str| {
        seen.lock().unwrap().push(model.to_string());
        Ok(Arc::clone(&llm))
    }));
    for t in ["a", "b", "c"] {
        h.text(t).await;
    }
    let mut wf = workflow(
        "models",
        vec![
            phase("a", Some("b")),
            phase("b", Some("c")),
            phase("c", None),
        ],
    );
    wf.definition
        .phase_models
        .insert("b".into(), "cheap".into());
    wf.definition
        .params
        .insert("c_model".into(), toml::Value::String("cheap".into()));
    run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        *requested.lock().unwrap(),
        ["cheap"],
        "cached after first build"
    );
}

#[tokio::test]
async fn lifecycle_signals_are_emitted() {
    let h = Harness::new();
    h.text("a").await;
    let started = Arc::new(StdMutex::new(Vec::<String>::new()));
    let completed = Arc::new(StdMutex::new(Vec::<String>::new()));
    {
        let s = Arc::clone(&started);
        h.deps
            .signals
            .on::<XaftWorkflowPhaseStarted>(move |e| s.lock().unwrap().push(e.phase_name.clone()))
            .await;
        let c = Arc::clone(&completed);
        h.deps
            .signals
            .on::<XaftWorkflowRunCompleted>(move |e| c.lock().unwrap().push(e.status.clone()))
            .await;
    }
    let wf = workflow("signals", vec![phase("a", None)]);
    run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    // Handlers run on the bus; give them a moment.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(*started.lock().unwrap(), ["a"]);
    assert_eq!(*completed.lock().unwrap(), ["complete"]);
}

#[tokio::test]
async fn a_resume_records_the_users_continuation() {
    let h = Harness::new();
    h.text("first").await;
    h.text("never finishes").await;
    let mut second = phase("second", None);
    second.max_iterations = Some(1);
    second.transitions = vec![TransitionDefinition {
        tool: "finish".into(),
        description: "d".into(),
        ..Default::default()
    }];
    let wf = workflow("cont_note", vec![phase("first", Some("second")), second]);
    run_phased_workflow(&wf, "original", h.deps.clone(), None)
        .await
        .unwrap_err();
    let failed = h.store().latest_resumable(None).unwrap().unwrap();

    h.call("finish", json!({})).await;
    let out = run_phased_workflow(&wf, "use the v2 API instead", h.deps.clone(), Some(failed))
        .await
        .unwrap();
    assert_eq!(
        out.state.data.get("continuations"),
        Some(&json!(["use the v2 API instead"]))
    );
    assert_eq!(out.state.intent, "original");
}

#[test]
fn project_context_reaches_every_phase_system_prompt() {
    let mut h = Harness::new();
    h.deps.project_context = vec!["# AGENTS.md\nAlways run cargo fmt.".into()];
    let wf = workflow("ctx", vec![phase("a", None)]);
    let engine = Engine::new(&wf, h.deps.clone());
    let state = WorkflowRunState::new("r", "ctx", "t", "s", "/w", "fp");
    let data = RunData::default();
    let tctx = TemplateContext {
        state: &state,
        data: &data,
        phase: "a",
        iteration: 1,
    };
    let prompt = engine.system_prompt(&wf.definition.phases[0], &[], &[], &tctx);
    assert!(prompt.contains("Always run cargo fmt."), "{prompt}");
    assert!(prompt.contains("[REQUIREMENTS CLARIFICATION]"));
    assert!(prompt.contains("No phase-transition tool is available in the 'a' phase"));
}

// ── Script phases ───────────────────────────────────────────────────────────

#[cfg(unix)]
fn script(name: &str, command: &str, next: Option<&str>) -> PhaseDefinition {
    PhaseDefinition {
        name: name.into(),
        agent_type: "script".into(),
        command: Some(command.into()),
        next: next.map(str::to_string),
        ..Default::default()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_phase_runs_without_a_model_and_feeds_state_forward() {
    let h = Harness::new();
    h.text("used {state.version}").await;
    let check = script(
        "probe",
        r#"printf '{"summary": "%s in %s", "state": {"version": "1.2.3"}}' "$XAFT_PHASE" "$XAFT_WORKFLOW""#,
        Some("report"),
    );
    let mut report = phase("report", None);
    report.prompt = "Version is {state.version}".into();
    let wf = workflow("scripted", vec![check, report]);
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(history(&out.state), ["probe", "report"]);
    assert_eq!(
        out.state.phase_outputs["probe"].full_text,
        "probe in scripted"
    );
    assert_eq!(out.state.phase_outputs["probe"].role, "script");
    assert_eq!(out.state.data["state"]["version"], json!("1.2.3"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_script_follows_on_reject_and_can_loop_back() {
    let h = Harness::new();
    h.text("fixed it").await;
    // Fails until a marker file exists; the fix phase creates it.
    let mut test = script(
        "test",
        "test -f fixed.marker || { echo 'tests failed' >&2; exit 1; }",
        Some("done"),
    );
    test.on_reject = Some("fix".into());
    test.max_iterations = Some(3);
    let mut fix = script("fix", "touch fixed.marker && echo patched", Some("test"));
    fix.max_iterations = Some(3);
    let wf = workflow(
        "loop",
        vec![test, fix, script("done", "echo all green", None)],
    );
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(history(&out.state), ["test", "fix", "test", "done"]);
    assert_eq!(out.state.phase_history[0].approved, Some(false));
    assert!(
        out.state.phase_outputs["test"].full_text.is_empty()
            || out.state.phase_history[2].approved == Some(true)
    );
    assert_eq!(out.content, "all green");
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_script_without_an_edge_fails_the_run_resumably() {
    let h = Harness::new();
    let wf = workflow(
        "broken",
        vec![script("only", "echo nope >&2; exit 3", None)],
    );
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exit code 3"), "{err}");
    assert!(err.to_string().contains("nope"), "{err}");
    let saved = h.store().latest_resumable(Some("broken")).unwrap().unwrap();
    assert_eq!(saved.next_phase.as_deref(), Some("only"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_verdict_can_pick_the_next_phase_or_exit() {
    let h = Harness::new();
    let wf = workflow(
        "routing",
        vec![
            script(
                "route",
                r#"echo '{"next": "b", "summary": "go b"}'"#,
                Some("a"),
            ),
            script("a", "echo a", None),
            script(
                "b",
                r#"echo '{"exit": true, "summary": "nothing to do"}'"#,
                Some("a"),
            ),
        ],
    );
    let out = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(history(&out.state), ["route", "b"]);
    assert_eq!(out.state.status, RunStatus::Exited);
    assert_eq!(out.content, "nothing to do");
}

#[cfg(unix)]
#[tokio::test]
async fn a_declined_script_is_not_run() {
    let mut h = Harness::new();
    h.deps.approval_gate = Some(Arc::new(AlwaysReject));
    let wf = workflow(
        "guarded",
        vec![script("danger", "touch should-not-exist", None)],
    );
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("declined"), "{err}");
    assert!(!h.tmp.path().join("should-not-exist").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_script_that_overruns_its_timeout_is_killed() {
    let h = Harness::new();
    let mut slow = script("slow", "sleep 5", None);
    slow.timeout_s = Some(1);
    let wf = workflow("timeout", vec![slow]);
    let started = std::time::Instant::now();
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out after 1s"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
}

#[cfg(unix)]
#[tokio::test]
async fn the_script_can_read_the_run_file() {
    let h = Harness::new();
    h.text("first phase text").await;
    let reader = script(
        "inspect",
        r#"grep -q '"intent": "inspect me"' "$XAFT_RUN_FILE" && echo found"#,
        None,
    );
    let wf = workflow("runfile", vec![phase("first", Some("inspect")), reader]);
    let out = run_phased_workflow(&wf, "inspect me", h.deps.clone(), None)
        .await
        .unwrap();
    assert_eq!(out.content, "found");
}

// ── Built-in workflow ports: one scripted main path each ────────────────────

fn builtin(name: &str) -> PhasedWorkflow {
    let definition = xaft_config::workflows::builtin::builtin_phased_definitions()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no built-in {name}"))
        .1;
    PhasedWorkflow {
        name: name.into(),
        definition,
    }
}

/// goal_flow: clarify → decide → implement ⇄ verify per goal → summarize,
/// including a failed verification, a control of the active goal and the
/// guarded completion.
#[tokio::test]
async fn goal_flow_runs_each_goal_through_implement_and_verify() {
    let h = Harness::new();
    h.call(
        "complete_clarification",
        json!({"notes": "Two goals, Rust only."}),
    )
    .await;
    h.call(
        "finalize_goals",
        json!({"goals": ["add the parser", {"text": "add the cache"}]}),
    )
    .await;
    // Goal #1: implemented, rejected by verification, fixed, verified.
    h.call(
        "goal_implemented",
        json!({"summary": "parser v1", "files": ["src/parser.rs"]}),
    )
    .await;
    h.call(
        "verify_goal",
        json!({"satisfied": false, "evidence": "edge case fails"}),
    )
    .await;
    h.call(
        "goal_implemented",
        json!({"summary": "parser v2", "files": ["src/parser.rs", "tests/parser.rs"]}),
    )
    .await;
    h.call(
        "verify_goal",
        json!({"satisfied": "true", "evidence": "all green"}),
    )
    .await;
    // Goal #2: the agent finds it is no longer needed and skips the active
    // goal — the phase ends there and implement_goal is skipped for summarize.
    h.call(
        "skip_phase",
        json!({"phase_id": "2", "reason": "the parser caches already"}),
    )
    .await;
    // Completion is refused while a goal is open; here nothing is.
    h.call(
        "complete_workflow",
        json!({"summary": "Parser added; cache skipped.", "files": ["src/parser.rs"]}),
    )
    .await;

    let out = run_phased_workflow(
        &builtin("goal_flow"),
        "build a parser",
        h.deps.clone(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "clarify",
            "decide_goals",
            "implement_goal",
            "verify_goal",
            "implement_goal",
            "verify_goal",
            "implement_goal",
            "summarize"
        ]
    );
    assert_eq!(out.state.status, RunStatus::Complete);
    let goals = out.state.data["goals"].as_array().unwrap();
    assert_eq!(goals[0]["status"], json!("done"));
    assert_eq!(goals[0]["attempts"], json!(2));
    assert_eq!(
        goals[0]["files"],
        json!(["src/parser.rs", "tests/parser.rs"])
    );
    assert_eq!(goals[1]["status"], json!("skipped"));
    assert_eq!(
        out.state.phase_history[6].approved,
        Some(false),
        "a control of the active goal rejects the phase's work"
    );
    assert_eq!(
        out.state.phase_outputs["summarize"].structured,
        Some(json!({"summary": "Parser added; cache skipped.", "files": ["src/parser.rs"]}))
    );
    assert!(
        out.content
            .starts_with("summary: Parser added; cache skipped."),
        "{}",
        out.content
    );
    let receipts = out.state.data["goal_mutation_receipts"].as_array().unwrap();
    assert_eq!(receipts.last().unwrap()["operation"], json!("skip"));
}

/// complete_workflow is refused while goals remain open, and the summarize
/// phase is never entered while work is pending.
#[tokio::test]
async fn goal_flow_cannot_complete_with_an_open_goal() {
    let h = Harness::new();
    let data = RunData::default();
    data.set(
        "goals",
        json!([{"id": 1, "title": "x", "status": "pending"}]),
    );
    let control: tools::SharedControl = Arc::new(Mutex::new(PhaseControl::default()));
    let err = groups::check_guard("goals_complete", &data, &control).unwrap_err();
    assert!(err.contains("#1 x"), "{err}");
    let wf = builtin("goal_flow");
    let summarize = wf
        .definition
        .phases
        .iter()
        .find(|p| p.name == "summarize")
        .unwrap();
    assert_eq!(
        summarize.transitions[0].guard.as_deref(),
        Some("goals_complete")
    );
    drop(h);
}

#[cfg(unix)]
fn git_repo(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "xaft-test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "xaft-test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("git is installed");
        assert!(out.status.success(), "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "xaft-test"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
}

/// parallel_code_plan: plan → decompose → workers in their own worktrees →
/// collect → integrate → verify → summarize, recorded in the shared
/// orchestration store `xaft worktrees` reads.
#[cfg(unix)]
#[tokio::test]
async fn parallel_code_plan_runs_isolated_workers_and_integrates_them() {
    let mut h = Harness::new();
    git_repo(h.tmp.path());
    let state_dir = TempDir::new().unwrap();
    h.deps.config.execution.max_parallel_tasks = 1;
    let mut wf = builtin("parallel_code_plan");
    wf.definition.params.insert(
        "orchestration_dir".into(),
        toml::Value::String(
            state_dir
                .path()
                .join("orchestrations")
                .display()
                .to_string(),
        ),
    );
    wf.definition.params.insert(
        "worktree_root".into(),
        toml::Value::String(state_dir.path().join("worktrees").display().to_string()),
    );

    h.call("finalize_plan", json!({"plan": "Two independent files."}))
        .await;
    for (task, file) in [("alpha", "a.txt"), ("beta", "b.txt")] {
        h.transport
            .queue_tool_call(
                "add_parallel_task",
                json!({"task_id": task, "description": format!("create {file}"), "dependencies": []}),
            )
            .await;
    }
    h.call("finalize_task_graph", json!({"summary": "alpha and beta"}))
        .await;
    // dispatch: the coordinator's call, then each worker's turn, then the
    // coordinator's closing text.
    h.transport
        .queue_tool_call(
            "dispatch_parallel_workers",
            json!({"summary": "both ready"}),
        )
        .await;
    for file in ["a.txt", "b.txt"] {
        h.transport
            .queue_tool_call(
                "bash_exec",
                json!({"command": format!("printf 'x\\n' > {file} && git add {file} && git commit -qm {file}")}),
            )
            .await;
        h.text("committed").await;
    }
    h.text("dispatched").await;
    h.call(
        "collect_parallel_workers",
        json!({"summary": "both committed"}),
    )
    .await;
    h.transport
        .queue_tool_call(
            "integrate_parallel_worker",
            json!({"task_id": "alpha", "summary": "merge"}),
        )
        .await;
    h.call(
        "integrate_parallel_worker",
        json!({"task_id": "beta", "summary": "merge"}),
    )
    .await;
    h.call(
        "mark_parallel_complete",
        json!({"summary": "README and both files present"}),
    )
    .await;
    h.text("Orchestration complete: alpha and beta integrated.")
        .await;

    let out = run_phased_workflow(&wf, "add two files", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "plan",
            "decompose",
            "dispatch",
            "collect",
            "integrate",
            "verify",
            "summarize"
        ]
    );
    assert_eq!(out.state.status, RunStatus::Complete);
    assert!(h.tmp.path().join("a.txt").is_file() && h.tmp.path().join("b.txt").is_file());
    let store = xaft_tools::worktrees::ManifestStore::new(state_dir.path().join("orchestrations"));
    let manifests = store.list();
    assert_eq!(manifests.len(), 1, "one orchestration record per run");
    let manifest = &manifests[0];
    assert_eq!(manifest.run_id, out.state.run_id);
    assert_eq!(manifest.parent_session_id, "sess");
    assert_eq!(
        manifest.status,
        xaft_tools::worktrees::OrchestrationStatus::Complete
    );
    assert_eq!(manifest.worktrees.len(), 2, "one worktree per worker");
    assert!(
        manifest
            .tasks
            .iter()
            .all(|t| t.status == xaft_tools::worktrees::TaskStatus::Integrated)
    );
    assert_eq!(
        out.content,
        "Orchestration complete: alpha and beta integrated."
    );
}

/// make_xaft_tool: plan → draft → deterministic validation + smoke test →
/// publication → report; the published tool is what the loader accepts.
#[cfg(unix)]
#[tokio::test]
async fn make_xaft_tool_publishes_a_validated_and_smoke_tested_tool() {
    let h = Harness::new();
    let draft_dir = h.tmp.path().join(".xaft/tools/_drafts");
    std::fs::create_dir_all(&draft_dir).unwrap();
    let draft = draft_dir.join("xaft_e2e_greeter.toml");
    std::fs::write(
        &draft,
        "description = \"Greet someone.\"\ncapability = \"read\"\ncommand = \"echo hello {who}\"\n\n\
         [[params]]\nname = \"who\"\ndescription = \"Who to greet.\"\n",
    )
    .unwrap();
    let draft_text = draft.display().to_string();

    h.call(
        "submit_tool_plan",
        json!({
            "tool_name": "xaft_e2e_greeter",
            "description": "Greet someone.",
            "parameters": [{"name": "who", "description": "Who to greet."}],
            "capability": "read",
            "dependencies": ["echo"],
        }),
    )
    .await;
    h.call(
        "confirm_generation_complete",
        json!({"file_path": ".xaft/tools/_drafts/xaft_e2e_greeter.toml", "summary": "echo template"}),
    )
    .await;
    // validate: approving before the checks is refused by the guard.
    h.transport
        .queue_tool_call("approve_tool", json!({"summary": "too early"}))
        .await;
    h.transport
        .queue_tool_call("validate_custom_tool_draft", json!({"path": draft_text}))
        .await;
    h.transport
        .queue_tool_call(
            "smoke_test_custom_tool",
            json!({"path": draft_text, "args": {"who": "world"}}),
        )
        .await;
    h.call(
        "approve_tool",
        json!({"summary": "validated, smoke test printed hello world"}),
    )
    .await;
    h.text("Created xaft_e2e_greeter: trust it with `xaft trust add`.")
        .await;

    let out = run_phased_workflow(
        &builtin("make_xaft_tool"),
        "a tool that greets someone",
        h.deps.clone(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        history(&out.state),
        ["analyze", "generate", "validate", "finalize"]
    );
    let published = h.tmp.path().join(".xaft/tools/xaft_e2e_greeter.toml");
    assert!(published.is_file());
    assert_eq!(
        xaft_tools::custom_tools::load_tool_file(&published)
            .unwrap()
            .name,
        "xaft_e2e_greeter"
    );
    assert_eq!(
        out.state.data["state"]["published_tool_path"],
        json!(".xaft/tools/xaft_e2e_greeter.toml")
    );
    assert!(out.content.contains("xaft trust add"));
}
