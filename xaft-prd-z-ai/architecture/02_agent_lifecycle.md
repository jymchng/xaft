# 02 — Agent Lifecycle

> Full agent lifecycle: instantiation through every turn to shutdown.
> How xaft wraps the agtrs Agent trait with custom hooks for git, terminal streaming, and planning.

---

## Overview

xaft's agent lifecycle is built on top of the agtrs `Agent` trait. Every interaction with the LLM, every tool dispatch, and every state transition flows through well-defined lifecycle methods. xaft implements custom hooks at each phase to inject git operations, terminal streaming, cost tracking, and plan-mode behavior.

The lifecycle is a nested structure:

```
Agent Lifetime
├── on_start()              — once, before any turns
├── Turn Loop
│   ├── before_llm_call()   — before each LLM invocation
│   ├── LLM Call            — the actual completion request
│   ├── after_llm_call()    — after LLM response received
│   ├── Tool Dispatch       — execute tool calls from LLM response
│   │   ├── before_tool()   — per-tool hook
│   │   ├── tool.execute()  — the actual tool execution
│   │   └── after_tool()    — per-tool hook
│   ├── on_tool_result()    — after all tools in this turn complete
│   └── on_turn_complete()  — end of turn bookkeeping
└── on_finish()             — once, after all turns complete
```

---

## The Agent Trait (agtrs)

The base `Agent` trait from agtrs defines the lifecycle interface:

```rust
/// Core agent trait from the agtrs framework.
/// xaft implements this trait with custom behavior for each lifecycle method.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Unique identifier for this agent instance.
    fn id(&self) -> &AgentId;

    /// Display name for logging and TUI.
    fn name(&self) -> &str;

    /// The system prompt injected at the beginning of every conversation.
    fn system_prompt(&self) -> &str;

    /// Available tools for this agent.
    fn tools(&self) -> &[ErasedTool];

    /// Maximum number of turns before forced termination.
    fn max_turns(&self) -> u32 { 50 }

    /// Called once before the turn loop begins.
    /// Use for initialization, context injection, and pre-flight checks.
    async fn on_start(&mut self, ctx: &mut AgentContext) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called before each LLM invocation.
    /// Can modify the request (add context, adjust temperature, etc.)
    async fn before_llm_call(
        &mut self,
        request: &mut LlmRequest,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called after each LLM response is received.
    /// Can inspect the response, log, or trigger side effects.
    async fn after_llm_call(
        &mut self,
        response: &LlmResponse,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called before each tool execution.
    /// Return Err to veto the tool call.
    async fn before_tool(
        &mut self,
        tool_name: &str,
        args: &str,
        ctx: &mut AgentContext,
    ) -> Result<ToolVerdict, AgentError> {
        Ok(ToolVerdict::Allow)
    }

    /// Called after each tool execution.
    /// Can modify the result before it's returned to the agent.
    async fn after_tool(
        &mut self,
        tool_name: &str,
        result: &mut ToolOutput,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called after all tool results for a turn are collected.
    async fn on_tool_result(
        &mut self,
        results: &[(String, ToolOutput)],
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called at the end of each turn (after tool results are processed).
    /// Return false to continue, true to stop the agent loop.
    async fn on_turn_complete(
        &mut self,
        turn: u32,
        ctx: &mut AgentContext,
    ) -> Result<bool, AgentError> {
        Ok(false)
    }

    /// Called once after the turn loop ends (either natural or forced).
    async fn on_finish(
        &mut self,
        outcome: &AgentOutcome,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        Ok(())
    }
}

/// Verdict from before_tool: allow, deny, or redirect.
pub enum ToolVerdict {
    Allow,
    Deny { reason: String },
    Redirect { tool_name: String, args: String },
}

/// Context passed to every lifecycle method.
pub struct AgentContext {
    pub conversation: ConversationStore,
    pub memory: MemoryStore,
    pub scratchpad: Scratchpad,
    pub workspace: Arc<dyn WorkspaceStore>,
    pub git: GitRepo,
    pub signal_bus: Arc<SignalBus>,
    pub cost_tracker: Arc<CostTracker>,
    pub cancellation_token: CancellationToken,
    pub config: Arc<XaftConfig>,
}
```

