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
            session_approval_filter: None,
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
            subagents: None,
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

#[test]
fn safe_gates_side_effects_from_the_session_or_a_phase_override() {
    fn gated(engine: &Engine<'_>, spec: &PhaseDefinition) -> Vec<String> {
        let mut names: Vec<String> = engine
            .base_tools(spec)
            .iter()
            .filter(|t| t.requires_confirmation())
            .map(|t| t.name().to_string())
            .collect();
        names.sort();
        names
    }
    // Session in Safe: phases that follow the session mode are gated…
    let mut h = Harness::new();
    h.deps.session_approval_filter = Some(Arc::new(|name: &str| {
        !xaft_tools::capabilities::capability_of(name).is_read_only()
    }));
    let wf = workflow("safe", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());
    assert_eq!(
        gated(&engine, &phase("x", None)),
        ["bash_exec", "write_file"]
    );
    // …but a phase override replaces the session mode.
    let mut yolo = phase("x", None);
    yolo.mode_override = Some("yolo".into());
    assert!(gated(&engine, &yolo).is_empty());

    // A `safe` phase override gates on its own, whatever the session mode.
    let h = Harness::new();
    let engine = Engine::new(&wf, h.deps.clone());
    let mut safe = phase("x", None);
    safe.mode_override = Some("safe".into());
    assert_eq!(gated(&engine, &safe), ["bash_exec", "write_file"]);
    assert_eq!(
        tool_names(&engine, &safe),
        ["bash_exec", "git_status", "read_file", "write_file"],
        "Safe asks rather than hiding tools"
    );
}

// ── Runner knobs ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn max_attempts_bounds_continuations_without_capping_entries() {
    let h = Harness::new();
    // Three turns are queued; max_attempts = 2 consumes only two of them.
    for t in ["thinking", "still thinking", "never used"] {
        h.text(t).await;
    }
    let mut work = phase("work", None);
    work.max_attempts = Some(2);
    work.transitions = vec![TransitionDefinition {
        tool: "finish".into(),
        description: "Done.".into(),
        ..Default::default()
    }];
    assert_eq!(work.max_iterations, None, "entries stay uncapped");
    let wf = workflow("attempts", vec![work]);
    let err = run_phased_workflow(&wf, "t", h.deps.clone(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("after 2 turn(s)"), "{err}");
}

#[test]
fn a_stable_contract_leads_every_phase_system_prompt() {
    let h = Harness::new();
    let mut wf = workflow("contract", vec![phase("a", Some("b")), phase("b", None)]);
    wf.definition.stable_system_prompt = "[CONTRACT]\nNever guess.".into();
    let engine = Engine::new(&wf, h.deps.clone());
    let state = WorkflowRunState::new("r", "contract", "t", "s", "/w", "fp");
    let data = RunData::default();
    for spec in &wf.definition.phases {
        let tctx = TemplateContext {
            state: &state,
            data: &data,
            phase: &spec.name,
            iteration: 1,
        };
        let prompt = engine.system_prompt(spec, &[], &[], &tctx);
        assert!(prompt.starts_with("[CONTRACT]\nNever guess."), "{prompt}");
    }
}

// ── Built-in site workflows, end to end ─────────────────────────────────────

/// One LLM call as the scripted model saw it.
#[derive(Debug, Clone, Default)]
struct Seen {
    system: String,
    user: String,
    tools: Vec<String>,
}

type Respond = Box<dyn Fn(&Seen) -> (String, Value) + Send + Sync>;

enum Step {
    Text(String),
    Call(String, Value),
    /// A tool call computed from what the model was shown.
    Dynamic(Respond),
}

/// A model script over `MockTransport` that records every call, and can
/// answer from the prompt it was given (as a real model reads an id from it).
struct ScriptedLlm {
    inner: MockLlmProvider,
    transport: Arc<MockTransport>,
    steps: StdMutex<std::collections::VecDeque<Step>>,
    seen: StdMutex<Vec<Seen>>,
}

