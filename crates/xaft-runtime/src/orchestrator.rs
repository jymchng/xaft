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
//! The planner is now a first-class agent that can read the workspace and
//! decide whether the task requires code changes or can be answered directly.
//! Tasks like "describe this repository" or "what does X function do" return
//! without ever invoking the coder.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContextBuilder};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::{LlmProvider, LlmResponse};
use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
use agtrs_runtime::planner::{OneShotPlanner, Planner, PlannerContext};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::subagent::{ReturnMode, SubagentTool};
use agtrs_runtime::task::Intent;
use agtrs_runtime::team::{HandoffAgentStore, HandoffEvent, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use futures::StreamExt as _;

use crate::agent_registry::{AgentRegistry, WorkflowConfig};
use crate::error::RuntimeError;
use crate::session::AgentSession;
use crate::types::ExitCode;

use xaft_agent::signals::XaftLlmCallStarting;

// ── Return types ──────────────────────────────────────────────────────────────

/// Summary returned by the Coder agent when it finishes all edits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditSummary {
    /// Workspace-relative paths of every file changed.
    pub files_changed: Vec<String>,
    /// Brief human-readable description of what was done.
    pub description: String,
    /// Whether the agent ran and passed tests.
    #[serde(default)]
    pub tests_passed: bool,
    /// Optional notes.
    #[serde(default)]
    pub notes: String,
}

/// Structured output from the smart planner agent.
///
/// The planner uses read-only tools to understand the codebase, then decides:
/// - `"direct_answer"` — task is informational; answer immediately, skip coder.
/// - `"coding_plan"` — task requires file changes; proceed to coder with the plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannerOutput {
    /// `"direct_answer"` or `"coding_plan"`.
    pub task_type: String,
    /// For `"direct_answer"`: the complete response to the user.
    /// For `"coding_plan"`: numbered steps for the coder agent.
    pub content: String,
}

/// Result of the planning phase, driving which downstream agents run.
#[derive(Debug, Clone)]
pub enum PlanResult {
    /// Task was answered directly by the planner — coder and QA are skipped.
    DirectAnswer {
        /// Complete answer to return to the user.
        content: String,
    },
    /// Task requires code changes — proceed to coder → QA ↔ fixer.
    CodingPlan {
        /// Numbered step-by-step plan for the coder agent.
        plan_text: String,
    },
}

const PLANNER_NAME: &str = "planner";
const CODER_NAME: &str = "coder";
const QA_NAME: &str = "qa";
const FIXER_NAME: &str = "fixer";

// ── System prompts ────────────────────────────────────────────────────────────

fn planner_prompt(working_dir: &str) -> String {
    format!(
        "\
You are a smart task analyzer and router for a coding assistant.

WORKING DIRECTORY: {working_dir}
All file paths are relative to this directory.

WORKFLOW — follow exactly:
1. Call `list_files` to understand the project structure.
2. Call `read_file` on 1–3 files most relevant to the task.
3. Decide:

   INFORMATIONAL task (describe, explain, analyze, summarize, list, show,
   what is, how does, why, what are): write a thorough answer directly in
   your response. Do NOT call any handoff tool.

   CODE-CHANGING task (add, implement, fix, refactor, create, modify,
   delete, update, write, change, rename, remove): call
   `handoff_to_agent` with target_agent=\"coder\" and a numbered
   step-by-step plan as `reason`. The coder will receive your plan.

Do NOT call `handoff_to_agent` for informational tasks — just answer inline.
"
    )
}