---

## XaftAgent Implementation

xaft's primary agent implementation wraps the `Agent` trait with concrete behavior:

```rust
/// The main xaft agent — a coding-focused implementation of Agent.
pub struct XaftAgent {
    // ── Identity ──────────────────────────────────
    id: AgentId,
    name: String,

    // ── Configuration ─────────────────────────────
    system_prompt: String,
    tools: Vec<ErasedTool>,
    max_turns: u32,

    // ── State ─────────────────────────────────────
    plan: Option<TaskPlan>,
    turn_count: u32,
    total_cost: f64,

    // ── Hooks ─────────────────────────────────────
    git_hook: GitAutoCommitHook,
    streaming_hook: TerminalStreamingHook,
    cost_hook: CostTrackingHook,
    guardrails: Vec<Box<dyn Guardrail>>,
}

#[async_trait]
impl Agent for XaftAgent {
    fn id(&self) -> &AgentId { &self.id }
    fn name(&self) -> &str { &self.name }
    fn system_prompt(&self) -> &str { &self.system_prompt }
    fn tools(&self) -> &[ErasedTool] { &self.tools }
    fn max_turns(&self) -> u32 { self.max_turns }

    async fn on_start(&mut self, ctx: &mut AgentContext) -> Result<(), AgentError> {
        // ── Emit session start event ──────────────
        ctx.signal_bus.emit(Signal::AgentStarted {
            agent_id: self.id.clone(),
            plan: self.plan.clone(),
        })?;

        // ── Load relevant memory ──────────────────
        let relevant = ctx.memory.search(&ctx.scratchpad.get("task")?, 10).await?;
        if !relevant.is_empty() {
            let memory_context = format!(
                "## Relevant Past Experience\n{}",
                relevant.iter()
                    .map(|m| format!("- {}", m.summary))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            ctx.scratchpad.set("memory_context", memory_context)?;
        }

        // ── Initialize git branch ─────────────────
        if ctx.config.git.branch_per_task {
            let branch_name = format!(
                "{}{}",
                ctx.config.git.branch_prefix,
                self.plan.as_ref()
                    .map(|p| slugify(&p.summary))
                    .unwrap_or_else(|| "unnamed".to_string())
            );
            ctx.git.create_branch(&branch_name)?;
            ctx.signal_bus.emit(Signal::BranchCreated { name: branch_name })?;
        }

        // ── Run guardrails pre-check ──────────────
        for guardrail in &self.guardrails {
            guardrail.pre_check(&ctx.scratchpad, &*ctx.workspace)?;
        }

        Ok(())
    }

    async fn before_llm_call(
        &mut self,
        request: &mut LlmRequest,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // ── Inject plan context if available ──────
        if let Some(ref plan) = self.plan {
            let plan_context = format!(
                "## Execution Plan\n{}\n\nCurrent step: {}",
                plan.summary,
                plan.steps.get(self.turn_count as usize)
                    .map(|s| s.description.as_str())
                    .unwrap_or("Continuing execution")
            );
            request.messages.push(Message::system(&plan_context));
        }

        // ── Inject scratchpad state ───────────────
        let scratchpad_context = ctx.scratchpad.dump();
        if !scratchpad_context.is_empty() {
            request.messages.push(Message::system(&format!(
                "## Working Memory\n{}",
                scratchpad_context
            )));
        }

        // ── Budget pre-check ──────────────────────
        if ctx.cost_tracker.remaining() < 0.01 {
            ctx.signal_bus.emit(Signal::BudgetExhausted)?;
            return Err(AgentError::BudgetExceeded);
        }

        // ── Cancellation check ────────────────────
        if ctx.cancellation_token.is_cancelled() {
            return Err(AgentError::Cancelled);
        }

        ctx.signal_bus.emit(Signal::BeforeLlmCall {
            turn: self.turn_count,
            model: request.model.clone(),
        })?;

        Ok(())
    }

    async fn after_llm_call(
        &mut self,
        response: &LlmResponse,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // ── Track cost ────────────────────────────
        self.cost_hook.on_llm_response(response, &ctx.cost_tracker);

        // ── Emit event ────────────────────────────
        ctx.signal_bus.emit(Signal::AfterLlmCall {
            turn: self.turn_count,
            tokens_used: response.usage.total_tokens,
            cost_usd: response.cost,
            tool_calls: response.tool_calls.len(),
        })?;

        // ── Persist to conversation store ─────────
        ctx.conversation.append_assistant_message(
            &response.content,
            &response.tool_calls,
        ).await?;

        Ok(())
    }

    async fn before_tool(
        &mut self,
        tool_name: &str,
        args: &str,
        ctx: &mut AgentContext,
    ) -> Result<ToolVerdict, AgentError> {
        // ── Run guardrails ────────────────────────
        for guardrail in &self.guardrails {
            match guardrail.check_tool_call(tool_name, args)? {
                GuardrailVerdict::Allow => continue,
                GuardrailVerdict::Deny(reason) => {
                    ctx.signal_bus.emit(Signal::ToolBlocked {
                        tool: tool_name.to_string(),
                        reason: reason.clone(),
                    })?;
                    return Ok(ToolVerdict::Deny { reason });
                }
                GuardrailVerdict::Modify(new_args) => {
                    // Guardrail modified the args; continue with modified args
                    return Ok(ToolVerdict::Redirect {
                        tool_name: tool_name.to_string(),
                        args: new_args,
                    });
                }
            }
        }

        // ── Emit pre-tool event ───────────────────
        ctx.signal_bus.emit(Signal::BeforeTool {
            tool: tool_name.to_string(),
            turn: self.turn_count,
        })?;

        Ok(ToolVerdict::Allow)
    }

    async fn after_tool(
        &mut self,
        tool_name: &str,
        result: &mut ToolOutput,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // ── Git auto-commit hook ──────────────────
        self.git_hook.on_tool_result(tool_name, result, ctx).await?;

        // ── Streaming hook ────────────────────────
        self.streaming_hook.on_tool_result(tool_name, result, ctx)?;

        // ── Update scratchpad with tool results ───
        if result.is_ok() {
            ctx.scratchpad.append(
                &format!("tool_results_{}", self.turn_count),
                &format!("[{}] success\n", tool_name),
            )?;
        }

        Ok(())
    }

    async fn on_tool_result(
        &mut self,
        results: &[(String, ToolOutput)],
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // ── Persist tool results to conversation ───
        for (tool_name, output) in results {
            ctx.conversation.append_tool_result(
                tool_name,
                &output.to_string(),
            ).await?;
        }

        // ── Update scratchpad with summary ────────
        let summary = results.iter()
            .map(|(name, output)| format!("- {}: {}", name, output.summary()))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.scratchpad.set(
            &format!("turn_{}_results", self.turn_count),
            summary,
        )?;

        ctx.signal_bus.emit(Signal::ToolResultsCollected {
            turn: self.turn_count,
            count: results.len(),
        })?;

        Ok(())
    }

    async fn on_turn_complete(
        &mut self,
        turn: u32,
        ctx: &mut AgentContext,
    ) -> Result<bool, AgentError> {
        self.turn_count = turn;

        // ── Progress update ───────────────────────
        ctx.signal_bus.emit(Signal::TurnComplete {
            turn,
            cumulative_cost: self.total_cost,
            files_modified: ctx.workspace.dirty_files().len(),
        })?;

        // ── Plan step completion check ────────────
        if let Some(ref plan) = self.plan {
            if (turn as usize) < plan.steps.len() {
                ctx.signal_bus.emit(Signal::PlanStepComplete {
                    step: turn as usize,
                    total: plan.steps.len(),
                    description: plan.steps[turn as usize].description.clone(),
                })?;
            }

            // If all plan steps are complete, signal completion
            if (turn as usize) >= plan.steps.len() {
                ctx.signal_bus.emit(Signal::PlanComplete)?;
                return Ok(true); // Stop the agent loop
            }
        }

        // ── Max turns check ───────────────────────
        if turn >= self.max_turns {
            ctx.signal_bus.emit(Signal::MaxTurnsReached { max: self.max_turns })?;
            return Ok(true); // Stop the agent loop
        }

        Ok(false) // Continue the loop
    }

    async fn on_finish(
        &mut self,
        outcome: &AgentOutcome,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // ── Persist final memory ──────────────────
        let memory_entry = MemoryEntry {
            task: ctx.scratchpad.get("task")?,
            summary: outcome.summary.clone(),
            lessons_learned: self.extract_lessons(outcome),
            tools_used: outcome.tools_used.clone(),
            cost_usd: self.total_cost,
            timestamp: chrono::Utc::now(),
        };
        ctx.memory.store(memory_entry).await?;

        // ── Final git commit ──────────────────────
        if ctx.workspace.has_uncommitted_changes()? && ctx.config.workspace.auto_commit {
            let msg = format!(
                "xaft: completed - {}",
                outcome.summary.chars().take(60).collect::<String>()
            );
            ctx.git.commit_all(&msg)?;
            ctx.signal_bus.emit(Signal::AutoCommit { message: msg })?;
        }

        // ── Emit completion event ─────────────────
        ctx.signal_bus.emit(Signal::AgentFinished {
            agent_id: self.id.clone(),
            outcome: outcome.clone(),
            total_turns: self.turn_count,
            total_cost: self.total_cost,
        })?;

        Ok(())
    }
}
```