impl ScriptedLlm {
    fn new() -> Arc<Self> {
        let transport = Arc::new(MockTransport::new());
        Arc::new(Self {
            inner: MockLlmProvider::new(Arc::clone(&transport)),
            transport,
            steps: StdMutex::new(Default::default()),
            seen: StdMutex::new(Vec::new()),
        })
    }

    /// A transition call followed by the agent's closing text.
    fn call(&self, tool: &str, input: Value) {
        let mut steps = self.steps.lock().unwrap();
        steps.push_back(Step::Call(tool.into(), input));
        steps.push_back(Step::Text("ok".into()));
    }

    /// A tool call the agent follows with another call.
    fn tool(&self, tool: &str, input: Value) {
        self.steps
            .lock()
            .unwrap()
            .push_back(Step::Call(tool.into(), input));
    }

    fn text(&self, text: &str) {
        self.steps
            .lock()
            .unwrap()
            .push_back(Step::Text(text.into()));
    }

    fn dynamic(&self, respond: Respond) {
        let mut steps = self.steps.lock().unwrap();
        steps.push_back(Step::Dynamic(respond));
        steps.push_back(Step::Text("ok".into()));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn remaining(&self) -> usize {
        self.steps.lock().unwrap().len()
    }

    async fn prepare(
        &self,
        messages: &[agtrs_runtime::transport::Message],
        options: &agtrs_runtime::llm::LlmOptions,
    ) {
        use agtrs_runtime::transport::Role;
        let seen = Seen {
            system: messages
                .iter()
                .filter(|m| matches!(m.role, Role::System))
                .map(|m| m.text())
                .collect::<Vec<_>>()
                .join("\n"),
            user: messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.text())
                .unwrap_or_default(),
            tools: options.tools.iter().map(|t| t.name.clone()).collect(),
        };
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            Some(Step::Text(t)) => self.transport.queue_text(t).await,
            Some(Step::Call(name, input)) => self.transport.queue_tool_call(&name, input).await,
            Some(Step::Dynamic(f)) => {
                let (name, input) = f(&seen);
                self.transport.queue_tool_call(&name, input).await;
            }
            None => {}
        }
        self.seen.lock().unwrap().push(seen);
    }
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(
        &self,
        messages: &[agtrs_runtime::transport::Message],
        options: &agtrs_runtime::llm::LlmOptions,
    ) -> Result<agtrs_runtime::llm::LlmResponse, AgtrsError> {
        self.prepare(messages, options).await;
        self.inner.complete(messages, options).await
    }

    async fn stream(
        &self,
        messages: &[agtrs_runtime::transport::Message],
        options: &agtrs_runtime::llm::LlmOptions,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Result<agtrs_runtime::llm::StreamChunk, AgtrsError>>
                    + Send,
            >,
        >,
        AgtrsError,
    > {
        self.prepare(messages, options).await;
        self.inner.stream(messages, options).await
    }

    async fn embed(
        &self,
        inputs: &[String],
        model: Option<&str>,
    ) -> Result<Vec<agtrs_runtime::transport::Embedding>, AgtrsError> {
        self.inner.embed(inputs, model).await
    }

    async fn count_tokens(
        &self,
        messages: &[agtrs_runtime::transport::Message],
    ) -> Result<usize, AgtrsError> {
        self.inner.count_tokens(messages).await
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn supports_tool_calling(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
}

fn scripted() -> (Harness, Arc<ScriptedLlm>) {
    let mut h = Harness::new();
    let llm = ScriptedLlm::new();
    h.deps.llm = Arc::clone(&llm) as Arc<dyn LlmProvider>;
    (h, llm)
}

fn builtin_workflow(name: &str) -> PhasedWorkflow {
    let definition = xaft_config::workflows::builtin::builtin_phased_definitions()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap()
        .1;
    PhasedWorkflow {
        name: name.into(),
        definition,
    }
}

/// The calls made while `phase` ran, in order.
fn calls_in<'a>(seen: &'a [Seen], phase: &str) -> Vec<&'a Seen> {
    let marker = format!("You are running phase '{phase}'");
    seen.iter().filter(|s| s.system.contains(&marker)).collect()
}

