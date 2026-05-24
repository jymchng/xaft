# Agent Lifecycle

## Agent Roster

`xaft` ships with six specialized agents. Each is a concrete `Agent` implementation registered in the DI container as a singleton.

| Agent | Responsibility | Max Turns | Model Class |
|---|---|---|---|
| `PlannerAgent` | Decomposes intent into structured plan | 3 | Capable |
| `CodeAgent` | Implements plan steps via file edits | 20 | Capable |
| `ReviewAgent` | Reviews diffs for correctness and style | 5 | Capable |
| `FixerAgent` | Diagnoses and repairs test/compile failures | 10 | Capable |
| `IndexAgent` | Builds and queries semantic index | 5 | Cheap |
| `SummaryAgent` | Condenses context windows | 2 | Cheap |

## Agent Construction (via DI)

```rust
// xaft-agents/src/code_agent.rs
#[agent(
    name = "code",
    description = "Implements code changes in the repository",
    tools = [
        ReadFileTool, WriteFileTool, ApplyPatchTool,
        ListFilesTool, SearchCodeTool, RunCargoTool,
        GitStatusTool, GitDiffTool,
    ],
    max_turns = 20,
    temperature = 0.2,
    max_cost_usd = 2.00,
    parallel_tool_calls = true,
    tool_error_policy = "return_error",
    summarize_at = 0.80,
)]
#[injectable]
pub struct CodeAgent {
    #[injectable(inject)] llm: Inject<dyn LlmProvider>,
    #[injectable(inject)] workspace: Inject<WorkspaceEditor>,
    #[injectable(inject)] git: Inject<GitRepo>,
}

#[async_trait]
impl Agent for CodeAgent {
    fn name(&self) -> &str { "code" }

    fn system_prompt(&self) -> String {
        format!(
            r#"You are an expert Rust software engineer working in the repository at {root}.

Your job is to implement code changes requested by the task plan. Follow these principles:

1. Read files before modifying them to understand current state.
2. Make minimal, focused changes — do not refactor unrelated code.
3. After writing files, run `cargo check` to catch syntax errors early.
4. When tests fail, analyze the error carefully before attempting a fix.
5. Commit your changes with a clear, descriptive message.

Repository structure:
{structure}

Active worktree: {worktree}
Current plan step: {step}
"#,
            root = self.workspace.root().display(),
            structure = "... injected at runtime ...",
            worktree = "... injected at runtime ...",
            step = "... injected at runtime ...",
        )
    }

    fn tools(&self) -> Vec<Arc<ErasedTool>> {
        // macro-generated from tools = [...] annotation
        vec![ /* ReadFileTool, WriteFileTool, ... */ ]
    }

    fn config(&self) -> &AgentConfig {
        // macro-generated
        static CFG: OnceLock<AgentConfig> = OnceLock::new();
        CFG.get_or_init(|| AgentConfig {
            max_turns: 20,
            temperature: 0.2,
            max_cost_usd: Some(2.00),
            parallel_tool_calls: true,
            summarize_at: Some(0.80),
            ..Default::default()
        })
    }

    async fn on_start(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
        // Load workspace structure into context state
        let structure = self.workspace.summarize_structure().await?;
        ctx.set_context_state("workspace_structure", serde_json::json!(structure));

        // Load current worktree path
        if let Some(wt) = ctx.context_state().get("active_worktree") {
            ctx.set_context_state("worktree", wt.clone());
        }
        Ok(())
    }

    async fn on_complete(&self, ctx: &mut AgentContext, response: &AgentResponse) -> Result<(), AgtrsError> {
        // Auto-stage all modified files after code agent completes
        let modified = self.workspace.list_modified().await?;
        if !modified.is_empty() {
            self.git.stage_files(&modified).await?;
        }
        Ok(())
    }
}
```

## Agent Lifecycle Stages

```
Construction (DI container resolve)
    │
    ▼
Pre-run validation
    │ agent.config().strict_capability_check
    │ llm.supports_tool_calling() → if false and tools present: error
    │ cost budget check (previous session spend)
    ▼
on_start(ctx)
    │ Load workspace context into ctx.context_state
    │ Load conversation history if resuming
    │ Initialize per-run metrics
    ▼
ReAct Loop (AgentExecutor::run)
    │ [turn 1..max_turns]
    │   check_cancel!()
    │   run input guardrails
    │   llm.complete(messages, options)
    │   run output guardrails
    │   if tool_calls: execute tools (parallel or sequential)
    │   accumulate tool results
    │   if StopReason::EndTurn: break
    │   check cost budget
    ▼
on_complete(ctx, response)
    │ Stage modified files
    │ Save conversation to ConversationStore
    │ Emit AgentRunComplete signal
    ▼
Return AgentResponse
```