---

## PlanModeAgent Integration

The `PlanModeAgent` is a specialized agent that first plans, then executes. It wraps a standard `XaftAgent` but adds a planning phase before the main ReAct loop.

```rust
/// Agent that plans before executing.
/// Phase 1: Generate a plan using a structured LLM call.
/// Phase 2: Execute the plan using the standard XaftAgent ReAct loop.
pub struct PlanModeAgent {
    /// The inner agent that executes the plan.
    inner: XaftAgent,

    /// Planner instance for generating the plan.
    planner: Box<dyn Planner>,

    /// Current phase: planning or executing.
    phase: PlanPhase,

    /// The generated plan (set during planning phase).
    plan: Option<TaskPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanPhase {
    /// Agent is generating a plan via the planner.
    Planning,

    /// Plan has been generated; agent is now executing it.
    Executing { plan: TaskPlan },

    /// Plan execution is complete.
    Completed,

    /// Planning failed; falling back to direct execution.
    FallbackDirect,
}

#[async_trait]
impl Agent for PlanModeAgent {
    fn id(&self) -> &AgentId { self.inner.id() }
    fn name(&self) -> &str { "plan-mode-agent" }
    fn system_prompt(&self) -> &str { self.inner.system_prompt() }
    fn tools(&self) -> &[ErasedTool] { self.inner.tools() }
    fn max_turns(&self) -> u32 { self.inner.max_turns() + 5 } // extra turns for planning

    async fn on_start(&mut self, ctx: &mut AgentContext) -> Result<(), AgentError> {
        // ── Phase 1: Planning ────────────────────
        self.phase = PlanPhase::Planning;

        ctx.signal_bus.emit(Signal::PlanningStarted {
            planner: self.planner.name().to_string(),
        })?;

        let task_description = ctx.scratchpad.get("task")?;

        match self.planner.plan(&task_description, &*ctx.workspace).await {
            Ok(plan) => {
                ctx.signal_bus.emit(Signal::PlanCreated(plan.clone()))?;
                self.plan = Some(plan.clone());
                self.inner.plan = Some(plan.clone());
                self.phase = PlanPhase::Executing { plan };
            }
            Err(e) => {
                ctx.signal_bus.emit(Signal::PlanningFailed {
                    error: e.to_string(),
                })?;
                self.phase = PlanPhase::FallbackDirect;
                // Continue without a plan — the inner agent will work without guidance
            }
        }

        // ── Delegate to inner agent ──────────────
        self.inner.on_start(ctx).await
    }

    async fn before_llm_call(
        &mut self,
        request: &mut LlmRequest,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // Inject plan awareness into the request
        if let PlanPhase::Executing { ref plan } = self.phase {
            let plan_instruction = format!(
                "## Your Plan (follow it step by step)\n\
                 {}\n\n\
                 ## Steps\n\
                 {}\n\n\
                 You are on step {}/{}. Complete this step, then move to the next.\
                 Do NOT skip steps or jump ahead.",
                plan.summary,
                plan.steps.iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}. {} {}", i + 1, if i == 0 { "→" } else { " " }, s.description))
                    .collect::<Vec<_>>()
                    .join("\n"),
                self.inner.turn_count + 1,
                plan.steps.len(),
            );
            request.messages.push(Message::system(&plan_instruction));
        }

        self.inner.before_llm_call(request, ctx).await
    }

    async fn after_llm_call(
        &mut self,
        response: &LlmResponse,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        self.inner.after_llm_call(response, ctx).await
    }

    async fn before_tool(
        &mut self,
        tool_name: &str,
        args: &str,
        ctx: &mut AgentContext,
    ) -> Result<ToolVerdict, AgentError> {
        self.inner.before_tool(tool_name, args, ctx).await
    }

    async fn after_tool(
        &mut self,
        tool_name: &str,
        result: &mut ToolOutput,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        self.inner.after_tool(tool_name, result, ctx).await
    }

    async fn on_tool_result(
        &mut self,
        results: &[(String, ToolOutput)],
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        self.inner.on_tool_result(results, ctx).await
    }

    async fn on_turn_complete(
        &mut self,
        turn: u32,
        ctx: &mut AgentContext,
    ) -> Result<bool, AgentError> {
        self.inner.on_turn_complete(turn, ctx).await
    }

    async fn on_finish(
        &mut self,
        outcome: &AgentOutcome,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        self.phase = PlanPhase::Completed;
        self.inner.on_finish(outcome, ctx).await
    }
}
```