#[tokio::test]
async fn copy_website_studies_builds_and_loops_back_on_a_parity_rejection() {
    let (h, llm) = scripted();
    llm.call(
        "submit_target",
        json!({"url": "https://ref.test", "scope": "", "pages": "/,/pricing"}),
    );
    llm.call(
        "submit_study_report",
        json!({"report": "airy layout", "pages": "", "screenshots": "a.png"}),
    );
    llm.call(
        "submit_design_spec",
        json!({"spec": "two pages", "tokens": "blue", "components": "Card", "data_plan": "none"}),
    );
    llm.call("submit_scaffold", json!({"project_path": "web"}));
    let build_round = |llm: &ScriptedLlm| {
        llm.call("submit_layout", json!({"files": "app/layout.tsx"}));
        llm.call("submit_pages", json!({"pages_built": "/,/pricing"}));
        llm.call("submit_data_layer", json!({"hooks": "useProducts"}));
        llm.call("submit_responsive", json!({"verified": "390px ok"}));
    };
    build_round(&llm);
    llm.call("reject_parity", json!({"reason": "hero spacing is off"}));
    build_round(&llm);
    llm.call(
        "approve_parity",
        json!({"summary": "matches at both viewports"}),
    );
    llm.text("Final report: rebuilt https://ref.test in web/");

    let wf = builtin_workflow("copy_website");
    let out = run_phased_workflow(&wf, "https://ref.test", h.deps.clone(), None)
        .await
        .unwrap();

    let round = [
        "implement_layout",
        "implement_pages",
        "implement_data",
        "responsive_pass",
        "parity_verify",
    ];
    let mut expected = vec!["extract_target", "site_study", "design_spec", "scaffold"];
    expected.extend(round);
    expected.extend(round);
    expected.push("final_report");
    assert_eq!(history(&out.state), expected);
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(
        out.content,
        "Final report: rebuilt https://ref.test in web/"
    );
    assert_eq!(llm.remaining(), 0);

    let seen = llm.seen();
    let layout = calls_in(&seen, "implement_layout");
    assert!(
        !layout[0]
            .system
            .contains("Parity verification was rejected")
    );
    let second = layout
        .iter()
        .find(|s| {
            s.system
                .contains("Parity verification was rejected (iteration 1): hero spacing is off")
        })
        .expect("the fix context reaches the second layout pass");
    assert!(
        second.system.contains("[CACHE-STABLE WORKFLOW POLICY]"),
        "the stable contract leads every phase"
    );
    let pages = calls_in(&seen, "implement_pages");
    assert!(
        pages[0].user.contains("Pages to build: /, /pricing"),
        "{}",
        pages[0].user
    );
    assert!(
        calls_in(&seen, "site_study")[0]
            .user
            .contains("Study the target website now: https://ref.test")
    );
}