fn coder_prompt(working_dir: &str) -> String {
    format!(
        "\
You are an expert software engineer. Edit files using the provided tools.

WORKING DIRECTORY: {working_dir}
All file paths are relative to this directory. Use relative paths only. Do NOT use `cd`.

WORKFLOW — follow this order exactly:
1. Read the plan from your incoming message and understand what needs to change.
2. Call `list_files` to discover what files exist.
3. Call `read_file` to read each relevant file before editing.
4. For targeted edits call `edit_file` with {{\"path\": \"<f>\", \"old_content\": \"<exact>\", \"new_content\": \"<replacement>\"}}.
5. To create or fully rewrite a file call `write_file` with {{\"path\": \"<f>\", \"content\": \"<full content>\"}}.
6. Call `bash_exec` to verify changes. Fix any failures before proceeding.
7. When ALL changes are done, call `handoff_to_agent` with target_agent=\"qa\" and
   reason = a brief summary: \"Files changed: [list]. Description: [what changed].\"

RULES:
- Always read a file before editing it
- Make minimal targeted changes
- Supply ALL required fields in every tool call
- You MUST call handoff_to_agent(\"qa\", ...) when done — do not just output text
"
    )
}

/// Legacy variant that embeds the plan inside the system prompt.
/// Used when coder runs via SubagentTool outside the handoff orchestrator.
#[allow(dead_code)]
fn coder_prompt_with_plan(plan_text: &str, working_dir: &str) -> String {
    let plan_section = if plan_text.is_empty() {
        String::new()
    } else {
        format!("PLAN — execute these steps in order:\n{plan_text}\n\n")
    };
    format!(
        "{plan_section}{}",
        coder_prompt(working_dir)
    )
}

fn qa_prompt(task: &str, working_dir: &str) -> String {
    format!(
        "\
You are a code reviewer. Verify that the following task was completed correctly:

TASK: {task}
WORKING DIRECTORY: {working_dir}
Use relative paths for all file operations.

INSTRUCTIONS:
1. Call `list_files` to discover all files in the workspace.
2. Call `read_file` on the most important source files (up to 5 files maximum).
   Focus on files most directly related to the task. Do NOT read every file.
3. Verify by READING the code only — do NOT run bash commands or tests:
   a. The task was ACTUALLY completed (not just partially done)
   b. No obvious syntax errors or broken imports visible in the code
   c. No completely missing or stub implementations
   d. Code structure is consistent with the task

IMPORTANT: You only have a limited number of turns. Be efficient:
- Read only the key files (main source files, entry points)
- Skip test files, lock files, README, and generated files
- Do NOT try to compile or run the code

If the key files look correct: output exactly the word APPROVED on its own line.

If there are clear, obvious issues: call the `request_fix` tool once with a concise
list of ALL issues found. Be specific — name files and functions.
Do NOT fix anything yourself. Do NOT call request_fix more than once."
    )
}

fn fixer_prompt(task: &str, working_dir: &str) -> String {
    format!(
        "\
You are a bug fixer working on this task: {task}
WORKING DIRECTORY: {working_dir}
Use relative paths. Do NOT use `cd`.

INSTRUCTIONS:
1. Read the issues described in your incoming message.
2. Call `list_files` to see all files in the workspace.
3. Call `read_file` for each file that needs fixing.
4. For each file to fix, call `write_file` with the COMPLETE corrected content.
   Fix ALL reported issues. Write the full file — not just the changed lines.
5. When done, call `handoff_to_agent` with target_agent=\"qa\" and a brief summary
   of what you fixed. The QA agent will review your changes.

Do NOT output file content as plain text — all changes must go through write_file.
You MUST call handoff_to_agent(\"qa\", ...) when done — do not just output text."
    )
}

// ── request_fix tool (QA → Fixer handoff) ────────────────────────────────────

struct RequestFixTool {
    store: Arc<HandoffAgentStore>,
}

impl std::fmt::Debug for RequestFixTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFixTool").finish()
    }
}

#[async_trait::async_trait]
impl Tool for RequestFixTool {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        "request_fix"
    }
    fn description(&self) -> &str {
        "Report code issues to the fixer agent. Call when you find bugs, syntax errors, or incomplete task completion."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Precise description of all issues found — name files, functions, lines."
                }
            },
            "required": ["summary"],
            "additionalProperties": false
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        let summary = input["summary"]
            .as_str()
            .unwrap_or("Issues found")
            .to_string();
        let conv_id = ctx
            .state
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !conv_id.is_empty() {
            self.store.set_active_agent(&conv_id, FIXER_NAME).await;
            self.store.set_pending_summary(&conv_id, &summary).await;
        }
        Ok(ToolResult::ok(
            format!("Fix requested: {summary}"),
            &ctx.tool_use_id,
        ))
    }
}

// ── Minimal agent (non-streaming safe) ───────────────────────────────────────

struct NamedAgent {
    name: String,
    config: AgentConfig,
    tools: Vec<Arc<ErasedTool>>,
    /// Optional signal bus — used to emit per-turn text to TUI.
    signals: Option<Arc<SignalBus>>,
}

impl NamedAgent {
    fn new(name: &str, system_prompt: &str, max_turns: usize) -> Self {
        Self {
            name: name.to_string(),
            config: AgentConfig {
                system_prompt: system_prompt.to_string(),
                max_turns,
                strict_capability_check: false,
                parallel_tool_calls: false,
                ..Default::default()
            },
            tools: Vec::new(),
            signals: None,
        }
    }

    fn with_tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self {
        self.tools = tools;
        self
    }

    fn with_signals(mut self, signals: Arc<SignalBus>) -> Self {
        self.signals = Some(signals);
        self
    }
}

#[async_trait::async_trait]
impl agtrs_runtime::agent::Agent for NamedAgent {
    fn name(&self) -> &str {
        &self.name
    }
    fn system_prompt(&self) -> String {
        self.config.system_prompt.clone()
    }
    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        self.tools.clone()
    }
    fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Notify the TUI that this agent is about to call the LLM.
    ///
    /// Fires `XaftLlmCallStarting` so the TUI updates the phase indicator
    /// immediately — even for QA and Fixer agents that run through
    /// `HandoffOrchestrator` where `ModelCallStarted` might arrive late.
    async fn before_llm_call(
        &self,
        _messages: &mut Vec<agtrs_runtime::transport::Message>,
        _options: &mut agtrs_runtime::llm::LlmOptions,
    ) -> Result<(), AgtrsError> {
        if let Some(ref bus) = self.signals {
            let bus = Arc::clone(bus);
            let agent_name = self.name.clone();
            tokio::spawn(async move {
                bus.emit(xaft_agent::signals::XaftLlmCallStarting {
                    agent_name,
                    call_index: 0,
                })
                .await;
            });
        }
        Ok(())
    }

    /// Emit non-empty LLM text responses to TUI on every turn.
    async fn after_llm_call(&self, response: &LlmResponse) -> Result<(), AgtrsError> {
        // Only forward text responses (not pure tool-call turns)
        if let Some(ref bus) = self.signals {
            let text = response.message.text();
            if !text.trim().is_empty() {
                let bus = Arc::clone(bus);
                let agent_name = self.name.clone();
                tokio::spawn(async move {
                    bus.emit(xaft_agent::XaftAgentOutput {
                        agent_name,
                        content: text,
                    })
                    .await;
                });
            }
        }
        Ok(())
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the unified xaft workflow.
///
/// All four agents — planner, coder, QA, fixer — live inside a single
/// [`HandoffOrchestrator`], exactly like the `lauren-ai-chatbot-rs` CRM
/// pattern where any agent can choose to reply directly or hand off.
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

    // Planner: read-only tools + handoff_to_agent("coder") for coding tasks.
    // For info tasks it just outputs text inline — no handoff call needed.
    let mut planner_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
    planner_tools.push(
        Arc::new(crate::agent_registry::HandoffTool::new(
            Arc::clone(&handoff_store),
            vec![CODER_NAME.into()],
        )) as Arc<ErasedTool>,
    );
    let planner_agent = Arc::new(
        NamedAgent::new(PLANNER_NAME, &planner_prompt(&wd), 15)
            .with_tools(planner_tools)
            .with_signals(Arc::clone(&signals)),
    );

    // Coder: write tools + handoff_to_agent("qa") when done.
    let mut coder_tools: Vec<Arc<ErasedTool>> = write_tools.clone();
    coder_tools.push(
        Arc::new(crate::agent_registry::HandoffTool::new(
            Arc::clone(&handoff_store),
            vec![QA_NAME.into()],
        )) as Arc<ErasedTool>,
    );
    let coder_agent = Arc::new(
        NamedAgent::new(CODER_NAME, &coder_prompt(&wd), 40)
            .with_tools(coder_tools)
            .with_signals(Arc::clone(&signals)),
    );

    // QA: read tools + request_fix (writes to store → fixer).
    let fix_tool = Arc::new(RequestFixTool {
        store: Arc::clone(&handoff_store),
    }) as Arc<ErasedTool>;
    let mut qa_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
    qa_tools.push(Arc::clone(&fix_tool));
    let qa_agent = Arc::new(
        NamedAgent::new(QA_NAME, &qa_prompt(task, &wd), 25)
            .with_tools(qa_tools)
            .with_signals(Arc::clone(&signals)),
    );

    // Fixer: write tools + handoff_to_agent("qa") when done.
    let mut fixer_tools: Vec<Arc<ErasedTool>> = write_tools.clone();
    fixer_tools.push(
        Arc::new(crate::agent_registry::HandoffTool::new(
            Arc::clone(&handoff_store),
            vec![QA_NAME.into()],
        )) as Arc<ErasedTool>,
    );
    let fixer_agent = Arc::new(
        NamedAgent::new(FIXER_NAME, &fixer_prompt(task, &wd), 25)
            .with_tools(fixer_tools)
            .with_signals(Arc::clone(&signals)),
    );

    // ── One orchestrator for all agents ───────────────────────────────────────
    // max_handoffs: planner→coder(1) coder→qa(2) qa→fixer(3) fixer→qa(4) …×3 = ~14
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
        .prompt_fn(move |ctx| {
            // Called when any agent hands off to the next.
            // ctx.summary = the `reason` supplied to handoff_to_agent / request_fix.
            // ctx.original_message = the user's original task.
            match ctx.to_agent.as_str() {
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
            }
        })
        .with_approval_gate_opt(approval_gate)
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
        // Planner answered the question inline — no coding happened.
        return Ok((result.content, ExitCode::SUCCESS));
    }

    // Coding path: build a concise concluding summary.
    let approved = result.content.to_uppercase().contains("APPROVED");
    let qa_verdict = if approved { "✓ QA approved" } else { "⚠ QA incomplete" };

    let edit_summary = serde_json::from_str::<EditSummary>(&result.content).unwrap_or(
        EditSummary {
            files_changed: vec![],
            description: result.content.clone(),
            tests_passed: false,
            notes: String::new(),
        },
    );

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

// ── Concluding summary ────────────────────────────────────────────────────────

/// Ask the Planner LLM for a brief concluding summary of what was accomplished.
///
/// Uses `OneShotPlanner` with a summarisation prompt to produce 2–3 plain-text
/// sentences.  Falls back to a formatted string if the LLM call fails.
async fn build_concluding_summary(
    task: &str,
    edit_summary: &EditSummary,
    qa_content: &str,
    approved: bool,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
) -> String {
    let files_str = if edit_summary.files_changed.is_empty() {
        "(no files recorded)".to_string()
    } else {
        edit_summary.files_changed.join(", ")
    };

    // Strip APPROVED/REJECTED keyword, keep only the explanatory text.
    let qa_note = {
        let c = qa_content.trim();
        let stripped = c
            .trim_start_matches("APPROVED")
            .trim_start_matches("REJECTED")
            .trim();
        if stripped.is_empty() { c } else { stripped }
    };

    let instructions = format!(
        "You are summarising the result of an automated coding task. \
         Write 2–3 short, direct sentences (no bullet points, no markdown). \
         State what was done, which files changed, and the QA outcome.\n\n\
         Task: {task}\n\
         Changes: {desc}\n\
         Files: {files}\n\
         QA: {verdict}\n\
         QA note: {qa_note}",
        desc = edit_summary.description,
        files = files_str,
        verdict = if approved { "APPROVED" } else { "INCOMPLETE" },
    );

    let intent = agtrs_runtime::task::Intent::from_goal(task).build();
    let ctx = agtrs_runtime::planner::PlannerContext::initial(&intent, vec![]);
    let planner = OneShotPlanner::new(Arc::clone(&llm))
        .with_resolve_ctx(resolve_ctx)
        .with_max_steps(1)
        .with_instructions(&instructions);

    // We don't need structured plan steps — just need the planner's raw LLM
    // response as a text string.  Extract from the plan text field.
    match planner.plan(&ctx).await {
        Ok(plan) if !plan.steps.is_empty() => {
            // Steps contain the description; join as prose
            let text = plan
                .steps
                .iter()
                .map(|s| s.description.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            strip_markdown(&text)
        }
        _ => {
            // Fallback: build from known metadata without extra LLM call
            let test_str = if edit_summary.tests_passed { "Tests passed." } else { "" };
            let qa_line = if !qa_note.is_empty() {
                format!("\n\n{}", strip_markdown(qa_note))
            } else {
                String::new()
            };
            format!("{}{}{}", edit_summary.description, if test_str.is_empty() { "" } else { " " }, test_str) + &qa_line
        }
    }
}

// ── Markdown stripper ─────────────────────────────────────────────────────────

/// Strip common markdown formatting from text for plain-terminal display.
///
/// Removes: heading markers (`#`), bold/italic (`**`, `*`, `__`, `_`),
/// inline code backticks, horizontal rules (`---`, `===`), and list bullets
/// (`- `, `* `). Preserves line breaks and text content.
fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip pure horizontal rules
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '=' || c == '*' || c == ' ')
            && trimmed.len() >= 3
            && !trimmed.is_empty()
        {
            out.push('\n');
            continue;
        }
        // Strip heading markers (# ## ### etc.)
        let line = if trimmed.starts_with('#') {
            trimmed.trim_start_matches('#').trim()
        } else {
            line
        };
        // Strip list markers (- item, * item, + item)
        let line = if let Some(rest) = line
            .trim_start()
            .strip_prefix("- ")
            .or_else(|| line.trim_start().strip_prefix("* "))
            .or_else(|| line.trim_start().strip_prefix("+ "))
        {
            rest
        } else {
            line
        };
        // Strip inline bold/italic/code markers
        let line = line.replace("**", "").replace("__", "").replace('`', "");
        // Strip remaining single-star italic but keep * inside words
        // Simple approach: remove leading/trailing single *
        let line = line.trim_matches('*');
        out.push_str(line);
        out.push('\n');
    }
    // Collapse multiple consecutive blank lines into one
    let mut result = String::with_capacity(out.len());
    let mut blank_count = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }
    result.trim().to_string()
}


