//! Structured multi-agent workflow for xaft.
//!
//! ```text
//! run_workflow()
//!   ├── Step 1: Planning  (OneShotPlanner → IterativeRefinementPlanner)
//!   ├── Step 2: Coder     (SubagentTool<EditSummary> — AgentExecutor::run())
//!   └── Step 3: QA ↔ Fixer  (HandoffOrchestrator::run() — non-streaming)
//!              Cycles until QA outputs APPROVED or max_handoffs reached.
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContextBuilder};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::{LlmProvider, LlmResponse};
use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
use agtrs_runtime::planner::{IterativeRefinementPlanner, OneShotPlanner, Planner, PlannerContext};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::subagent::{ReturnMode, SubagentTool};
use agtrs_runtime::task::Intent;
use agtrs_runtime::team::{HandoffAgentStore, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::transport::Message;

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

const CODER_NAME: &str = "coder";
const QA_NAME: &str = "qa";
const FIXER_NAME: &str = "fixer";

// ── System prompts ────────────────────────────────────────────────────────────

fn coder_prompt(plan_text: &str, working_dir: &str) -> String {
    format!(
        "\
You are an expert software engineer. Edit files using the provided tools.

WORKING DIRECTORY: {working_dir}
All file paths are relative to this directory. Use relative paths (e.g. \"src/main.rs\"), NOT absolute paths.
Do NOT use `cd` — all commands run from {working_dir} automatically.

{plan_section}

WORKFLOW — follow this order exactly:
1. Call `list_files` to discover what files exist.
2. Call `read_file` with {{\"path\": \"<filename>\"}} to read each relevant file.
3. Call `grep` with {{\"pattern\": \"<search_term>\"}} to locate specific patterns.
4. For targeted edits call `edit_file` with {{\"path\": \"<f>\", \"old_content\": \"<exact>\", \"new_content\": \"<replacement>\"}}.
5. To create or fully rewrite a file call `write_file` with {{\"path\": \"<f>\", \"content\": \"<full content>\"}}.
6. Call `bash_exec` with {{\"command\": \"<cmd>\"}} to verify changes (e.g. \"python src/main.py\").
   If a command FAILS (exit code != 0), fix the issue before proceeding.

RULES:
- Always read a file before editing it
- Use relative paths only
- Supply ALL required fields in every tool call
- Make minimal targeted changes
- After ALL changes are done, output ONLY this JSON:
{{\"files_changed\":[\"path/a.py\"],\"description\":\"one sentence\",\"tests_passed\":false,\"notes\":\"\"}}
",
        plan_section = if plan_text.is_empty() {
            String::new()
        } else {
            format!("PLAN — execute these steps in order:\n{plan_text}\n")
        }
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
1. Call `list_files` to see all files in the workspace.
2. Call `read_file` for each file that needs fixing.
3. For each file to fix, call `write_file` with the COMPLETE corrected content.
   Fix ALL reported issues. Write the full file — not just the changed lines.
4. After fixing all files, output a brief summary of what was changed.

Do NOT output file content as plain text — all changes must go through write_file.
Supply ALL required fields in every tool call."
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

/// Run the full xaft orchestrated workflow: plan → coder → QA ↔ fixer.
///
/// `conversation_store` — when `Some`, conversation history is persisted so
/// the session can be resumed.  `None` → ephemeral in-memory store.
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
    // ── Step 1: Plan ──────────────────────────────────────────────────────────
    let tool_names: Vec<String> = write_tools.iter().map(|t| t.name().into()).collect();
    let intent = Intent::from_goal(task).build();
    let plan_ctx = PlannerContext::initial(&intent, tool_names);
    let plan_text = build_plan(task, &plan_ctx, Arc::clone(&llm), Arc::clone(&resolve_ctx)).await;
    info!(task, "xaft: plan ready");
    tracing::info!(plan = %plan_text, "xaft: plan");

    // Emit planner output to TUI so it appears in the conversation pane
    signals
        .emit(XaftLlmCallStarting {
            agent_name: "planner".to_string(),
            call_index: 0,
        })
        .await;
    signals
        .emit(xaft_agent::XaftAgentOutput {
            agent_name: "planner".to_string(),
            content: plan_text.clone(),
        })
        .await;

    // ── Step 2: Coder (SubagentTool → AgentExecutor::run, non-streaming) ─────
    let wd = session.workspace_root.display().to_string();
    let coder_prompt = coder_prompt(&plan_text, &wd);
    let coder_agent = Arc::new(
        NamedAgent::new(CODER_NAME, &coder_prompt, 40)
            .with_tools(write_tools.clone())
            .with_signals(Arc::clone(&signals)),
    );

    let coder_tool = SubagentTool::<EditSummary>::builder()
        .name(CODER_NAME)
        .description("Execute the coding plan")
        .subagent(Arc::clone(&coder_agent) as Arc<dyn Agent>)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .system_prompt(&coder_prompt)
        .max_turns(40)
        .return_mode(ReturnMode::StructuredLlm)
        .signals(Arc::clone(&signals))
        .approval_gate_opt(approval_gate.clone())
        .build();

    let edit_summary = match coder_tool.run(task.to_string()).await {
        Ok(s) => {
            info!(files = ?s.files_changed, tests_passed = s.tests_passed, "xaft: coder done");
            tracing::info!(files = ?s.files_changed, description = %s.description, "xaft: coder done");
            s
        }
        Err(e) => {
            warn!(error = %e, "xaft: coder summary parse failed — proceeding to QA");
            EditSummary {
                files_changed: vec![],
                description: format!("coder completed (summary parse failed: {e})"),
                tests_passed: false,
                notes: String::new(),
            }
        }
    };
    session.turn_count += 1;

    // ── Step 3: QA ↔ Fixer via HandoffOrchestrator::run() (non-streaming) ────
    let handoff_store = Arc::new(HandoffAgentStore::new());
    let fix_tool = Arc::new(RequestFixTool {
        store: Arc::clone(&handoff_store),
    }) as Arc<ErasedTool>;

    let mut qa_tools = read_tools.clone();
    qa_tools.push(Arc::clone(&fix_tool));

    // QA: 10 turns max — reads key files then approves or calls request_fix once.
    // Fixer: 15 turns max — reads + rewrites affected files.
    // max_handoffs: counts total agent runs (QA+Fixer+QA+...); 10 = 5 full cycles.
    let qa_agent = Arc::new(
        NamedAgent::new(QA_NAME, &qa_prompt(task, &wd), 25)
            .with_tools(qa_tools)
            .with_signals(Arc::clone(&signals)),
    );
    let fixer_agent = Arc::new(
        NamedAgent::new(FIXER_NAME, &fixer_prompt(task, &wd), 25)
            .with_tools(write_tools.clone())
            .with_signals(Arc::clone(&signals)),
    );

    info!("xaft: starting QA/Fixer review (max 3 cycles)");

    let orchestrator = HandoffOrchestrator::builder()
        .agent(QA_NAME, Arc::clone(&qa_agent) as Arc<dyn Agent>)
        .agent(FIXER_NAME, Arc::clone(&fixer_agent) as Arc<dyn Agent>)
        .conv_store(
            conversation_store
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryConversationStore::new())),
        )
        .agent_store(Arc::clone(&handoff_store))
        .max_handoffs(10) // 5 full QA→Fixer→QA cycles
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .prompt_fn(|ctx| {
            format!(
                "The code reviewer found these issues:\n{}\n\nOriginal task:\n{}\n\n\
                 Fix ALL listed issues. The reviewer will check again after you finish.",
                ctx.summary, ctx.original_message
            )
        })
        .with_approval_gate_opt(approval_gate)
        .build();

    let conv_id = format!("{}::qa", session.id);
    let changed_list = if edit_summary.files_changed.is_empty() {
        "Use list_files to discover changed files.".to_string()
    } else {
        format!("Files changed by coder: {:?}", edit_summary.files_changed)
    };
    let qa_message = format!(
        "Review the code changes for this task: {task}\n\n{changed_list}\n\
         Use list_files + read_file to inspect the changes."
    );

    // Use run() — non-streaming, uses AgentExecutor::run() → llm.complete()
    // Eliminates streaming delta assembly issues entirely.
    let qa_result = orchestrator
        .run(HandoffRunParams {
            message: qa_message,
            conversation_id: conv_id.clone(),
            initial_agent: QA_NAME.to_string(),
            context_state: {
                let mut m = HashMap::new();
                m.insert("conversation_id".to_string(), serde_json::json!(conv_id));
                m
            },
            #[cfg(feature = "axum")]
            extensions: Default::default(),
            signals: Some(Arc::clone(&signals)),
            max_handoffs_override: None,
        })
        .await
        .map_err(|e| RuntimeError::Agent(e.to_string()))?;

    session.turn_count += qa_result.turns as u32;

    let approved = qa_result.content.to_uppercase().contains("APPROVED");
    info!(approved, agent = %qa_result.agent_name, turns = qa_result.turns, "xaft: QA cycle complete");
    tracing::info!(approved, content = %qa_result.content, "xaft: QA result");

    // ── Planner concluding summary (LLM call) ─────────────────────────────────
    // After QA, hand back to Planner for a human-readable concluding summary.
    // This is a single-turn LLM call — no tools, no sub-agents, just a concise
    // closing statement the user sees as the final output.
    let qa_verdict = if approved { "✓ QA approved" } else { "⚠ QA incomplete" };

    let summary_text = build_concluding_summary(
        task,
        &edit_summary,
        &qa_result.content,
        approved,
        Arc::clone(&llm),
        Arc::clone(&resolve_ctx),
    )
    .await;

    signals
        .emit(XaftLlmCallStarting {
            agent_name: "planner".to_string(),
            call_index: 0,
        })
        .await;
    signals
        .emit(xaft_agent::XaftAgentOutput {
            agent_name: "planner".to_string(),
            content: format!("{qa_verdict}\n\n{summary_text}"),
        })
        .await;
    let summary_text = format!("{qa_verdict}\n\n{summary_text}");

    Ok((summary_text, ExitCode::SUCCESS))
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

// ── Planning helper ───────────────────────────────────────────────────────────

async fn build_plan(
    task: &str,
    ctx: &PlannerContext<'_>,
    llm: Arc<dyn LlmProvider>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
) -> String {
    let one_shot = OneShotPlanner::new(Arc::clone(&llm))
        .with_resolve_ctx(resolve_ctx)
        .with_max_steps(10)
        .with_instructions(
            "You are planning code edits on an EXISTING codebase. \
             Each step should read, search, edit, or verify specific files. \
             Be concrete — name the files and functions to change.",
        );

    match one_shot.plan(ctx).await {
        Ok(plan) if !plan.steps.is_empty() => plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {} (tool: {})", i + 1, s.description, s.tool_name))
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(_) => {
            let iterative = IterativeRefinementPlanner::new(llm).with_max_iterations(1);
            match iterative.plan(ctx).await {
                Ok(plan) if !plan.steps.is_empty() => plan
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}. {} (tool: {})", i + 1, s.description, s.tool_name))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => task.to_string(),
            }
        }
        Err(_) => task.to_string(),
    }
}
