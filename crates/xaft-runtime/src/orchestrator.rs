//! Structured multi-agent workflow for xaft.
//!
//! ```text
//! run_workflow()
//!   ├── Step 1: Smart Planning  (SubagentTool<PlannerOutput> with read tools)
//!   │          ↓ PlanResult::DirectAnswer  → emit answer, return early (no coder)
//!   │          ↓ PlanResult::CodingPlan    → continue
//!   ├── Step 2: Coder     (SubagentTool<EditSummary> — AgentExecutor::run())
//!   └── Step 3: QA ↔ Fixer  (HandoffOrchestrator::run() — non-streaming)
//!              Cycles until QA outputs APPROVED or max_handoffs reached.
//! ```
//!
//! Domain agent definitions (prompts, types, agents) live in `xaft-agents`.
//! This module contains only the orchestration plumbing that wires them together.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracing::info;

use agtrs_runtime::agent::Agent;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::team::{HandoffAgentStore, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::ErasedTool;

use crate::agent_registry::{AgentRegistry, HandoffTool, WorkflowConfig};
use crate::error::RuntimeError;
use crate::session::AgentSession;
use crate::types::ExitCode;

use xaft_agent::signals::XaftLlmCallStarting;

// ── Re-exports from xaft-agents ───────────────────────────────────────────────
// These preserve backward compatibility: external code that imported from
// `xaft_runtime::orchestrator::EditSummary` etc. continues to compile.

pub use xaft_agents::coder::CODER_NAME;
pub use xaft_agents::coder::EditSummary;
pub use xaft_agents::coder::coder_system_prompt;
pub use xaft_agents::fixer::FIXER_NAME;
pub use xaft_agents::fixer::fixer_system_prompt;
pub use xaft_agents::named::NamedAgent;
pub use xaft_agents::planner::PLANNER_NAME;
pub use xaft_agents::planner::planner_system_prompt;
pub use xaft_agents::planner::{PlanResult, PlannerOutput, parse_plan_result};
pub use xaft_agents::qa::QA_NAME;
pub use xaft_agents::qa::RequestFixTool;
pub use xaft_agents::qa::qa_system_prompt;
pub use xaft_agents::summarizer::{build_concluding_summary, strip_markdown};

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the unified xaft workflow.
///
/// All four agents — planner, coder, QA, fixer — live inside a single
/// [`HandoffOrchestrator`].
///
/// Flow:
/// - **Planner** reads the workspace then either:
///   - answers inline (no handoff → orchestrator terminates with planner)
///   - calls `handoff_to_agent("coder", plan)` → coder runs
/// - **Coder** makes file changes then calls `handoff_to_agent("qa", summary)`.
/// - **QA** approves (APPROVED → done) or calls `request_fix` → fixer.
/// - **Fixer** fixes issues then calls `handoff_to_agent("qa", summary)` to re-review.
///
/// `conversation_store` — when `Some`, history persists for session resume.
pub async fn run_workflow(
    task: &str,
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    read_tools: Vec<Arc<ErasedTool>>,
    write_tools: Vec<Arc<ErasedTool>>,
    session: &mut AgentSession,
    conversation_store: Option<Arc<dyn ConversationStore>>,
    approval_gate: Option<Arc<dyn agtrs_runtime::approval::ApprovalGate>>,
) -> Result<(String, ExitCode), RuntimeError> {
    let wd = session.workspace_root.display().to_string();

    // Shared store: all handoff tools write here; orchestrator reads it.
    let handoff_store = Arc::new(HandoffAgentStore::new());

    // ── Build agents ──────────────────────────────────────────────────────────
    // Each agent that calls handoff_to_agent gets its own AtomicBool stop flag
    // shared with its HandoffTool.  When the handoff tool fires the flag is set
    // to `true`, and the agent's `before_llm_call` returns Err on the very next
    // call — preventing the agent from looping and calling handoff repeatedly.

    // Planner: read tools + handoff_to_agent("coder") for coding tasks.
    let planner_stop = Arc::new(AtomicBool::new(false));
    let mut planner_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
    planner_tools.push(Arc::new(HandoffTool::new_with_flag(
        Arc::clone(&handoff_store),
        vec![CODER_NAME.into()],
        Arc::clone(&planner_stop),
    )) as Arc<ErasedTool>);
    let planner_agent = Arc::new(
        NamedAgent::new(PLANNER_NAME, &planner_system_prompt(&wd), 100)
            .with_tools(planner_tools)
            .with_signals(Arc::clone(&signals))
            .with_handoff_flag(Arc::clone(&planner_stop)),
    );

    // Coder: write tools + handoff_to_agent("qa") when done.
    let coder_stop = Arc::new(AtomicBool::new(false));
    let mut coder_tools: Vec<Arc<ErasedTool>> = write_tools.clone();
    coder_tools.push(Arc::new(HandoffTool::new_with_flag(
        Arc::clone(&handoff_store),
        vec![QA_NAME.into()],
        Arc::clone(&coder_stop),
    )) as Arc<ErasedTool>);
    let coder_agent = Arc::new(
        NamedAgent::new(CODER_NAME, &coder_system_prompt("", &wd), 100)
            .with_tools(coder_tools)
            .with_signals(Arc::clone(&signals))
            .with_handoff_flag(Arc::clone(&coder_stop)),
    );

    // QA: read tools + request_fix (writes to store → fixer).
    let fix_tool = Arc::new(RequestFixTool::new(Arc::clone(&handoff_store))) as Arc<ErasedTool>;
    let mut qa_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
    qa_tools.push(Arc::clone(&fix_tool));
    let qa_agent = Arc::new(
        NamedAgent::new(QA_NAME, &qa_system_prompt(task, &wd), 100)
            .with_tools(qa_tools)
            .with_signals(Arc::clone(&signals)),
    );

    // Fixer: write tools + handoff_to_agent("qa") when done.
    let fixer_stop = Arc::new(AtomicBool::new(false));
    let mut fixer_tools: Vec<Arc<ErasedTool>> = write_tools.clone();
    fixer_tools.push(Arc::new(HandoffTool::new_with_flag(
        Arc::clone(&handoff_store),
        vec![QA_NAME.into()],
        Arc::clone(&fixer_stop),
    )) as Arc<ErasedTool>);
    let fixer_agent = Arc::new(
        NamedAgent::new(FIXER_NAME, &fixer_system_prompt(task, &wd), 100)
            .with_tools(fixer_tools)
            .with_signals(Arc::clone(&signals))
            .with_handoff_flag(Arc::clone(&fixer_stop)),
    );

    // ── One orchestrator for all agents ───────────────────────────────────────
    // max_handoffs: planner→coder(1) coder→qa(2) qa→fixer(3) fixer→qa(4) …×3 = ~14
    let read_before_edit_hook = Arc::new(xaft_tools::ReadBeforeEditHook::new())
        as Arc<dyn agtrs_runtime::tool_hooks::ToolHook>;
    let orchestrator = HandoffOrchestrator::builder()
        .agent(PLANNER_NAME, Arc::clone(&planner_agent) as Arc<dyn Agent>)
        .agent(CODER_NAME, Arc::clone(&coder_agent) as Arc<dyn Agent>)
        .agent(QA_NAME, Arc::clone(&qa_agent) as Arc<dyn Agent>)
        .agent(FIXER_NAME, Arc::clone(&fixer_agent) as Arc<dyn Agent>)
        .conv_store(
            conversation_store
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryConversationStore::new())),
        )
        .agent_store(Arc::clone(&handoff_store))
        .max_handoffs(14)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .prompt_fn(move |ctx| match ctx.to_agent.as_str() {
            "coder" => format!(
                "PLAN — execute these steps in order:\n{}\n\nOriginal task: {}",
                ctx.summary, ctx.original_message
            ),
            "qa" => format!(
                "Review the code changes for this task: {}\n\n\
                 Changes summary: {}\n\nUse list_files + read_file to inspect.",
                ctx.original_message, ctx.summary
            ),
            "fixer" => format!(
                "Fix the following issues found by the code reviewer:\n{}\n\n\
                 Original task: {}\n\nCall handoff_to_agent(\"qa\", ...) when done.",
                ctx.summary, ctx.original_message
            ),
            _ => format!(
                "[HANDOFF from {}]: {}\n\nOriginal task: {}",
                ctx.from_agent, ctx.summary, ctx.original_message
            ),
        })
        .with_approval_gate_opt(approval_gate)
        .with_global_tool_hook(read_before_edit_hook)
        .build();

    let conv_id = format!("{}::workflow", session.id);
    let mut context_state = HashMap::new();
    context_state.insert("conversation_id".to_string(), serde_json::json!(conv_id));

    info!(task, "xaft: starting unified handoff workflow");

    let result = orchestrator
        .run(HandoffRunParams {
            message: task.to_string(),
            conversation_id: conv_id,
            initial_agent: PLANNER_NAME.to_string(),
            context_state,
            #[cfg(feature = "axum")]
            extensions: Default::default(),
            signals: Some(Arc::clone(&signals)),
            max_handoffs_override: None,
        })
        .await
        .map_err(|e| RuntimeError::Agent(e.to_string()))?;

    session.turn_count += result.turns as u32;
    info!(
        final_agent = %result.agent_name,
        turns = result.turns,
        "xaft: workflow complete"
    );

    // ── Route on final agent ──────────────────────────────────────────────────
    if result.agent_name == PLANNER_NAME {
        return Ok((result.content, ExitCode::SUCCESS));
    }

    // Coding path: build a concise concluding summary.
    let approved = result.content.to_uppercase().contains("APPROVED");
    let qa_verdict = if approved {
        "✓ QA approved"
    } else {
        "⚠ QA incomplete"
    };

    let edit_summary =
        serde_json::from_str::<EditSummary>(&result.content).unwrap_or(EditSummary {
            files_changed: vec![],
            description: result.content.clone(),
            tests_passed: false,
            notes: String::new(),
        });

    let summary_text = build_concluding_summary(
        task,
        &edit_summary,
        &result.content,
        approved,
        Arc::clone(&llm),
        Arc::clone(&resolve_ctx),
    )
    .await;

    signals
        .emit(XaftLlmCallStarting {
            agent_name: PLANNER_NAME.to_string(),
            call_index: 0,
        })
        .await;
    signals
        .emit(xaft_agent::XaftAgentOutput {
            agent_name: PLANNER_NAME.to_string(),
            content: format!("{qa_verdict}\n\n{summary_text}"),
        })
        .await;

    Ok((format!("{qa_verdict}\n\n{summary_text}"), ExitCode::SUCCESS))
}