#[tokio::test]
async fn site_imitate_builds_and_verifies_every_planned_component() {
    let (h, llm) = scripted();
    llm.call("submit_analysis", json!({"analysis": "a sticky header and a footer", "components": "three lines -> Hamburger Menu (Sheet)"}));
    llm.call("submit_plan", json!({"plan": "Next.js site", "components": ["Header | sticky bar | header is sticky", "Footer | links | footer renders"]}));
    llm.call("scaffold_complete", json!({"path": "bakery"}));
    // Component 1: the verification summary must evidence mobile checks.
    llm.call("component_built", json!({}));
    llm.tool("component_verified", json!({"summary": "tsc passes"}));
    llm.call(
        "component_verified",
        json!({"summary": "tsc passes; mobile 375px has no overflow"}),
    );
    // Component 2 fails once, then passes.
    llm.call("component_built", json!({}));
    llm.call(
        "component_verification_failed",
        json!({"errors": "missing aria-label"}),
    );
    llm.call("component_built", json!({}));
    llm.call(
        "component_verified",
        json!({"summary": "responsive at all widths"}),
    );
    // The site build fails: back to the last component.
    llm.call(
        "final_verify_failed",
        json!({"errors": "type error in Footer"}),
    );
    llm.call("component_built", json!({}));
    llm.call(
        "component_verified",
        json!({"summary": "viewport checks pass"}),
    );
    llm.call(
        "final_verify_passed",
        json!({"summary": "next build ok; responsive nav works"}),
    );

    let wf = builtin_workflow("site_imitate");
    let out = run_phased_workflow(
        &wf,
        "https://ref.test | a bakery storefront",
        h.deps.clone(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "analyze",
            "plan",
            "scaffold",
            "build",
            "verify",
            "build",
            "verify",
            "build",
            "verify",
            "final_verify",
            "build",
            "verify",
            "final_verify"
        ]
    );
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(llm.remaining(), 0);

    let seen = llm.seen();
    let analyze = calls_in(&seen, "analyze");
    assert!(
        analyze[0]
            .user
            .contains("Target URL: https://ref.test\nNew purpose: a bakery storefront"),
        "{}",
        analyze[0].user
    );
    assert!(analyze[0].tools.iter().any(|t| t == "lookup_component"));
    assert!(analyze[0].tools.iter().any(|t| t == "list_components"));
    assert!(
        analyze[0]
            .system
            .contains("[MOBILE-FIRST RESPONSIVE SITE CONTRACT]")
    );
    let builds = calls_in(&seen, "build");
    assert!(
        builds[0].system.contains("'Header' (component 1 of 2)"),
        "{}",
        builds[0].system
    );
    assert!(builds[0].tools.iter().any(|t| t == "lookup_component"));
    assert!(
        !builds[0].tools.iter().any(|t| t == "list_components"),
        "build only looks components up"
    );
    assert!(
        builds
            .iter()
            .any(|s| s.system.contains("'Footer' (component 2 of 2)"))
    );
    assert!(
        builds
            .last()
            .unwrap()
            .user
            .contains("NOW BUILDING COMPONENT 2/2 - Footer")
    );
    let plan = calls_in(&seen, "plan");
    assert!(
        plan[0]
            .user
            .contains("three lines -> Hamburger Menu (Sheet)")
    );
    assert_eq!(out.state.data["site_imitate"]["component_index"], 2);
}

fn ux_discovery(scope: &str) -> Value {
    json!({
        "summary": "A checkout flow for a web shop",
        "scope": scope,
        "acceptance_criteria": ["Checkout works on mobile", "Errors are announced"],
    })
}

#[tokio::test]
async fn ui_ux_pro_max_runs_the_targeted_route_with_bounded_remediation() {
    let (h, llm) = scripted();
    llm.call("complete_product_discovery", ux_discovery("targeted"));
    llm.call(
        "submit_design_research",
        json!({"summary": "contrast research", "queries": ["checkout contrast"]}),
    );
    llm.call(
        "submit_implementation_guide",
        json!({"summary": "React guide", "detected_stack": "react"}),
    );
    // Submitting before approval is refused; approval opens the guard.
    llm.tool(
        "submit_experience_architecture",
        json!({"summary": "screens"}),
    );
    llm.tool(
        "request_implementation_approval",
        json!({"plan": "1. checkout screen"}),
    );
    llm.call(
        "submit_experience_architecture",
        json!({"summary": "screens", "implementation_tasks": ["checkout"]}),
    );
    llm.call(
        "complete_design_implementation",
        json!({"summary": "built", "modified_files": ["Checkout.tsx"]}),
    );
    llm.call("submit_visual_ux_audit", json!({"summary": "one issue", "findings": ["High | button | e | x | y | fix | check"], "visual_inspection_status": "unavailable"}));
    llm.call(
        "complete_design_remediation",
        json!({"summary": "fixed", "resolved_findings": ["button"]}),
    );
    llm.call(
        "submit_visual_ux_audit",
        json!({"summary": "clean", "visual_inspection_status": "unavailable"}),
    );
    llm.call(
        "complete_design_acceptance",
        json!({"summary": "accepted", "criterion_results": [
            "PASS | Checkout works on mobile | e2e on 375px",
            "NOT_APPLICABLE | Errors are announced | no error states in scope"
        ]}),
    );
    llm.call(
        "complete_design_delivery",
        json!({"summary": "Checkout redesigned; all criteria accepted."}),
    );

    let wf = builtin_workflow("ui_ux_pro_max");
    let out = run_phased_workflow(&wf, "Improve the mobile checkout", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "product_discovery",
            "targeted_design_research",
            "stack_adaptation",
            "experience_architecture",
            "design_implementation",
            "visual_ux_audit",
            "design_remediation",
            "visual_ux_audit",
            "design_acceptance",
            "delivery"
        ]
    );
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(llm.remaining(), 0);
    let seen = llm.seen();
    assert_eq!(
        calls_in(&seen, "product_discovery")[0].user,
        "Improve the mobile checkout"
    );
    let remediation = calls_in(&seen, "design_remediation");
    assert!(
        remediation[0]
            .user
            .starts_with("[DYNAMIC WORKFLOW CONTEXT: DESIGN_REMEDIATION]\n{"),
        "{}",
        remediation[0].user
    );
    assert!(
        remediation[0]
            .user
            .contains("\"request\":\"Improve the mobile checkout\"")
    );
    assert!(
        calls_in(&seen, "experience_architecture")[0]
            .tools
            .iter()
            .any(|t| t == "request_implementation_approval")
    );
    assert_eq!(out.state.data["ui_ux_pro_max"]["remediation_cycles"], 1);
}