## PlannerAgent — Special Lifecycle

The `PlannerAgent` runs before `CodeAgent` and produces a `Plan` rather than code. Its output is typed via `StructuredLlm<Plan>`.

```rust
pub struct PlannerAgent {
    llm: Arc<dyn LlmProvider>,
    planner: Arc<dyn Planner>,   // OneShotPlanner or IterativeRefinementPlanner
}

impl PlannerAgent {
    pub async fn plan(&self, intent: &Intent, tools: Vec<String>) -> Result<Plan, XaftError> {
        let ctx = PlannerContext::initial(intent, tools);
        let plan = self.planner.plan(&ctx).await?;
        Ok(plan)
    }

    pub async fn replan(
        &self,
        intent: &Intent,
        completed: Vec<PlanStep>,
        failed: Option<PlanStep>,
        reason: &str,
        tools: Vec<String>,
    ) -> Result<Plan, XaftError> {
        let ctx = PlannerContext::replan(intent, completed, failed, Some(reason.into()), tools);
        let plan = self.planner.plan(&ctx).await?;
        Ok(plan)
    }
}
```

## FixerAgent — QA Loop Integration

The `FixerAgent` is invoked when `CodeAgent` produces code that fails `cargo test`:

```rust
pub struct FixerAgent {
    llm: Arc<dyn LlmProvider>,
    workspace: Arc<WorkspaceEditor>,
    shell: Arc<ShellExecutor>,
}

impl FixerAgent {
    pub async fn fix_and_verify(
        &self,
        error_output: &str,
        affected_files: &[PathBuf],
        max_iterations: u32,
    ) -> Result<FixResult, XaftError> {
        for i in 0..max_iterations {
            // Run agent with error context
            let mut ctx = self.build_fixer_ctx(error_output, affected_files, i);
            let response = AgentExecutor::run(self, Message::user(
                format!("Fix these errors:\n```\n{error_output}\n```")
            ), &mut ctx).await?;

            // Verify
            let test_out = self.shell.run("cargo test --workspace 2>&1", None).await?;
            if test_out.exit_code == 0 {
                return Ok(FixResult { success: true, iterations: i + 1 });
            }
            // Update error_output for next iteration
        }
        Ok(FixResult { success: false, iterations: max_iterations })
    }
}
```

## Context Injection Between Agents

When the `PlanExecutor` transitions from one agent to another (e.g., `CodeAgent` → `FixerAgent`), it passes context through `AgentContext::context_state`:

```rust
// After CodeAgent completes, before FixerAgent starts:
fixer_ctx.set_context_state("prior_agent", serde_json::json!("code"));
fixer_ctx.set_context_state("prior_output", serde_json::to_value(&code_response)?);
fixer_ctx.set_context_state("affected_files", serde_json::to_value(&modified_files)?);
fixer_ctx.set_context_state("test_error", serde_json::json!(test_stderr));
```

## SubAgent Delegation Pattern

`CodeAgent` delegates isolated subtasks to subagents via `SubagentTool`. Each subagent gets:
- A fresh `AgentContext` (no parent conversation history)
- A bounded token/cost budget
- A specific structured output type

```rust
// CodeAgent registers subagent tools during on_start:
let reviewer = SubagentTool::<ReviewResult>::builder()
    .name("review_diff")
    .description("Review a code diff for correctness and potential issues")
    .subagent(Arc::new(ReviewAgent::new()))
    .llm(Arc::clone(&cheap_llm))  // ReviewAgent can use cheaper model
    .max_turns(5)
    .max_cost_usd(0.10)
    .return_mode(ReturnMode::StructuredLlm)
    .build();

ctx.register_tool("review_diff", Arc::new(reviewer));
```

## Agent Memory Lifecycle

```
Session start
    │ ConversationStore::load(session_id) → previous messages
    │ UserMemoryStore::search(user_id, topic) → relevant facts
    ▼
Each turn
    │ ShortTermMemory::add_user/assistant/tool_result
    │ ShortTermMemory::trim_to_fit(memory_window_tokens)
    │ If summarize_at threshold crossed:
    │   SummaryAgent::summarize(oldest messages)
    │   ShortTermMemory::set_summary(summary_text)
    ▼
Session end
    │ ConversationStore::save(session_id, all_messages)
    │ Extract and save new user memory facts
```

## References

- agtrs: `agtrs-runtime/src/agent.rs`, `agtrs-runtime/src/executor.rs`
- agtrs: `agtrs-runtime/src/subagent.rs`
- Next: [Event Bus Architecture →](03_event_bus.md)