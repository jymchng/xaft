//! Structured multi-agent workflow for xaft.
//!
//! Mirrors the codegen CLI pipeline adapted for editing existing codebases:
//!
//! ```text
//! run_workflow()
//!   ├── Step 1: Planning (OneShotPlanner → IterativeRefinementPlanner)
//!   │            Decomposes the task into ordered steps.
//!   │
//!   ├── Step 2: Coder  (SubagentTool<EditSummary>)
//!   │            Executes the plan: reads, edits, writes, verifies.
//!   │            Terminates by returning a JSON EditSummary — not by
//!   │            running out of turns.
//!   │
//!   └── Step 3: QA ↔ Fixer  (HandoffOrchestrator, up to 2 cycles)
//!              QA reviews changed files → APPROVED or request_fix →
//!              Fixer patches in-place → back to QA.
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use agtrs_runtime::agent::{Agent, AgentConfig, AgentContext, AgentContextBuilder};
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::memory::InMemoryConversationStore;
use agtrs_runtime::planner::{IterativeRefinementPlanner, OneShotPlanner, Planner, PlannerContext};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::subagent::{ReturnMode, SubagentTool};
use agtrs_runtime::task::Intent;
use agtrs_runtime::team::{HandoffAgentStore, HandoffEvent, HandoffOrchestrator, HandoffRunParams};
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use agtrs_runtime::transport::Message;

use crate::error::RuntimeError;
use crate::session::AgentSession;
use crate::types::ExitCode;

// ── Return types ──────────────────────────────────────────────────────────────

/// Summary returned by the Coder agent when it finishes all edits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EditSummary {
    /// Workspace-relative paths of every file changed.
    pub files_changed: Vec<String>,
    /// Brief human-readable description of what was done.
    pub description: String,
    /// Whether the agent ran and passed tests (e.g. via bash_exec).
    #[serde(default)]
    pub tests_passed: bool,
    /// Optional notes (tradeoffs, limitations, skipped items).
    #[serde(default)]
    pub notes: String,
}

// ── Agent names ───────────────────────────────────────────────────────────────

const CODER_NAME: &str = "coder";
const QA_NAME: &str = "qa";
const FIXER_NAME: &str = "fixer";

// ── System prompts ────────────────────────────────────────────────────────────