// ── Dynamic handoff entry point ───────────────────────────────────────────────

/// Run a dynamic multi-agent workflow using [`AgentRegistry`] and
/// [`HandoffOrchestrator`].
///
/// Any agent in the registry can hand off to any other by calling the
/// `handoff_to_agent` tool. The orchestrator loops until no agent requests a
/// handoff or `max_handoffs` is reached.
pub async fn run_dynamic_handoff(
    task: &str,
    registry: &AgentRegistry,
    workflow: &WorkflowConfig,
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    read_tools: Vec<Arc<ErasedTool>>,
    write_tools: Vec<Arc<ErasedTool>>,
    session: &mut AgentSession,
    conversation_store: Option<Arc<dyn ConversationStore>>,
    approval_gate: Option<Arc<dyn agtrs_runtime::approval::ApprovalGate>>,
) -> Result<agtrs_runtime::team::HandoffResult, RuntimeError> {
    let (initial_agent, max_handoffs, agent_subset) = match workflow {
        WorkflowConfig::Standard => {
            return Err(RuntimeError::Agent(
                "run_dynamic_handoff called with WorkflowConfig::Standard; \
                 use run_workflow() for the classic pipeline"
                    .into(),
            ));
        }
        WorkflowConfig::Dynamic {
            initial_agent,
            max_handoffs,
            agent_subset,
        } => (
            initial_agent.as_str(),
            *max_handoffs,
            agent_subset.as_deref(),
        ),
    };

    let handoff_store = Arc::new(HandoffAgentStore::new());
    let wd = session.workspace_root.display().to_string();
    let conv_id = format!("{}::{}", session.id, initial_agent);

    let agent_names: Vec<&str> = match agent_subset {
        Some(subset) => subset.iter().map(String::as_str).collect(),
        None => registry.agent_names().iter().map(String::as_str).collect(),
    };

    if !agent_names.contains(&initial_agent) {
        return Err(RuntimeError::Agent(format!(
            "initial_agent '{initial_agent}' is not in the agent_subset or registry"
        )));
    }

    let read_before_edit_hook = Arc::new(xaft_tools::ReadBeforeEditHook::new())
        as Arc<dyn agtrs_runtime::tool_hooks::ToolHook>;
    let mut builder = HandoffOrchestrator::builder()
        .conv_store(
            conversation_store
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryConversationStore::new())),
        )
        .agent_store(Arc::clone(&handoff_store))
        .max_handoffs(max_handoffs)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .with_approval_gate_opt(approval_gate)
        .with_global_tool_hook(read_before_edit_hook)
        .prompt_fn(|ctx| {
            format!(
                "[HANDOFF from {}]: {}\n\n[ORIGINAL REQUEST]: {}",
                ctx.from_agent, ctx.summary, ctx.original_message
            )
        });

    for name in &agent_names {
        let agent = registry.build_agent(
            name,
            task,
            &wd,
            &read_tools,
            &write_tools,
            Arc::clone(&handoff_store),
            Arc::clone(&signals),
        )?;
        builder = builder.agent(*name, agent);
    }

    let orchestrator = builder.build();

    let mut context_state = HashMap::new();
    context_state.insert("conversation_id".to_string(), serde_json::json!(conv_id));

    let result = orchestrator
        .run(HandoffRunParams {
            message: task.to_string(),
            conversation_id: conv_id.clone(),
            initial_agent: initial_agent.to_string(),
            context_state,
            #[cfg(feature = "axum")]
            extensions: Default::default(),
            signals: Some(Arc::clone(&signals)),
            max_handoffs_override: None,
        })
        .await
        .map_err(|e| RuntimeError::Agent(e.to_string()))?;

    session.turn_count += result.turns as u32;
    info!(
        agent = %result.agent_name,
        turns = result.turns,
        "xaft: dynamic handoff workflow complete"
    );

    Ok(result)
}