---

## Memory Architecture

xaft uses three complementary memory systems from agtrs:

```
┌────────────────────────────────────────────────────────────────┐
│                     Memory Architecture                         │
│                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────┐ │
│  │ ConversationStore│  │   MemoryStore    │  │  Scratchpad  │ │
│  │                  │  │                  │  │              │ │
│  │ Full conversation│  │ Cross-session    │  │ Per-turn     │ │
│  │ history (all     │  │ lessons learned, │  │ working      │ │
│  │ messages, tool   │  │ patterns, errors │  │ memory       │ │
│  │ calls, results)  │  │                  │  │              │ │
│  │                  │  │ Persists across  │  │ Reset each   │ │
│  │ Persisted to:    │  │ sessions         │  │ turn or kept │ │
│  │ SQLite / JSON    │  │                  │  │ as needed    │ │
│  │                  │  │ Persisted to:    │  │              │ │
│  │ Used for:        │  │ SQLite           │  │ Used for:    │ │
│  │ - Resume         │  │                  │  │ - Plan state │ │
│  │ - Replay         │  │ Used for:        │  │ - Accumulated│ │
│  │ - Context window │  │ - Avoid repeated │  │   results    │ │
│  │   management     │  │   mistakes       │  │ - Task state │ │
│  └──────────────────┘  │ - Pattern reuse  │  └──────────────┘ │
│                        │ - Cost estimation│                    │
│                        └──────────────────┘                    │
└────────────────────────────────────────────────────────────────┘
```