#[tokio::test]
async fn ui_ux_pro_max_blocks_delivery_while_a_criterion_is_unverified() {
    let (h, llm) = scripted();
    llm.call(
        "complete_product_discovery",
        ux_discovery("accessibility_audit"),
    );
    llm.call(
        "submit_design_research",
        json!({"summary": "aria research", "queries": ["form errors"]}),
    );
    llm.call(
        "submit_implementation_guide",
        json!({"summary": "guide", "detected_stack": "vue"}),
    );
    llm.call("submit_visual_ux_audit", json!({"summary": "audit", "findings": ["Medium | form | e | x | y | fix | check"], "visual_inspection_status": "not_required"}));
    llm.call(
        "complete_design_acceptance",
        json!({"summary": "partial", "criterion_results": [
            "PASS | Checkout works on mobile | keyboard walkthrough",
            "UNVERIFIED | Errors are announced | no screen reader available"
        ]}),
    );
    llm.tool(
        "complete_design_delivery",
        json!({"summary": "Audit delivered; one criterion unverified."}),
    );
    llm.text("The audit is partial: error announcements could not be verified.");

    let wf = builtin_workflow("ui_ux_pro_max");
    let out = run_phased_workflow(
        &wf,
        "Audit the checkout's accessibility",
        h.deps.clone(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "product_discovery",
            "targeted_design_research",
            "stack_adaptation",
            "visual_ux_audit",
            "design_acceptance",
            "delivery"
        ],
        "an audit never implements or remediates"
    );
    assert_eq!(
        out.state.status,
        RunStatus::Exited,
        "agenthicc's terminal BLOCKED"
    );
    assert_eq!(
        out.content,
        "The audit is partial: error announcements could not be verified."
    );
}

