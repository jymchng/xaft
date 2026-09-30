//! End-to-end tests for the two multi-agent pipelines in
//! `xaft_runtime::orchestrator`: the unified four-agent workflow
//! (`run_workflow`) and the meta-agent pipeline (`run_meta_workflow`).
//!
//! `handoff_e2e.rs` already covers `run_dynamic_handoff` with two custom
//! agents. What it does not drive is either *built-in* pipeline, which means
//! the parts that only those two functions contain — the mode-patch prompt
//! prepending, the four handoff prompt templates, the QA verdict routing, and
//! the entire meta agent wiring — have never been executed.
//!
//! These tests drive them through `MockLlmProvider`, so no API calls are made.
//! Where a behaviour is invisible from the return value (what the agents were
//! actually *told*), the tests use [`RecordingLlm`], which captures the message
//! list every call receives. Asserting on the prompts is the point: a handoff
//! template that silently stopped interpolating the task, or a mode patch that
//! stopped reaching the coder, would leave every other test in the file green.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::{LlmOptions, LlmProvider, LlmResponse, StreamChunk};
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::transport::{Embedding, Message, MessageContent, ToolCall};
use tempfile::TempDir;
use xaft_runtime::agent_registry::{AgentRegistry, WorkflowConfig};
use xaft_runtime::orchestrator::{run_dynamic_handoff, run_meta_workflow, run_workflow};
use xaft_runtime::session::AgentSession;
use xaft_runtime::{MetaWorkflowConfig, types::ExitCode};

// ── Scripted response ───────────────────────────────────────────────────────

/// One scripted LLM reply: either plain text, or a single tool call.
enum Scripted {
    Text(String),
    ToolCall {
        name: &'static str,
        input: serde_json::Value,
    },
}

impl Scripted {
    fn text(s: impl Into<String>) -> Self {
        Scripted::Text(s.into())
    }

    fn tool(name: &'static str, input: serde_json::Value) -> Self {
        Scripted::ToolCall { name, input }
    }
}

impl From<&str> for Scripted {
    fn from(s: &str) -> Self {
        Scripted::Text(s.to_string())
    }
}

/// An [`LlmProvider`] that wraps [`MockLlmProvider`], replays a script of
/// responses in order, and records every message list it is handed.
///
/// It delegates to the mock rather than reimplementing `LlmProvider` so that
/// the response shapes (tool-call encoding, usage, stop reason) are exactly the
/// ones the production agents already cope with.
struct RecordingLlm {
    inner: Arc<MockLlmProvider>,
    script: Mutex<std::collections::VecDeque<Scripted>>,
    seen: Mutex<Vec<Vec<String>>>,
}