### ConversationStore Usage

```rust
/// How xaft uses ConversationStore throughout the lifecycle.
impl ConversationStore for SqliteConversationStore {
    /// Append a user message to the conversation.
    async fn append_user_message(&self, content: &str) -> Result<()>;

    /// Append an assistant message (LLM response) with any tool calls.
    async fn append_assistant_message(
        &self,
        content: &str,
        tool_calls: &[ToolCall],
    ) -> Result<()>;

    /// Append a tool result message.
    async fn append_tool_result(
        &self,
        tool_name: &str,
        result: &str,
    ) -> Result<()>;

    /// Get the full conversation as LLM messages, truncated to fit context window.
    async fn get_messages(&self, max_tokens: usize) -> Result<Vec<Message>>;

    /// Get a summary of the conversation for context compression.
    async fn summarize(&self, max_tokens: usize) -> Result<String>;

    /// Persist the current conversation state.
    async fn persist(&self, session: &AgentSession) -> Result<()>;

    /// Load a conversation for session resume.
    async fn load(&self, session_id: &SessionId) -> Result<Conversation>;
}
```

### Context Window Management

xaft manages the LLM context window by truncating conversation history when it exceeds the model's limit:

```rust
/// Manage context window within the before_llm_call hook.
async fn manage_context_window(
    conversation: &dyn ConversationStore,
    max_tokens: usize,
    system_prompt_tokens: usize,
) -> Result<Vec<Message>, AgentError> {
    let budget = max_tokens.saturating_sub(system_prompt_tokens);

    // Strategy: keep system prompt + last N messages + summaries of older messages
    let mut messages = conversation.get_messages(budget).await?;

    // If we're over budget, apply progressive truncation:
    // 1. Summarize messages older than the last 10
    // 2. Remove tool results older than the last 5 turns
    // 3. As a last resort, truncate the oldest messages

    let total_tokens: usize = messages.iter().map(|m| m.estimated_tokens()).sum();

    if total_tokens > budget {
        // Step 1: Replace old messages with summaries
        let cutoff = messages.len().saturating_sub(10);
        if cutoff > 0 {
            let summary = conversation.summarize(budget / 4).await?;
            messages = vec![Message::system(&format!("Previous conversation summary:\n{}", summary))]
                .into_iter()
                .chain(messages.into_iter().skip(cutoff))
                .collect();
        }
    }

    Ok(messages)
}
```