#[tokio::test]
async fn reconstruct_site_runs_the_static_profile_through_the_gate_and_a_reentry() {
    let (h, llm) = scripted();
    llm.call(
        "submit_initial_state",
        json!({
            "reference_url": "https://ref.test/",
            "target_directory": "site",
            "desired_routes": "/, /about",
            "reference_is_static": true
        }),
    );
    for (tool, summary) in [
        ("submit_route_inventory", "two routes"),
        ("submit_visual_spec", "measured"),
        ("submit_interaction_inventory", "menus"),
        ("submit_asset_inventory", "logo"),
        ("submit_responsive_research", "3 breakpoints"),
        ("submit_architecture", "app router"),
        ("submit_design_system", "tokens"),
    ] {
        llm.call(tool, json!({ "summary": summary }));
    }
    // A stale baseline id is refused; the current one is read from the
    // [EVIDENCE DIGEST], as a model would.
    llm.tool(
        "approve_research_baseline",
        json!({"summary": "complete", "baseline_artifact_id": "stale"}),
    );
    llm.dynamic(Box::new(|seen: &Seen| {
        let id = seen
            .system
            .split("Baseline: ")
            .nth(1)
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_hexdigit)
                    .collect::<String>()
            })
            .unwrap_or_default();
        assert_eq!(
            id.len(),
            64,
            "the digest names the baseline: {}",
            seen.system
        );
        (
            "approve_research_baseline".into(),
            json!({"summary": "complete", "baseline_artifact_id": id}),
        )
    }));
    llm.call("confirm_bootstrap_healthy", json!({"summary": "builds"}));
    llm.call("confirm_global_shell", json!({"summary": "shell"}));
    llm.call("confirm_component_system", json!({"summary": "library"}));
    llm.call(
        "complete_page",
        json!({"page_route": "/", "summary": "home"}),
    );
    llm.call(
        "complete_page",
        json!({"page_route": "/about", "summary": "about"}),
    );
    llm.call("confirm_responsive", json!({"summary": "breakpoints ok"}));
    llm.call("visual_rejected", json!({"discrepancies": [{"page": "/", "issue": "card radius"}], "target_phase": "component_system"}));
    llm.call(
        "confirm_component_system",
        json!({"summary": "radius fixed"}),
    );
    llm.call("confirm_responsive", json!({"summary": "still ok"}));
    for tool in [
        "visual_approved",
        "interaction_approved",
        "a11y_approved",
        "perf_approved",
        "fidelity_approved",
    ] {
        llm.call(tool, json!({"summary": "ok"}));
    }
    llm.call(
        "final_approved",
        json!({"summary": "typecheck, lint, build pass"}),
    );

    let wf = builtin_workflow("reconstruct_site");
    let out = run_phased_workflow(&wf, "Rebuild https://ref.test", h.deps.clone(), None)
        .await
        .unwrap();

    assert_eq!(
        history(&out.state),
        [
            "init",
            "recon",
            "visual_research",
            "interaction_analysis",
            "content_assets",
            "responsive_research",
            "architecture",
            "design_system",
            "research_gate",
            "bootstrap",
            "global_shell",
            "component_system",
            "page",
            "page",
            "responsive_pass",
            "visual_validation",
            "component_system",
            "responsive_pass",
            "visual_validation",
            "interaction_validation",
            "accessibility",
            "performance",
            "fidelity_pass",
            "final_validation"
        ],
        "static: no data layer, no infrastructure; the re-entry skips the finished pages"
    );
    assert_eq!(out.state.status, RunStatus::Complete);
    assert_eq!(llm.remaining(), 0);

    let rec = &out.state.data["reconstruct_site"];
    assert_eq!(rec["profile"], "static");
    assert_eq!(rec["research_gate_status"], "approved");
    assert_eq!(rec["reentry_count"], 1);
    assert_eq!(
        rec["browser_capability_status"], "unavailable",
        "the harness offers no browser tools"
    );
    let manifest_path = h
        .tmp
        .path()
        .join(".xaft/reconstruct_site")
        .join(&out.state.run_id)
        .join("manifest.json");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["status"], "complete");
    assert_eq!(
        manifest["reentry_history"][0]["target_phase"],
        "component_system"
    );

    let seen = llm.seen();
    let pages = calls_in(&seen, "page");
    assert!(
        pages[0]
            .user
            .starts_with("Implement route / (page 1 of 2)."),
        "{}",
        pages[0].user
    );
    assert!(
        pages
            .iter()
            .any(|s| s.user.starts_with("Implement route /about (page 2 of 2)."))
    );
    assert!(
        calls_in(&seen, "recon")[0]
            .system
            .contains("`site/research/reconnaissance/route-inventory.md`")
    );
    assert!(
        calls_in(&seen, "research_gate")[0]
            .user
            .contains("\"mode\":\"summary_only\"")
    );
}

// ── Session tools in phases: MCP and subagents ──────────────────────────────