/// Parse a raw planner response string into [`PlanResult`].
///
/// Used as a fallback when `SubagentTool<PlannerOutput>` cannot parse JSON.
/// Heuristically detects numbered-step plans vs. prose answers.
pub fn parse_plan_result(task: &str, raw: &str) -> PlanResult {
    let trimmed = raw.trim();
    // Try JSON first
    if let Ok(output) = serde_json::from_str::<PlannerOutput>(trimmed) {
        return if output.task_type == "direct_answer" {
            PlanResult::DirectAnswer {
                content: output.content,
            }
        } else {
            PlanResult::CodingPlan {
                plan_text: output.content,
            }
        };
    }
    // Heuristic: if it looks like a numbered plan (starts with "1."), treat as coding
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.starts_with("1.") || first_line.starts_with("1)") {
        PlanResult::CodingPlan {
            plan_text: trimmed.to_string(),
        }
    } else if trimmed.is_empty() || trimmed == task {
        // Unparseable — fall through to coding
        PlanResult::CodingPlan {
            plan_text: task.to_string(),
        }
    } else {
        // Assume prose = direct answer
        PlanResult::DirectAnswer {
            content: trimmed.to_string(),
        }
    }
}

// ── Dynamic handoff entry point ───────────────────────────────────────────────

/// Run a dynamic multi-agent workflow using [`AgentRegistry`] and
/// [`HandoffOrchestrator`].
///
/// Any agent in the registry can hand off to any other by calling the
/// `handoff_to_agent` tool. The orchestrator loops until no agent requests a
/// handoff or `max_handoffs` is reached.
///
/// # Workflow config
///
/// Pass [`WorkflowConfig::Dynamic`] to select which agent starts the run and
/// how many handoffs are allowed. [`WorkflowConfig::Standard`] returns
/// immediately — use [`run_workflow`] for the classic pipeline.
///
/// # Example
///
/// ```rust,ignore
/// let registry = AgentRegistry::default_xaft()
///     .register(AgentDefinition { name: "db_migrator", ... });
///
/// run_dynamic_handoff(
///     "migrate users table",
///     &registry,
///     WorkflowConfig::Dynamic {
///         initial_agent: "db_migrator".into(),
///         max_handoffs: 6,
///         agent_subset: None,
///     },
///     llm, signals, resolve_ctx,
///     read_tools, write_tools,
///     &mut session,
///     Some(conv_store), None,
/// ).await?;
/// ```
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
        } => (initial_agent.as_str(), *max_handoffs, agent_subset.as_deref()),
    };

    let handoff_store = Arc::new(HandoffAgentStore::new());
    let wd = session.workspace_root.display().to_string();
    let conv_id = format!("{}::{}", session.id, initial_agent);

    // Build the orchestrator with all agents from the (optionally filtered) registry.
    let agent_names: Vec<&str> = match agent_subset {
        Some(subset) => subset.iter().map(String::as_str).collect(),
        None => registry.agent_names().iter().map(String::as_str).collect(),
    };

    if !agent_names.contains(&initial_agent) {
        return Err(RuntimeError::Agent(format!(
            "initial_agent '{initial_agent}' is not in the agent_subset or registry"
        )));
    }

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

    // ── Use run_stream to capture HandoffEvent::AgentHandoff events ──────────
    // For each handoff detected, emit a `XaftAgentHandoff` signal on the bus.
    let mut event_stream = orchestrator.run_stream(HandoffRunParams {
        message: task.to_string(),
        conversation_id: conv_id.clone(),
        initial_agent: initial_agent.to_string(),
        context_state,
        #[cfg(feature = "axum")]
        extensions: Default::default(),
        signals: Some(Arc::clone(&signals)),
        max_handoffs_override: None,
    });

    // Accumulate result fields as we consume the stream.
    let mut final_agent_name = initial_agent.to_string();
    let mut final_content = String::new();
    let mut total_turns = 0usize;

    while let Some(event) = event_stream.next().await {
        match event {
            HandoffEvent::AgentHandoff {
                ref from_agent,
                ref to_agent,
                ref summary,
            } => {
                let signal = xaft_agent::signals::XaftAgentHandoff {
                    from_agent: from_agent.clone(),
                    to_agent: to_agent.clone(),
                    summary: summary.clone(),
                };
                let bus = Arc::clone(&signals);
                tokio::spawn(async move {
                    bus.emit(signal).await;
                });
            }
            HandoffEvent::AgentStarted { ref agent_name } => {
                final_agent_name = agent_name.clone();
            }
            HandoffEvent::AgentEvent(ref stream_event) => {
                // Extract the final text content from Done events.
                if let agtrs_runtime::streaming::StreamEvent::Done {
                    content,
                    turns,
                    ..
                } = stream_event
                {
                    total_turns += turns;
                    final_content = content.clone();
                }
            }
            HandoffEvent::Completed => break,
        }
    }

    let result = agtrs_runtime::team::HandoffResult {
        content: final_content,
        agent_name: final_agent_name,
        turns: total_turns,
    };

    session.turn_count += result.turns as u32;
    info!(
        agent = %result.agent_name,
        turns = result.turns,
        "xaft: dynamic handoff workflow complete"
    );

    Ok(result)
}