---

## Lifecycle Hooks

xaft implements three primary lifecycle hooks that compose with the agent:

### GitAutoCommitHook

```rust
/// Automatically commits file changes after tool execution.
pub struct GitAutoCommitHook {
    /// Track which tools modify files.
    file_modifying_tools: HashSet<String>,
    /// Minimum number of changes before auto-commit.
    commit_threshold: usize,
    /// Accumulated changes since last commit.
    pending_changes: Vec<PathBuf>,
}

impl GitAutoCommitHook {
    pub fn on_tool_result(
        &mut self,
        tool_name: &str,
        result: &mut ToolOutput,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        // Only commit after file-modifying tools
        if !self.file_modifying_tools.contains(tool_name) {
            return Ok(());
        }

        // Track modified files
        if let Some(files) = result.modified_files() {
            self.pending_changes.extend(files);
        }

        // Auto-commit if threshold reached
        if self.pending_changes.len() >= self.commit_threshold {
            let msg = format!(
                "xaft: auto-commit after {} file changes (turn {})",
                self.pending_changes.len(),
                // turn count from context
            );
            ctx.git.commit_specific(&self.pending_changes, &msg)?;
            ctx.signal_bus.emit(Signal::AutoCommit { message: msg })?;
            self.pending_changes.clear();
        }

        Ok(())
    }
}
```

### TerminalStreamingHook

```rust
/// Hooks into tool results to stream progress to the terminal.
pub struct TerminalStreamingHook {
    output: Arc<dyn OutputSink>,
}

impl TerminalStreamingHook {
    pub fn on_tool_result(
        &self,
        tool_name: &str,
        result: &mut ToolOutput,
        ctx: &mut AgentContext,
    ) -> Result<(), AgentError> {
        match tool_name {
            "bash_exec" => {
                // Stream shell command output line by line
                if let Some(stdout) = result.get_field("stdout") {
                    for line in stdout.lines() {
                        self.output.render_shell_output(line)?;
                    }
                }
            }
            "edit_file" | "write_file" => {
                // Show diff of changes
                if let Some(diff) = result.get_field("diff") {
                    self.output.render_diff(diff)?;
                }
            }
            "git_diff" | "git_status" => {
                // Stream git output directly
                self.output.render_git_output(&result.to_string())?;
            }
            _ => {
                // Generic result streaming
                self.output.render_tool_result(tool_name, result)?;
            }
        }
        Ok(())
    }
}
```

### CostTrackingHook