fn coder_prompt(plan_text: &str) -> String {
    format!(
        "\
You are an expert software engineer. Edit files using the provided tools.

{plan_section}

WORKFLOW — follow this order exactly:
1. Call `list_files` to discover what files exist.
2. Call `read_file` with {{\"path\": \"<filename>\"}} to read each relevant file.
3. Call `grep` with {{\"pattern\": \"<search_term>\"}} to locate specific patterns.
4. For targeted edits call `edit_file` with {{\"path\": \"<f>\", \"old_content\": \"<exact>\", \"new_content\": \"<replacement>\"}}.
5. To create or fully rewrite a file call `write_file` with {{\"path\": \"<f>\", \"content\": \"<full content>\"}}.
6. Call `bash_exec` with {{\"command\": \"<cmd>\"}} to verify changes (run tests, linter, etc.).

RULES:
- Always read a file before editing it
- Supply ALL required fields in every tool call
- Make minimal targeted changes; do not rewrite code that does not need changing
- After ALL changes are done, output ONLY this JSON — no markdown, no other text:
{{\"files_changed\":[\"path/a.py\"],\"description\":\"one sentence\",\"tests_passed\":false,\"notes\":\"\"}}
",
        plan_section = if plan_text.is_empty() {
            String::new()
        } else {
            format!("PLAN — execute these steps in order:\n{plan_text}\n")
        }
    )
}

fn qa_prompt(task: &str) -> String {
    format!(
        "\
You are a code reviewer. Your job is to verify that the following task was completed correctly:

TASK: {task}

INSTRUCTIONS:
1. Call `list_files` to discover all files in the workspace.
2. Call `read_file` for each file relevant to the task.
3. Verify:
   a. The task was ACTUALLY completed (not just partially done)
   b. No syntax errors or broken imports
   c. No missing or stub implementations
   d. No logic bugs that would cause incorrect behaviour
   e. No obvious security regressions
   f. The changes are consistent and complete

Output exactly: APPROVED  — ONLY if ALL of the above pass.

If anything is wrong or the task is not fully done: call `request_fix` with a precise
description of every remaining issue. Be specific — name files, functions, and lines.
Do NOT attempt to fix anything yourself."
    )
}

const FIXER_PROMPT: &str = "\
You are a bug fixer.

INSTRUCTIONS:
1. Call `list_files` to see all files in the workspace.
2. Call `read_file` for each file that needs fixing.
3. For each file to fix, call `write_file` with the COMPLETE corrected content.
   Fix ALL reported issues. Write the full file — not just the changed lines.
4. After fixing all files, output a brief summary of what was changed.

Do NOT output file content as plain text — all changes must go through write_file.";

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
        "Report code issues to the fixer agent. Call when you find bugs, syntax errors, or broken imports."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Concise description of all issues found."
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

// ── Minimal named agent ───────────────────────────────────────────────────────

struct NamedAgent {
    name: String,
    config: AgentConfig,
    tools: Vec<Arc<ErasedTool>>,
}

impl NamedAgent {
    fn new(name: &str, system_prompt: &str, max_turns: usize) -> Self {
        Self {
            name: name.to_string(),
            config: AgentConfig {
                system_prompt: system_prompt.to_string(),
                max_turns,
                strict_capability_check: false,
                parallel_tool_calls: true,
                ..Default::default()
            },
            tools: Vec::new(),
        }
    }

    fn with_tools(mut self, tools: Vec<Arc<ErasedTool>>) -> Self {
        self.tools = tools;
        self
    }
}

#[async_trait::async_trait]
impl Agent for NamedAgent {
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
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the full xaft orchestrated workflow and return `(content, exit_code)`.
///
/// `read_tools` — read-only tools (list_files, read_file, grep)
/// `write_tools` — all tools including write/edit/bash
pub async fn run_workflow(
    task: &str,
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    read_tools: Vec<Arc<ErasedTool>>,
    write_tools: Vec<Arc<ErasedTool>>,
    session: &mut AgentSession,
    headless: bool,
) -> Result<(String, ExitCode), RuntimeError> {
    let run_id = session.id.to_string();

    // ── Step 1: Plan ──────────────────────────────────────────────────────────
    let tool_names: Vec<String> = write_tools.iter().map(|t| t.name().into()).collect();
    let intent = Intent::from_goal(task).build();
    let plan_ctx = PlannerContext::initial(&intent, tool_names);

    let plan_text = build_plan(task, &plan_ctx, Arc::clone(&llm), Arc::clone(&resolve_ctx)).await;
    info!(task, steps = ?plan_text.lines().count(), "xaft: plan ready");
    if !headless {
        eprintln!("[Plan]\n{plan_text}\n");
    }

    // ── Step 2: Coder ─────────────────────────────────────────────────────────
    let coder_prompt = coder_prompt(&plan_text);
    let coder_agent =
        Arc::new(NamedAgent::new(CODER_NAME, &coder_prompt, 40).with_tools(write_tools.clone()));

    let coder_tool = SubagentTool::<EditSummary>::builder()
        .name(CODER_NAME)
        .description("Execute the coding plan")
        .subagent(Arc::clone(&coder_agent) as Arc<dyn Agent>)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .system_prompt(&coder_prompt)
        .max_turns(40)
        .return_mode(ReturnMode::DirectJson)
        .signals(Arc::clone(&signals))
        .build();

    let edit_summary = match coder_tool.run(task.to_string()).await {
        Ok(s) => {
            info!(
                files = ?s.files_changed,
                tests_passed = s.tests_passed,
                "xaft: coder done"
            );
            if !headless {
                eprintln!("[Coder] Changed: {:?}", s.files_changed);
                if !s.description.is_empty() {
                    eprintln!("[Coder] {}", s.description);
                }
            }
            s
        }
        Err(e) => {
            warn!(error = %e, "xaft: coder summary parse failed — proceeding to QA anyway");
            EditSummary {
                files_changed: vec![],
                description: format!("coder completed (summary parse failed: {e})"),
                tests_passed: false,
                notes: String::new(),
            }
        }
    };

    session.turn_count += 1;

    // ── Step 3: QA ↔ Fixer ───────────────────────────────────────────────────
    let handoff_store = Arc::new(HandoffAgentStore::new());
    let conv_store = Arc::new(InMemoryConversationStore::new());
    let fix_tool = Arc::new(RequestFixTool {
        store: Arc::clone(&handoff_store),
    }) as Arc<ErasedTool>;

    let mut qa_tools = read_tools.clone();
    qa_tools.push(Arc::clone(&fix_tool));

    let qa_agent = Arc::new(NamedAgent::new(QA_NAME, &qa_prompt(task), 20).with_tools(qa_tools));
    let fixer_agent =
        Arc::new(NamedAgent::new(FIXER_NAME, FIXER_PROMPT, 20).with_tools(write_tools.clone()));

    let orchestrator = HandoffOrchestrator::builder()
        .agent(QA_NAME, Arc::clone(&qa_agent) as Arc<dyn Agent>)
        .agent(FIXER_NAME, Arc::clone(&fixer_agent) as Arc<dyn Agent>)
        .conv_store(Arc::new(InMemoryConversationStore::new()))
        .agent_store(Arc::clone(&handoff_store))
        // Allow up to 5 QA→Fixer cycles; QA naturally stops by outputting APPROVED
        .max_handoffs(5)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .prompt_fn(|ctx| {
            format!(
                "The code reviewer found these issues:\n\
                 {}\n\n\
                 Original task for context:\n\
                 {}\n\n\
                 Fix ALL listed issues completely. \
                 The reviewer will check your work again after you are done.",
                ctx.summary, ctx.original_message
            )
        })
        .build();

    let conv_id = format!("{run_id}::qa");
    let changed_list = if edit_summary.files_changed.is_empty() {
        "Use list_files to discover changed files.".to_string()
    } else {
        format!("Files changed by coder: {:?}", edit_summary.files_changed)
    };
    let qa_message = format!(
        "Review the code changes made for this task: {task}\n\n{changed_list}\n\
         Use list_files + read_file to inspect the changes. \
         Output APPROVED if everything is correct, or call request_fix if there are issues."
    );

    let mut qa_stream = orchestrator.run_stream(HandoffRunParams {
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
    });

    let mut final_content = edit_summary.description.clone();
    let mut approved = false;

    while let Some(event) = qa_stream.next().await {
        match event {
            HandoffEvent::AgentHandoff {
                from_agent,
                to_agent,
                summary,
            } => {
                info!(from = %from_agent, to = %to_agent, summary = %summary, "xaft: handoff");
                if !headless {
                    eprintln!("[QA→Fixer] {summary}");
                }
            }
            HandoffEvent::AgentEvent(agtrs_runtime::streaming::StreamEvent::Done {
                content,
                agent_name,
                turns,
                ..
            }) => {
                if agent_name == QA_NAME
                    && (content.trim() == "APPROVED" || content.to_uppercase().contains("APPROVED"))
                {
                    approved = true;
                    info!("xaft: QA approved");
                    if !headless {
                        eprintln!("[QA] ✓ Approved");
                    }
                }
                if !content.is_empty() {
                    final_content = content;
                }
                session.turn_count += turns as u32;
            }
            HandoffEvent::Completed => {
                info!(approved, "xaft: QA/Fixer cycle complete");
            }
            HandoffEvent::AgentEvent(agtrs_runtime::streaming::StreamEvent::TextDelta {
                delta,
            }) => {
                if !headless {
                    eprint!("{delta}");
                }
            }
            _ => {}
        }
    }

    if !headless {
        eprintln!();
    }

    Ok((final_content, ExitCode::SUCCESS))
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
             Each step should read, search, edit, or verify a specific file/function. \
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
            // Empty plan — escalate to iterative refinement
            let iterative = IterativeRefinementPlanner::new(llm).with_max_iterations(1);
            match iterative.plan(ctx).await {
                Ok(plan) => plan
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}. {} (tool: {})", i + 1, s.description, s.tool_name))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(_) => task.to_string(),
            }
        }
        Err(_) => task.to_string(), // fallback: pass task directly as "plan"
    }
}