fn with_subagents(h: &mut Harness) {
    h.deps.subagents = Some(crate::subagents::SubagentServices {
        llm: Arc::clone(&h.deps.llm),
        signals: Arc::clone(&h.deps.signals),
        resolve_ctx: Arc::clone(&h.deps.resolve_ctx),
        approval_gate: None,
        session_id: "sess".into(),
        parent_run_id: "run".into(),
        working_dir: h.deps.working_dir.clone(),
        journal: None,
        continuations: Arc::default(),
        registry: Arc::new(crate::subagents::SubagentTypeRegistry::builtin()),
        max_concurrent: 4,
    });
}

fn sorted_names(tools: &[Arc<ErasedTool>]) -> Vec<String> {
    let mut names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    names.sort();
    names
}

#[test]
fn every_agent_phase_gets_spawn_subagents_unless_its_mode_or_list_excludes_it() {
    let mut h = Harness::new();
    with_subagents(&mut h);
    let wf = workflow("subagents", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());
    assert_eq!(
        sorted_names(&engine.subagent_tools(&phase("x", None))),
        [
            "answer_subagent",
            "collect_subagent_results",
            "send_subagent_message",
            "spawn_subagents"
        ]
    );
    // A capability allowlist does not hide it: it grants no capability, and
    // its workers draw only on the phase's own (filtered) tools.
    let mut reviewer = phase("x", None);
    reviewer.agent_type = "reviewer".into();
    assert_eq!(engine.subagent_tools(&reviewer).len(), 4);
    let mut plan = phase("x", None);
    plan.mode_override = Some("plan".into());
    assert!(
        engine.subagent_tools(&plan).is_empty(),
        "Plan blocks the undeclared spawn"
    );
    let mut listed = phase("x", None);
    listed.tools = Some(vec!["read_file".into()]);
    assert!(engine.subagent_tools(&listed).is_empty());
    listed.tools = Some(vec!["read_file".into(), "spawn_subagents".into()]);
    assert_eq!(engine.subagent_tools(&listed).len(), 4);
    let mut safe = phase("x", None);
    safe.mode_override = Some("safe".into());
    let gated: Vec<String> = engine
        .subagent_tools(&safe)
        .iter()
        .filter(|t| t.requires_confirmation())
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(
        gated,
        ["spawn_subagents"],
        "Safe asks before a spawn, not a message"
    );

    let bare = Harness::new();
    let engine = Engine::new(&wf, bare.deps.clone());
    assert!(engine.subagent_tools(&phase("x", None)).is_empty());
}

#[test]
fn a_session_plan_filter_hides_spawn_subagents_from_phases_that_follow_it() {
    let mut h = Harness::new();
    with_subagents(&mut h);
    h.deps.session_tool_filter = Some(Arc::new(|name: &str| {
        !xaft_tools::capabilities::capability_of(name).is_restricted()
    }));
    let wf = workflow("subagents", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());
    assert!(engine.subagent_tools(&phase("x", None)).is_empty());
    let mut auto = phase("x", None);
    auto.mode_override = Some("auto".into());
    assert_eq!(
        engine.subagent_tools(&auto).len(),
        4,
        "a phase override replaces the session mode"
    );
}

#[test]
fn mcp_tools_reach_only_phases_that_allow_network() {
    let mut h = Harness::new();
    h.deps
        .write_tools
        .push(FakeTool::erased("mcp__docs__search"));
    let wf = workflow("mcp", vec![phase("x", None)]);
    let engine = Engine::new(&wf, h.deps.clone());
    assert!(tool_names(&engine, &phase("x", None)).contains(&"mcp__docs__search".to_string()));
    let mut reviewer = phase("x", None);
    reviewer.agent_type = "reviewer".into();
    assert!(!tool_names(&engine, &reviewer).contains(&"mcp__docs__search".to_string()));
    let mut network = phase("x", None);
    network.allowed_capabilities = Some(vec!["read".into(), "network".into()]);
    assert_eq!(
        tool_names(&engine, &network),
        ["mcp__docs__search", "read_file"]
    );
    let mut plan = phase("x", None);
    plan.mode_override = Some("plan".into());
    assert!(!tool_names(&engine, &plan).contains(&"mcp__docs__search".to_string()));
}