```rust
/// Tracks costs across LLM calls and tool executions.
pub struct CostTrackingHook;

impl CostTrackingHook {
    pub fn on_llm_response(
        &self,
        response: &LlmResponse,
        tracker: &CostTracker,
    ) {
        tracker.record_llm_cost(
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.cost,
        );
    }

    pub fn on_tool_result(
        &self,
        tool_name: &str,
        result: &ToolOutput,
        tracker: &CostTracker,
    ) {
        // Some tools have associated costs (e.g., embedding API calls in search)
        if let Some(cost) = result.cost() {
            tracker.record_tool_cost(tool_name, cost);
        }
    }
}
```

---

## Sub-Agent Spawning

xaft uses `SubagentTool<T>` to spawn specialized sub-agents for parallelizable work:

```rust
/// Example: spawning a sub-agent for test execution.
fn create_test_subagent(
    provider: impl LlmProvider,
    workspace: Arc<dyn WorkspaceStore>,
    bus: Arc<SignalBus>,
) -> SubagentTool<TestResult> {
    SubagentTool::new(
        "test_runner",
        XaftAgent::new(
            "test-runner",
            TEST_RUNNER_SYSTEM_PROMPT,
            vec![
                ErasedTool::from(BashExecTool::new(ShellConfig::sandboxed())),
                ErasedTool::from(ReadFileTool::new(workspace.clone())),
            ],
            provider,
        ),
        bus,
    )
}

/// The sub-agent returns a typed result.
#[derive(Debug, Serialize, Deserialize)]
pub struct TestResult {
    pub passed: Vec<String>,
    pub failed: Vec<TestFailure>,
    pub summary: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestFailure {
    pub test_name: String,
    pub error_message: String,
    pub relevant_files: Vec<PathBuf>,
}
```

### Sub-Agent Lifecycle

```
Parent Agent Turn
    │
    ├── LLM response includes tool call: "test_runner"
    │
    ▼
SubagentTool::execute(args)
    │
    ├── Construct sub-agent with limited tools
    ├── Create isolated ToolContext
    │   (shared workspace, isolated conversation)
    ├── AgentExecutor::run_stream(sub_agent, args, ...)
    │   │
    │   ├── sub_agent.on_start()
    │   ├── [ReAct loop turns]
    │   └── sub_agent.on_finish()
    │
    ├── Collect final result
    ├── Deserialize into typed TestResult
    ├── Emit Signal::SubagentComplete { agent_id, result }
    │
    ▼
Return typed TestResult to parent agent
```

---

## Agent Outcome

The final result of an agent run:

```rust
/// The outcome of an agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutcome {
    /// Whether the task was completed successfully.
    pub success: bool,

    /// Human-readable summary of what was accomplished.
    pub summary: String,

    /// Files that were modified.
    pub files_modified: Vec<PathBuf>,

    /// Files that were created.
    pub files_created: Vec<PathBuf>,

    /// Git commits made.
    pub commits: Vec<String>,

    /// Tools that were used (with counts).
    pub tools_used: HashMap<String, u32>,

    /// Total cost in USD.
    pub total_cost_usd: f64,

    /// Total turns executed.
    pub total_turns: u32,

    /// Any warnings or notes.
    pub warnings: Vec<String>,

    /// Error details if the task failed.
    pub error: Option<String>,
}
```

---

## Error Recovery in Lifecycle

Each lifecycle method can fail. xaft's error recovery strategy varies by phase:

| Lifecycle Phase | Error Strategy | Recovery Action |
|---|---|---|
| `on_start` | Fatal | Abort runtime, report to user |
| `before_llm_call` | Retryable | Retry with modified request (context reduction) |
| LLM Call | Retryable | FallbackProvider switches provider, retry up to 3 times |
| `after_llm_call` | Non-fatal | Log error, continue turn |
| `before_tool` | Vetoable | Return ToolVerdict::Deny, agent receives rejection message |
| `tool.execute` | Retryable | Retry up to 2 times with exponential backoff |
| `after_tool` | Non-fatal | Log error, continue with unmodified result |
| `on_tool_result` | Non-fatal | Log error, continue |
| `on_turn_complete` | Non-fatal | Log error, continue to next turn |
| `on_finish` | Best-effort | Log error, still persist session |