impl RecordingLlm {
    fn new(script: Vec<Scripted>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(MockLlmProvider::new(Arc::new(MockTransport::new()))),
            script: Mutex::new(script.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Every prompt the agents were given, flattened to plain strings.
    fn prompts(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// True if any recorded prompt contains `needle`.
    fn saw(&self, needle: &str) -> bool {
        self.prompts().iter().any(|p| p.contains(needle))
    }
}

/// Best-effort plain-text rendering of a message, for prompt assertions.
fn render(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::MultiPart(blocks) => blocks
            .iter()
            .map(|b| format!("{b:?}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[async_trait::async_trait]
impl LlmProvider for RecordingLlm {
    async fn complete(
        &self,
        messages: &[Message],
        options: &LlmOptions,
    ) -> Result<LlmResponse, AgtrsError> {
        self.seen
            .lock()
            .unwrap()
            .push(messages.iter().map(render).collect());

        let next = self.script.lock().unwrap().pop_front();
        let response = match next {
            Some(Scripted::Text(t)) => LlmResponse {
                message: Message::assistant(t),
                usage: agtrs_runtime::transport::TokenUsage::new(1, 1),
                tool_calls: vec![],
                finish_reason: agtrs_runtime::transport::StopReason::EndTurn,
                thinking_blocks: vec![],
            },
            Some(Scripted::ToolCall { name, input }) => {
                let call = ToolCall {
                    tool_use_id: uuid::Uuid::new_v4().to_string(),
                    name: name.to_string(),
                    input,
                };
                LlmResponse {
                    message: Message::assistant(""),
                    usage: agtrs_runtime::transport::TokenUsage::new(1, 1),
                    tool_calls: vec![call.clone()],
                    finish_reason: agtrs_runtime::transport::StopReason::ToolUse,
                    thinking_blocks: vec![],
                }
            }
            None => {
                return Err(AgtrsError::LlmCallFailed {
                    reason: "script exhausted".to_string(),
                });
            }
        };

        // Route through the mock so the tool-call encoding matches production.
        let _ = options;
        if response.tool_calls.is_empty() {
            self.inner
                .transport()
                .queue_text(extract_text(&response.message))
                .await;
        } else {
            let call = response.tool_calls[0].clone();
            self.inner
                .transport()
                .queue_tool_call(&call.name, call.input.clone())
                .await;
        }
        self.inner.complete(messages, options).await
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: &LlmOptions,
    ) -> Result<
        Pin<Box<dyn futures::Stream<Item = Result<StreamChunk, AgtrsError>> + Send>>,
        AgtrsError,
    > {
        Err(AgtrsError::LlmCallFailed {
            reason: "RecordingLlm does not stream".to_string(),
        })
    }

    async fn embed(
        &self,
        _inputs: &[String],
        _model: Option<&str>,
    ) -> Result<Vec<Embedding>, AgtrsError> {
        Ok(vec![])
    }

    async fn count_tokens(&self, _messages: &[Message]) -> Result<usize, AgtrsError> {
        Ok(0)
    }

    fn context_window(&self) -> usize {
        8192
    }

    fn model(&self) -> &str {
        "recording-mock"
    }
}

fn extract_text(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::MultiPart(_) => String::new(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_ctx() -> Arc<injectable_rs_runtime::ResolveContext> {
    Arc::new(injectable_rs_runtime::ResolveContext::from_store(Arc::new(
        injectable_rs_runtime::EmptySingletonStore,
    )))
}

fn session(dir: &TempDir) -> AgentSession {
    AgentSession::new(
        "add a greeting".to_string(),
        dir.path().to_path_buf(),
        "default".to_string(),
        "mock-model".to_string(),
    )
}

fn handoff(target: &str) -> Scripted {
    Scripted::tool(
        "handoff_to_agent",
        serde_json::json!({"target_agent": target, "reason": "next stage"}),
    )
}

fn request_fix(summary: &str) -> Scripted {
    Scripted::tool("request_fix", serde_json::json!({"summary": summary}))
}

// ── run_workflow: the four-agent planner → coder → QA ↔ fixer pipeline ───────
//
// SCRIPTING NOTE, learned the hard way: an agent that fires a handoff tool does
// not stop immediately — the target agent then runs its OWN turns, so a
// planner → coder → QA → fixer → QA run costs NINE orchestrator turns, not six.
// `run_workflow` makes one further LLM call afterwards to build the concluding
// summary, so a run that must end on a given verdict needs the verdict at
// position 9 and a summary at position 10. Each test below says which turn it is
// asserting on.

/// The coding path must render an `APPROVED` verdict, and the handoff
/// templates must actually reach the agents they address.
///
/// This is the only test that walks planner → coder → QA → fixer → QA, so it is
/// also the only one that exercises all four arms of the orchestrator's
/// `prompt_fn`. Each arm interpolates different fields, and an arm that
/// interpolated the wrong one would produce a plausible-looking run — hence the
/// prompt assertions rather than just the exit code.
#[tokio::test]
async fn coding_path_renders_approved_verdict_and_addresses_each_agent() {
    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![
        handoff("coder"),
        Scripted::text("Wrote the greeting into src/lib.rs"),
        handoff("qa"),
        Scripted::text("Reviewing the change"),
        request_fix("missing newline at end of file"),
        Scripted::text("Added the trailing newline"),
        handoff("qa"),
        Scripted::text("Re-checking the fix"),
        Scripted::text("APPROVED - the change is correct"),
        Scripted::text("Added a greeting to the library."),
    ]);

    let mut s = session(&tmp);
    let (content, code) = run_workflow(
        "add a greeting",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        Some("MODE PATCH: answer with a plan only."),
    )
    .await
    .expect("the four-agent pipeline should complete");

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        content.starts_with("\u{2713} QA approved"),
        "approved run must render the check verdict, got: {content}"
    );

    // The coder template says "PLAN — execute these steps in order" and carries
    // the planner's summary plus the original task.
    assert!(
        llm.saw("execute these steps in order"),
        "coder handoff template never reached the coder"
    );
    // The QA template tells it to use list_files + read_file and carries the task.
    assert!(
        llm.saw("Use list_files + read_file to inspect"),
        "qa handoff template never reached QA"
    );
    // The fixer template names the review findings and tells it to hand back.
    assert!(
        llm.saw("Call handoff_to_agent(\"qa\", ...) when done"),
        "fixer handoff template never reached the fixer"
    );
    // Each of those templates must carry the task text through, not drop it.
    assert!(
        llm.saw("add a greeting"),
        "handoff templates must interpolate the original task"
    );
    assert!(
        llm.saw("missing newline at end of file"),
        "the fixer's handoff must carry QA's findings verbatim"
    );

    assert!(s.turn_count > 0, "session turn_count must be updated");
}

/// A QA verdict that never says APPROVED must be reported as incomplete, not
/// silently rendered as success.
///
/// Without this the `⚠` branch of `qa_verdict` is dead code: the approved string
/// contains "APPROVED" by construction, so only a run whose final QA message
/// omits the keyword can reach the other branch.
#[tokio::test]
async fn qa_that_never_approves_is_reported_incomplete() {
    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![
        handoff("coder"),
        Scripted::text("Attempted the change"),
        handoff("qa"),
        Scripted::text("I could not verify that the change was applied."),
        Scripted::text("The change could not be verified."),
    ]);

    let mut s = session(&tmp);
    let (content, code) = run_workflow(
        "add a greeting",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("the pipeline should still complete");

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        content.starts_with("\u{26a0} QA incomplete"),
        "a run QA never approved must be flagged incomplete, got: {content}"
    );
    assert!(
        !content.contains("QA approved"),
        "the approved verdict must not appear when QA withheld approval"
    );
}

/// The mode patch (PRD 64) must be prepended to *every* agent's system prompt,
/// ahead of that agent's own prompt — not appended, and not applied to only the
/// first agent built.
///
/// A patch that leaked into the QA prompt but not the fixer prompt would let the
/// fixer edit files in plan mode, which is the whole failure this guards.
#[tokio::test]
async fn mode_patch_is_prepended_to_every_agents_system_prompt() {
    const PATCH: &str = "MODE PATCH: plan only, do not edit.";

    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![
        handoff("coder"),
        Scripted::text("Planned the greeting"),
        handoff("qa"),
        Scripted::text("Reviewed"),
        request_fix("needs work"),
        Scripted::text("Fixed"),
        handoff("qa"),
        Scripted::text("Re-checking"),
        Scripted::text("APPROVED"),
        Scripted::text("Summary text."),
    ]);

    let mut s = session(&tmp);
    run_workflow(
        "add a greeting",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        Some(PATCH),
    )
    .await
    .expect("pipeline completes");

    let all_prompts = llm.prompts();
    let patch_prompts: Vec<&String> = all_prompts
        .iter()
        .filter(|p| p.starts_with(PATCH))
        .collect();

    // The planner, coder, QA and fixer each got their own prompt built with the
    // patch; more than one agent must have run with it.
    assert!(
        patch_prompts.len() >= 4,
        "expected the patch on all four agents' prompts, saw it on {}",
        patch_prompts.len()
    );
    for prompt in &patch_prompts {
        assert!(
            prompt.starts_with(PATCH),
            "the patch must come FIRST, before the agent's own prompt"
        );
        assert!(
            prompt.len() > PATCH.len() + 2 && prompt[PATCH.len()..].starts_with("\n\n"),
            "the agent's own prompt must follow the patch after a blank line, got: {:?}",
            &prompt[..prompt.len().min(80)]
        );
    }
}

/// An empty or whitespace-only patch must be ignored rather than prepended as a
/// blank line, which would otherwise silently displace every agent prompt.
#[tokio::test]
async fn a_blank_mode_patch_leaves_the_agent_prompt_untouched() {
    for patch in [None, Some(""), Some("   \n\t ")] {
        let tmp = TempDir::new().unwrap();
        let llm = RecordingLlm::new(vec![Scripted::text("No code change needed.")]);

        let mut s = session(&tmp);
        run_workflow(
            "explain the crate",
            llm.clone() as Arc<dyn LlmProvider>,
            Arc::new(agtrs_runtime::signals::SignalBus::new()),
            resolve_ctx(),
            vec![],
            vec![],
            &mut s,
            None,
            None,
            None,
            patch,
        )
        .await
        .expect("planner answering inline should complete");

        let prompts = llm.prompts();
        assert!(
            !prompts.is_empty(),
            "the planner must still have been given a prompt (patch={patch:?})"
        );
        for prompt in &prompts {
            assert!(
                !prompt.starts_with('\n'),
                "a blank patch must not prepend a newline, got: {:?}",
                &prompt[..prompt.len().min(20)]
            );
        }
    }
}

/// A planner that answers without handing off must short-circuit: no coder, no
/// QA verdict line, and the answer returned verbatim.
#[tokio::test]
async fn planner_answering_inline_short_circuits_the_coding_path() {
    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![Scripted::text(
        "The crate is a TUI; no change needed.",
    )]);

    let mut s = session(&tmp);
    let (content, code) = run_workflow(
        "explain the crate",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("an inline answer should complete");

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(content, "The crate is a TUI; no change needed.");
    assert!(
        !content.contains("QA approved") && !content.contains("QA incomplete"),
        "an inline answer must not be decorated with a QA verdict: {content}"
    );
}

// ── run_meta_workflow: the meta agent spawns specialists, then QA ↔ fixer ────

fn meta_config() -> MetaWorkflowConfig {
    MetaWorkflowConfig::default()
}

/// The meta pipeline must run end to end and report success even when the meta
/// agent answers directly.
///
/// `run_meta_workflow` builds the whole agent graph — the spawn tool, the
/// parallel tool, the handoff tool, QA and the fixer — before the first LLM call,
/// so this asserts that graph is constructible and that a direct answer returns
/// the agent's own content rather than a QA verdict.
#[tokio::test]
async fn meta_pipeline_answers_directly_when_no_specialist_is_spawned() {
    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![Scripted::text("The task needs no specialists.")]);

    let mut s = session(&tmp);
    let (content, code) = run_meta_workflow(
        "explain the crate",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        meta_config(),
    )
    .await
    .expect("the meta pipeline should complete");

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        content.contains("The task needs no specialists."),
        "the meta agent's own answer must be returned, got: {content}"
    );
    assert!(
        !content.contains("QA approved") && !content.contains("QA incomplete"),
        "run_meta_workflow has no QA verdict stage, got: {content}"
    );
    assert!(s.turn_count > 0, "session turn_count must be updated");
}

/// The meta agent hands off to QA, and the meta pipeline's `prompt_fn` must
/// build QA's prompt from the meta agent's summary — a different template from
/// the one `run_workflow` uses, with its own `_` fallback.
#[tokio::test]
async fn meta_pipeline_hands_off_to_qa_with_the_meta_summary() {
    let tmp = TempDir::new().unwrap();
    let llm = RecordingLlm::new(vec![
        handoff("qa"),
        Scripted::text("Reviewing the meta agent's plan"),
        Scripted::text("APPROVED - no issues found"),
    ]);

    let mut s = session(&tmp);
    let (content, code) = run_meta_workflow(
        "add a greeting",
        llm.clone() as Arc<dyn LlmProvider>,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
        meta_config(),
    )
    .await
    .expect("the meta pipeline should complete after a handoff");

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(content.contains("APPROVED"));

    // run_meta_workflow's QA arm: "Review changes for task: ... Meta result: ..."
    assert!(
        llm.saw("Meta result:"),
        "the meta pipeline's QA handoff template never reached QA"
    );
    assert!(
        llm.saw("Review changes for task:"),
        "the meta QA template must name the task"
    );
}

/// A `WorkflowConfig::Meta` handed to the *dynamic* runner must be rejected with
/// a message that points at `run_meta_workflow` — otherwise the failure reads as
/// a generic agent error and the caller cannot tell a config mistake from a bug.
#[tokio::test]
async fn dynamic_runner_rejects_a_meta_config_and_names_the_right_entry_point() {
    let tmp = TempDir::new().unwrap();
    let transport = Arc::new(MockTransport::new());
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider::new(transport));

    let mut s = session(&tmp);
    let err = run_dynamic_handoff(
        "task",
        &AgentRegistry::default_xaft(),
        &WorkflowConfig::Meta {
            meta_prompt: None,
            max_spawned_agents: 4,
            max_parallel_agents: 2,
            allow_nesting: false,
            max_nesting_depth: 0,
        },
        llm,
        Arc::new(agtrs_runtime::signals::SignalBus::new()),
        resolve_ctx(),
        vec![],
        vec![],
        &mut s,
        None,
        None,
        None,
    )
    .await;

    let err = err.expect_err("a Meta config must not be accepted by the dynamic runner");
    let msg = err.to_string();
    assert!(
        msg.contains("Meta"),
        "the error must name the offending variant: {msg}"
    );
    assert!(
        msg.contains("run_meta_workflow"),
        "the error must point at the correct entry point: {msg}"
    );
}