```rust
/// Error recovery wrapper for the ReAct loop.
async fn execute_turn_with_recovery(
    agent: &mut dyn Agent,
    ctx: &mut AgentContext,
) -> Result<TurnResult, AgentError> {
    // before_llm_call — retry with context reduction
    let mut request = build_llm_request(ctx)?;
    for attempt in 0..3 {
        match agent.before_llm_call(&mut request, ctx).await {
            Ok(()) => break,
            Err(e) if attempt < 2 => {
                // Reduce context and retry
                request.messages.truncate(request.messages.len().saturating_sub(5));
                ctx.signal_bus.emit(Signal::ContextReduced { attempt })?;
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    // LLM call — with fallback provider
    let response = match ctx.provider.complete(request.clone()).await {
        Ok(r) => r,
        Err(e) => {
            ctx.signal_bus.emit(Signal::ProviderError {
                provider: "primary".to_string(),
                error: e.to_string(),
            })?;
            // FallbackProvider handles the retry internally
            return Err(AgentError::LlmFailed(e));
        }
    };

    // after_llm_call — non-fatal
    if let Err(e) = agent.after_llm_call(&response, ctx).await {
        ctx.signal_bus.emit(Signal::LifecycleError {
            phase: "after_llm_call".to_string(),
            error: e.to_string(),
        })?;
    }

    Ok(TurnResult { response })
}
```

---

## Complete Turn Sequence Diagram

```
┌────────┐  ┌─────────┐  ┌──────────┐  ┌──────┐  ┌──────────┐  ┌─────┐
│  User  │  │ Executor │  │   Agent  │  │  LLM │  │   Tool   │  │ Bus │
└───┬────┘  └────┬─────┘  └────┬─────┘  └──┬───┘  └────┬─────┘  └──┬──┘
    │            │             │            │           │            │
    │  prompt    │             │            │           │            │
    │───────────▶│             │            │           │            │
    │            │  on_start() │            │           │            │
    │            │────────────▶│            │           │            │
    │            │             │─────────────────────────────────────▶│
    │            │             │            │           │   AgentStarted
    │            │             │◀────────────────────────────────────│
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │            │ ═══════ TURN LOOP ═══════            │            │
    │            │             │            │           │            │
    │            │ before_llm  │            │           │            │
    │            │────────────▶│            │           │            │
    │            │             │─────────────────────────────────────▶│
    │            │             │            │           │  BeforeLlmCall
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │            │ llm.stream()│            │           │            │
    │            │────────────────────────▶│            │            │
    │            │◀────────────────────────│ tokens     │            │
    │            │─────────────────────────────────────────────────▶│
    │            │             │            │           │ StreamToken│
    │            │             │            │           │            │
    │            │ after_llm   │            │           │            │
    │            │────────────▶│            │           │            │
    │            │             │─────────────────────────────────────▶│
    │            │             │            │           │ AfterLlmCall
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │            │ tool.dispatch(args)       │           │            │
    │            │────────────▶│            │           │            │
    │            │             │ before_tool│           │            │
    │            │             │────────────────────────────────────▶│
    │            │             │            │           │ BeforeTool │
    │            │             │▶───────────┼──────────▶│            │
    │            │             │            │  execute  │            │
    │            │             │◀───────────┼───────────│            │
    │            │             │ after_tool │           │            │
    │            │             │────────────────────────────────────▶│
    │            │◀────────────│            │           │ ToolResult │
    │            │             │            │           │            │
    │            │ on_tool_result           │           │            │
    │            │────────────▶│            │           │            │
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │            │ on_turn_complete         │           │            │
    │            │────────────▶│            │           │            │
    │            │             │─────────────────────────────────────▶│
    │            │             │            │           │TurnComplete│
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │            │ ═════════ END LOOP ═══════           │            │
    │            │             │            │           │            │
    │            │ on_finish() │            │           │            │
    │            │────────────▶│            │           │            │
    │            │             │─────────────────────────────────────▶│
    │            │             │            │           │AgentFinished
    │            │◀────────────│            │           │            │
    │            │             │            │           │            │
    │  result    │             │            │           │            │
    │◀───────────│             │            │           │            │
    ▼            ▼             ▼            ▼           ▼            ▼
```
