# Agent Handoff Patterns

> How xauft transfers control between agents: `HandoffOrchestrator`,
> `HandoffAgentStore`, `RequestFixTool`, context preservation, and
> coordinator→worker fan-out patterns.

---

## 1. Overview

Handoff is the mechanism by which xauft **transfers control** from one agent
to another within a session. Unlike simple tool calls (where the parent agent
retains control), handoffs represent a **full context switch**: the receiving
agent becomes the active agent and continues the task autonomously until it
hands off to another agent or completes.

```
┌──────────────────────────────────────────────────────────────┐
│                    HandoffOrchestrator                        │
│                                                              │
│  ┌─────────┐     ┌─────────┐     ┌─────────┐               │
│  │ Agent A │────▶│ Agent B │────▶│ Agent C │               │
│  │ (Coder) │     │  (QA)   │     │ (Fixer) │               │
│  └─────────┘     └─────────┘     └─────────┘               │
│       │               │               │                      │
│       │   HandoffAgentStore           │                      │
│       │   ┌───────────────────────┐   │                      │
│       └──▶│ active: AgentId       │◀──┘                      │
│           │ history: [A→B, B→C]   │                          │
│           │ contexts: {A: ...,    │                          │
│           │           B: ...,     │                          │
│           │           C: ...}     │                          │
│           └───────────────────────┘                          │
└──────────────────────────────────────────────────────────────┘
```

---

## 2. HandoffOrchestrator

The `HandoffOrchestrator` manages the lifecycle of agent handoffs within a
session. It decides *when* to hand off, *to whom*, and ensures context is
properly transferred.

### 2.1 Architecture

```rust
pub struct HandoffOrchestrator<P: LlmProvider> {
    /// Provider for creating agents.
    provider: Arc<P>,
    /// Agent configuration registry.
    agent_configs: HashMap<AgentRole, AgentConfig>,
    /// Tracks active agent and handoff history.
    agent_store: HandoffAgentStore,
    /// Message bus for inter-agent communication.
    bus: AgentMessageBus,
    /// Handoff trigger rules.
    rules: Vec<HandoffRule>,
    /// Maximum handoffs per task (prevents infinite loops).
    max_handoffs: usize,
    /// Configuration.
    config: HandoffConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffConfig {
    /// Maximum handoffs before escalation.
    pub max_handoffs: usize,
    /// Whether to preserve full message history across handoffs.
    pub preserve_full_history: bool,
    /// Whether to include tool results in handoff context.
    pub include_tool_results: bool,
    /// Timeout for handoff target to respond.
    pub handoff_timeout: Duration,
    /// Whether to auto-handoff on tool call triggers.
    pub auto_handoff: bool,
}
```

### 2.2 Handoff Triggers

Handoffs can be triggered by:

1. **Tool calls** — specific tools like `RequestFixTool` trigger handoffs.
2. **Agent decision** — an agent explicitly requests a handoff.
3. **Error escalation** — an agent fails and control transfers to a specialist.
4. **Planner directive** — the plan specifies which agent handles each step.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HandoffTrigger {
    /// A tool call triggered the handoff.
    ToolCall {
        tool_name: String,
        tool_input: serde_json::Value,
    },
    /// The agent explicitly requested a handoff.
    AgentRequested {
        target_role: AgentRole,
        reason: String,
    },
    /// An error occurred and control needs to transfer.
    ErrorEscalation {
        error: String,
        error_type: ErrorType,
    },
    /// The plan specified this handoff.
    PlanDirective {
        step_id: StepId,
        target_role: AgentRole,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorType {
    /// LLM provider error (rate limit, context overflow, etc.)
    ProviderError,
    /// Tool execution error (command failed, file not found, etc.)
    ToolError,
    /// Validation error (output didn't match schema, etc.)
    ValidationError,
    /// Timeout error.
    TimeoutError,
}
```

### 2.3 Handoff Rules

Rules define *when* a handoff should occur:

```rust
pub struct HandoffRule {
    /// Condition that triggers the rule.
    condition: Box<dyn HandoffCondition + Send + Sync>,
    /// Target agent role for the handoff.
    target_role: AgentRole,
    /// Priority of this rule (higher = evaluated first).
    priority: i32,
    /// Whether the handoff is mandatory or optional.
    mandatory: bool,
}

#[async_trait]
pub trait HandoffCondition: Send + Sync {
    async fn evaluate(&self, context: &HandoffContext) -> bool;
}

/// Built-in condition: trigger handoff when QA finds issues.
pub struct QaIssuesFoundCondition;

#[async_trait]
impl HandoffCondition for QaIssuesFoundCondition {
    async fn evaluate(&self, context: &HandoffContext) -> bool {
        // Check if the QA agent's output contains issues
        context.last_tool_output.as_ref()
            .and_then(|v| v.get("issues"))
            .and_then(|v| v.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false)
    }
}

/// Built-in condition: trigger handoff on compilation error.
pub struct CompilationErrorCondition;

#[async_trait]
impl HandoffCondition for CompilationErrorCondition {
    async fn evaluate(&self, context: &HandoffContext) -> bool {
        context.last_tool_output.as_ref()
            .and_then(|v| v.get("exit_code"))
            .and_then(|v| v.as_i64())
            .map(|code| code != 0)
            .unwrap_or(false)
    }
}
```

### 2.4 Handoff Execution

```rust
impl<P: LlmProvider + Clone + 'static> HandoffOrchestrator<P> {
    /// Execute a handoff from the current agent to a target agent.
    pub async fn handoff(
        &self,
        from: AgentId,
        trigger: HandoffTrigger,
        current_context: &AgentExecutionContext,
    ) -> Result<HandoffResult, HandoffError> {
        // 1. Determine target agent
        let target_role = match &trigger {
            HandoffTrigger::ToolCall { tool_name, .. } => {
                self.resolve_target_for_tool(tool_name)?
            }
            HandoffTrigger::AgentRequested { target_role, .. } => target_role.clone(),
            HandoffTrigger::ErrorEscalation { .. } => AgentRole::Fixer,
            HandoffTrigger::PlanDirective { target_role, .. } => target_role.clone(),
        };

        // 2. Check handoff limits
        let history = self.agent_store.history().await;
        if history.len() >= self.max_handoffs {
            return Err(HandoffError::MaxHandoffsExceeded {
                count: history.len(),
                max: self.max_handoffs,
            });
        }

        // 3. Create target agent
        let config = self.agent_configs.get(&target_role)
            .ok_or(HandoffError::NoAgentForRole(target_role.clone()))?;
        let target_agent = Agent::new(config, self.provider.clone());
        let target_id = target_agent.id();

        // 4. Prepare handoff context
        let handoff_context = self.prepare_handoff_context(
            &from,
            &target_id,
            &trigger,
            current_context,
        ).await?;

        // 5. Transfer context to target agent
        target_agent.receive_context(&handoff_context).await?;

        // 6. Record handoff
        self.agent_store.handoff(
            from.clone(),
            target_id.clone(),
            HandoffReason::from_trigger(&trigger),
            handoff_context.messages.clone(),
        ).await;

        // 7. Publish handoff event
        self.bus.publish(AgentMessage::Handoff {
            from: from.clone(),
            to: target_id.clone(),
            reason: HandoffReason::from_trigger(&trigger),
            context: handoff_context.summary(),
        });

        // 8. Execute target agent
        let result = target_agent.execute().await?;

        Ok(HandoffResult {
            from,
            to: target_id,
            trigger,
            result,
        })
    }

    /// Prepare the context to transfer during handoff.
    async fn prepare_handoff_context(
        &self,
        from: &AgentId,
        to: &AgentId,
        trigger: &HandoffTrigger,
        current: &AgentExecutionContext,
    ) -> Result<HandoffTransferContext, HandoffError> {
        let mut messages = Vec::new();

        if self.config.preserve_full_history {
            // Transfer full conversation history
            messages.extend(current.conversation.messages().cloned());
        } else {
            // Transfer only recent messages (sliding window)
            let window = current.conversation.last_n_messages(10);
            messages.extend(window);
        }

        // Always include tool results if configured
        let tool_results = if self.config.include_tool_results {
            current.tool_results.clone()
        } else {
            HashMap::new()
        };

        // Add handoff instruction message
        messages.push(Message::system(
            format!(
                "[HANDOFF] Control transferred from {} to {}.\n\
                 Trigger: {:?}\n\
                 You are now the active agent. Continue the task from where \
                 the previous agent left off.",
                from, to, trigger
            )
        ));

        Ok(HandoffTransferContext {
            messages,
            tool_results,
            artifacts: current.artifacts.clone(),
            scratchpad: current.scratchpad.clone(),
        })
    }
}
```

---

## 3. RequestFixTool: The Handoff Trigger

`RequestFixTool` is the primary mechanism by which the QA agent triggers a
handoff to the Fixer agent. It demonstrates the tool-call-triggered handoff
pattern.

### 3.1 Design

```
  QA Agent                     HandoffOrchestrator              Fixer Agent
     │                              │                              │
     │  tool_call("request_fix",    │                              │
     │    {issues: [...],           │                              │
     │     file: "src/auth.rs"})    │                              │
     │─────────────────────────────▶│                              │
     │                              │  detect RequestFixTool call   │
     │                              │  ─────────────────────        │
     │                              │  trigger handoff              │
     │                              │  target: Fixer                │
     │                              │                               │
     │                              │  prepare context               │
     │                              │  (QA's messages + issues)     │
     │                              │──────────────────────────────▶│
     │                              │                               │
     │                              │                fixer processes │
     │                              │                issues and     │
     │                              │                produces fix   │
     │                              │                               │
     │                              │  HandoffResult(fixed_code)    │
     │                              │◀──────────────────────────────│
     │                              │                               │
     │  tool_result(fixed_code)     │                               │
     │◀─────────────────────────────│                               │
     │                              │                               │
     │  (QA may re-review)          │                               │
```

### 3.2 Implementation

```rust
/// Tool that triggers a handoff from QA to Fixer.
pub struct RequestFixTool {
    orchestrator: Arc<HandoffOrchestrator<dyn LlmProvider>>,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct RequestFixInput {
    /// List of issues found during review.
    issues: Vec<IssueDescription>,
    /// The file(s) that need fixing.
    files: Vec<String>,
    /// Priority of the fix.
    priority: FixPriority,
    /// Optional: specific line ranges with issues.
    line_ranges: Option<HashMap<String, Vec<(u32, u32)>>>,
    /// Optional: suggested fix approach.
    suggested_approach: Option<String>,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct IssueDescription {
    severity: IssueSeverity,
    message: String,
    file: String,
    line: Option<u32>,
    code_snippet: Option<String>,
}

#[derive(Debug, JsonSchema, Deserialize)]
enum FixPriority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, JsonSchema, Serialize)]
struct RequestFixOutput {
    fix_id: String,
    status: FixStatus,
    fixed_files: Vec<String>,
    remaining_issues: Vec<String>,
    iterations_used: u32,
}

#[derive(Debug, JsonSchema, Serialize)]
enum FixStatus {
    Fixed,
    PartiallyFixed,
    UnableToFix,
}

#[async_trait]
impl Tool for RequestFixTool {
    fn name(&self) -> &str { "request_fix" }
    fn description(&self) -> &str {
        "Request a fix for issues found during code review. Triggers a handoff \
         to the Fixer agent who will address the listed issues."
    }
    fn input_schema(&self) -> serde_json::Value {
        schemars::schema_for!(RequestFixInput).into()
    }

    async fn call(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let input: RequestFixInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        // Trigger handoff through the orchestrator
        let current_context = self.orchestrator.current_context().await
            .ok_or(ToolError::ExecutionFailed("No active context".into()))?;

        let result = self.orchestrator.handoff(
            current_context.active_agent_id.clone(),
            HandoffTrigger::ToolCall {
                tool_name: "request_fix".into(),
                tool_input: serde_json::to_value(&input).unwrap(),
            },
            &current_context,
        ).await.map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Convert handoff result to tool output
        let output = RequestFixOutput {
            fix_id: Uuid::new_v4().to_string(),
            status: FixStatus::Fixed, // determined from result
            fixed_files: input.files.clone(),
            remaining_issues: vec![],
            iterations_used: 1,
        };

        Ok(ToolOutput::Json(serde_json::to_value(output)?))
    }
}
```

---

## 4. HandoffAgentStore

The `HandoffAgentStore` tracks the currently active agent and preserves
context across handoffs.

### 4.1 Data Structures

```rust
pub struct HandoffAgentStore {
    /// Currently active agent ID.
    active: RwLock<Option<AgentId>>,
    /// Complete handoff history for the current session.
    history: RwLock<Vec<HandoffRecord>>,
    /// Per-agent accumulated context.
    contexts: DashMap<AgentId, AgentContext>,
    /// Session ID for this store.
    session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffRecord {
    /// Source agent.
    pub from: AgentId,
    /// Target agent.
    pub to: AgentId,
    /// Reason for the handoff.
    pub reason: HandoffReason,
    /// Timestamp of the handoff.
    pub timestamp: DateTime<Utc>,
    /// Messages transferred during handoff.
    pub context_transferred: Vec<Message>,
    /// Summary of what the source agent accomplished.
    pub from_summary: String,
    /// Instructions for the target agent.
    pub to_instructions: String,
    /// Token usage at handoff time.
    pub token_usage: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HandoffReason {
    /// QA found issues, handing off to Fixer.
    IssuesFound { issue_count: usize },
    /// Agent explicitly requested handoff.
    AgentRequested { reason: String },
    /// Error escalation.
    ErrorEscalation { error_type: ErrorType, error_message: String },
    /// Plan-specified transition.
    PlanDirective { step_id: StepId },
    /// Agent completed its part, handing off to next in chain.
    StepCompleted,
    /// Agent's context window is full, handing off to fresh agent.
    ContextOverflow { tokens_used: usize, tokens_limit: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    /// All messages this agent has seen.
    pub messages: Vec<Message>,
    /// Tool call results this agent has accumulated.
    pub tool_results: HashMap<ToolCallId, ToolOutput>,
    /// Artifacts (file contents, search results, etc.) this agent has produced.
    pub artifacts: Vec<Artifact>,
    /// Scratchpad for cross-turn notes.
    pub scratchpad: String,
    /// Number of LLM calls this agent has made.
    pub llm_calls: usize,
    /// Total tokens this agent has consumed.
    pub total_tokens: TokenUsage,
    /// When this agent's context was created.
    pub created_at: DateTime<Utc>,
    /// Last activity timestamp.
    pub last_active: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub artifact_type: ArtifactType,
    pub name: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactType {
    FileContent,
    SearchResult,
    TestOutput,
    Diff,
    Summary,
    Note,
}
```

### 4.2 Context Preservation Across Handoffs

The key challenge is ensuring the receiving agent has enough context to
continue effectively without being overwhelmed. xauft uses a **layered
context** approach:

```
┌─────────────────────────────────────────────────┐
│                  Layer 3: Summary               │
│  "Agent A implemented auth middleware,           │
│   Agent B found 3 security issues,              │
│   Agent C fixed 2 of 3 issues"                  │
├─────────────────────────────────────────────────┤
│                  Layer 2: Key Messages           │
│  Last N messages from prior agents              │
│  + handoff instruction messages                 │
├─────────────────────────────────────────────────┤
│                  Layer 1: Full History           │
│  Complete message history (only if configured)  │
│  + all tool results                             │
└─────────────────────────────────────────────────┘
```

```rust
impl HandoffAgentStore {
    /// Build the context to transfer to the target agent.
    pub async fn build_transfer_context(
        &self,
        target: &AgentId,
        mode: ContextTransferMode,
    ) -> AgentContext {
        match mode {
            ContextTransferMode::Full => {
                // Merge all prior agent contexts
                let mut merged = AgentContext::default();
                for entry in self.contexts.iter() {
                    merged.messages.extend(entry.value().messages.clone());
                    merged.tool_results.extend(entry.value().tool_results.clone());
                    merged.artifacts.extend(entry.value().artifacts.clone());
                }
                // Add handoff history as system messages
                let history = self.history.read().await;
                for record in history.iter() {
                    merged.messages.push(Message::system(
                        format!("[HANDOFF {}→{}] {}",
                            record.from, record.to, record.to_instructions)
                    ));
                }
                merged
            }
            ContextTransferMode::Summary => {
                // Only include summary of prior work
                let mut context = AgentContext::default();
                let summary = self.generate_summary().await;
                context.messages.push(Message::system(summary));
                context
            }
            ContextTransferMode::Window { size } => {
                // Include only the last N messages from the most recent agent
                let mut context = AgentContext::default();
                if let Some(active) = self.active.read().await.as_ref() {
                    if let Some(source_ctx) = self.contexts.get(active) {
                        let start = source_ctx.messages.len().saturating_sub(size);
                        context.messages = source_ctx.messages[start..].to_vec();
                    }
                }
                context
            }
        }
    }

    /// Generate a summary of all prior agent work.
    async fn generate_summary(&self) -> String {
        let history = self.history.read().await;
        let mut parts = Vec::new();

        for record in history.iter() {
            parts.push(format!(
                "Agent {} → {}: {} (Summary: {})",
                record.from, record.to, record.reason, record.from_summary
            ));
        }

        format!(
            "Session handoff history:\n{}\n\nYou are continuing this work.",
            parts.join("\n")
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextTransferMode {
    /// Transfer full conversation history.
    Full,
    /// Transfer only a summary of prior work.
    Summary,
    /// Transfer a sliding window of N messages.
    Window { size: usize },
}
```

---

## 5. Coordinator→Worker Fan-Out

In `TeamMode::Coordinator`, the coordinator agent fans out sub-tasks to
multiple workers. This is a special handoff pattern where the coordinator
delegates to multiple agents simultaneously.

### 5.1 Sequence Diagram

```
  Coordinator           Worker A (Coder)       Worker B (QA)        Worker C (Docs)
      │                      │                      │                      │
      │  1. Plan steps       │                      │                      │
      │──────┐               │                      │                      │
      │      │               │                      │                      │
      │◀─────┘               │                      │                      │
      │                      │                      │                      │
      │  2. Fan-out step A   │                      │                      │
      │─────────────────────▶│                      │                      │
      │                      │                      │                      │
      │  3. Fan-out step B   │                      │                      │
      │─────────────────────────────────────────────▶│                      │
      │                      │                      │                      │
      │  4. Fan-out step C   │                      │                      │
      │────────────────────────────────────────────────────────────────────▶│
      │                      │                      │                      │
      │                      │  5. Execute          │  5. Execute          │
      │                      │──────┐               │──────┐               │
      │                      │      │               │      │               │
      │                      │◀─────┘               │◀─────┘               │
      │                      │                      │                      │
      │  6. Result A         │                      │                      │
      │◀─────────────────────│                      │                      │
      │                      │                      │                      │
      │  7. Result B         │                      │                      │
      │◀─────────────────────────────────────────────│                      │
      │                      │                      │                      │
      │  8. Result C         │                      │                      │
      │◀────────────────────────────────────────────────────────────────────│
      │                      │                      │                      │
      │  9. Synthesize       │                      │                      │
      │──────┐               │                      │                      │
      │      │               │                      │                      │
      │◀─────┘               │                      │                      │
      │                      │                      │                      │
      │  10. Final Output    │                      │                      │
      │                      │                      │                      │
```

### 5.2 Fan-Out with Context Isolation

Each worker receives an **isolated context** — only the information relevant
to its assigned step. This reduces token usage and prevents cross-contamination.

```rust
impl<P: LlmProvider + Clone + 'static> CoordinatorExecutor<P> {
    /// Fan out sub-tasks to workers with isolated contexts.
    pub async fn fan_out(
        &self,
        steps: Vec<PlannedStep>,
        base_context: &AgentContext,
    ) -> Vec<StepResult> {
        let mut join_set = JoinSet::new();

        for step in steps {
            let pool = self.pool.clone();
            let bus = self.bus.clone();

            // Build isolated context for this worker
            let worker_context = AgentContext {
                messages: vec![
                    Message::system(format!(
                        "You are working on step: {}\n\
                         Your role: {:?}\n\
                         Available tools: {}",
                        step.description,
                        step.assigned_role,
                        step.available_tools.join(", ")
                    )),
                    // Include only the task description, not full history
                    base_context.messages.first().cloned()
                        .unwrap_or(Message::user(&step.description)),
                ],
                tool_results: HashMap::new(),
                artifacts: vec![],
                scratchpad: String::new(),
                llm_calls: 0,
                total_tokens: TokenUsage::default(),
                created_at: Utc::now(),
                last_active: Utc::now(),
            };

            join_set.spawn(async move {
                let agent = pool.acquire_agent(step.assigned_role).await
                    .map_err(|e| StepResult::Error(e.to_string()))?;
                agent.set_context(worker_context).await
                    .map_err(|e| StepResult::Error(e.to_string()))?;
                let result = agent.execute_step(&step).await
                    .map_err(|e| StepResult::Error(e.to_string()))?;

                bus.publish(AgentMessage::Completed {
                    agent_id: agent.id(),
                    step_id: step.id,
                    result: result.clone(),
                });

                Ok(result)
            });
        }

        let mut results = Vec::new();
        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(e)) => results.push(StepResult::Error(e.to_string())),
                Err(e) => results.push(StepResult::Error(format!("Join error: {}", e))),
            }
        }
        results
    }
}
```

### 5.3 Fan-Out with Sequential Dependencies

When steps have dependencies, fan-out is staggered:

```rust
impl<P: LlmProvider + Clone + 'static> CoordinatorExecutor<P> {
    /// Execute a plan respecting step dependencies.
    pub async fn execute_plan(
        &self,
        plan: &Plan,
    ) -> Result<Vec<StepResult>, ExecutorError> {
        let mut completed: HashMap<StepId, StepResult> = HashMap::new();
        let mut remaining: HashSet<StepId> = plan.steps.iter().map(|s| s.id).collect();
        let mut results = Vec::new();

        while !remaining.is_empty() {
            // Find steps whose dependencies are all satisfied
            let ready: Vec<_> = plan.steps.iter()
                .filter(|s| remaining.contains(&s.id))
                .filter(|s| s.depends_on.iter().all(|dep| completed.contains_key(dep)))
                .collect();

            if ready.is_empty() && !remaining.is_empty() {
                return Err(ExecutorError::Deadlock {
                    remaining: remaining.into_iter().collect(),
                });
            }

            // Execute ready steps in parallel
            let ready_steps: Vec<PlannedStep> = ready.iter().map(|s| (*s).clone()).collect();
            let step_results = self.fan_out(ready_steps, &self.base_context()).await;

            // Record results
            for (step, result) in ready.iter().zip(step_results) {
                completed.insert(step.id, result.clone());
                remaining.remove(&step.id);
                results.push(result);
            }
        }

        Ok(results)
    }
}
```

---

## 6. Context Preservation: Messages in Done Event

When an agent completes (produces a `Done` event), its full message history
is included in the event. This allows the next agent or the coordinator to
access everything the prior agent saw and did.

### 6.1 Done Event Structure

```rust
/// Event emitted when an agent completes its task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDoneEvent {
    /// The agent that completed.
    pub agent_id: AgentId,
    /// The task or step that was completed.
    pub task_id: TaskId,
    /// The step that was completed.
    pub step_id: Option<StepId>,
    /// The agent's output.
    pub output: AgentOutput,
    /// Full conversation history from this agent's execution.
    pub messages: Vec<Message>,
    /// Tool calls made and their results.
    pub tool_trace: Vec<ToolTraceEntry>,
    /// Files modified by this agent.
    pub modified_files: Vec<FileChange>,
    /// Token usage summary.
    pub token_usage: TokenUsage,
    /// Duration of execution.
    pub duration: Duration,
    /// Agent's self-assessment of completion quality.
    pub self_assessment: Option<CompletionAssessment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTraceEntry {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub duration: Duration,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionAssessment {
    pub confidence: f64,
    pub completeness: f64,
    pub notes: String,
    pub follow_up_needed: bool,
}
```

### 6.2 Context Reconstruction

When the next agent starts, it reconstructs its context from the `Done` event:

```rust
impl AgentContext {
    /// Reconstruct context from a prior agent's Done event.
    pub fn from_done_event(event: &AgentDoneEvent, mode: ContextTransferMode) -> Self {
        match mode {
            ContextTransferMode::Full => AgentContext {
                messages: event.messages.clone(),
                tool_results: event.tool_trace.iter()
                    .map(|t| (ToolCallId::new(), ToolOutput::Json(t.output.clone())))
                    .collect(),
                artifacts: event.modified_files.iter()
                    .map(|f| Artifact {
                        artifact_type: ArtifactType::Diff,
                        name: f.path.to_string_lossy().to_string(),
                        content: f.diff.clone().unwrap_or_default(),
                        created_at: Utc::now(),
                    })
                    .collect(),
                scratchpad: String::new(),
                llm_calls: 0,
                total_tokens: event.token_usage,
                created_at: Utc::now(),
                last_active: Utc::now(),
            },
            ContextTransferMode::Summary => AgentContext {
                messages: vec![
                    Message::system(format!(
                        "Prior agent {} completed task {}.\n\
                         Summary: {} files modified, {} tool calls made.\n\
                         Self-assessment: confidence={}, completeness={}\n\
                         Notes: {}",
                        event.agent_id,
                        event.task_id,
                        event.modified_files.len(),
                        event.tool_trace.len(),
                        event.self_assessment.as_ref()
                            .map(|a| a.confidence).unwrap_or(0.0),
                        event.self_assessment.as_ref()
                            .map(|a| a.completeness).unwrap_or(0.0),
                        event.self_assessment.as_ref()
                            .map(|a| a.notes.as_str()).unwrap_or("N/A"),
                    )),
                ],
                tool_results: HashMap::new(),
                artifacts: vec![],
                scratchpad: String::new(),
                llm_calls: 0,
                total_tokens: TokenUsage::default(),
                created_at: Utc::now(),
                last_active: Utc::now(),
            },
            ContextTransferMode::Window { size } => {
                let start = event.messages.len().saturating_sub(size);
                AgentContext {
                    messages: event.messages[start..].to_vec(),
                    ..Self::default()
                }
            }
        }
    }
}
```

---

## 7. Handoff Pattern Catalog

### 7.1 Pattern Summary

| Pattern                | Trigger              | Direction         | Context Transfer | Use Case                    |
|------------------------|----------------------|-------------------|------------------|-----------------------------|
| QA→Fixer Loop          | RequestFixTool call  | QA → Fixer → QA   | Full + issues    | Code review feedback loop   |
| Coordinator Fan-Out    | Plan step dispatch   | Coordinator → N   | Isolated         | Parallel sub-task execution |
| Sequential Chain       | Step completion      | A → B → C → ...   | Accumulated      | Collaborate mode            |
| Error Escalation       | Agent failure        | Worker → Specialist| Error context    | Recoverable error handling  |
| Context Overflow       | Token limit reached  | Agent → Fresh Agent| Summary          | Long-running tasks          |
| Planner Handoff        | Plan directive       | As specified      | Per plan config  | Structured multi-step tasks |

### 7.2 QA→Fixer Loop (Detailed Sequence)

```
  Coder               QA                  Fixer               QA (re-review)
    │                  │                    │                      │
    │  1. Submit code  │                    │                      │
    │─────────────────▶│                    │                      │
    │                  │                    │                      │
    │                  │  2. Review code    │                      │
    │                  │──────┐             │                      │
    │                  │      │             │                      │
    │                  │◀─────┘             │                      │
    │                  │                    │                      │
    │                  │  3. Issues found!  │                      │
    │                  │  request_fix()     │                      │
    │                  │───────────────────▶│                      │
    │                  │                    │                      │
    │                  │                    │  4. Fix issues       │
    │                  │                    │──────┐               │
    │                  │                    │      │               │
    │                  │                    │◀─────┘               │
    │                  │                    │                      │
    │                  │  5. Fixed code     │                      │
    │                  │◀───────────────────│                      │
    │                  │                    │                      │
    │                  │  6. Re-review      │                      │
    │                  │──────┐             │                      │
    │                  │      │             │                      │
    │                  │◀─────┘             │                      │
    │                  │                    │                      │
    │                  │  7. Approved ✓     │                      │
    │                  │                    │                      │
```

### 7.3 Error Escalation Pattern

```
  Worker Agent               Error Handler Agent
      │                              │
      │  1. Execute step             │
      │──────┐                       │
      │      │  ERROR!               │
      │◀─────┘                       │
      │                              │
      │  2. Handoff (error context)  │
      │─────────────────────────────▶│
      │                              │
      │                              │  3. Analyze error
      │                              │──────┐
      │                              │      │
      │                              │◀─────┘
      │                              │
      │                              │  4. Attempt recovery
      │                              │──────┐
      │                              │      │
      │                              │◀─────┘
      │                              │
      │  5. Recovery result          │
      │◀─────────────────────────────│
      │                              │
```

---

## 8. Configuration

```toml
[xaft.handoff]
max_handoffs = 10               # maximum handoffs per task
preserve_full_history = false    # use summary mode by default
include_tool_results = true      # include tool results in context
auto_handoff = true              # auto-handoff on tool triggers
handoff_timeout_secs = 120       # timeout for handoff completion
context_transfer_mode = "window" # "full" | "summary" | "window"
context_window_size = 10         # messages for window mode

[xaft.handoff.rules]
# Auto-handoff when QA finds issues
qa_issues_to_fixer = true
# Auto-handoff on compilation error
compile_error_to_fixer = true
# Escalate on context overflow
context_overflow_fresh_agent = true
```

---

## 9. Observability

All handoff events are emitted for telemetry and debugging:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum HandoffEvent {
    #[serde(rename = "handoff.initiated")]
    Initiated {
        from: AgentId,
        to: AgentId,
        trigger: String,
        context_size_messages: usize,
        context_size_tokens: usize,
    },

    #[serde(rename = "handoff.completed")]
    Completed {
        from: AgentId,
        to: AgentId,
        duration: Duration,
        result_summary: String,
    },

    #[serde(rename = "handoff.failed")]
    Failed {
        from: AgentId,
        to: AgentId,
        error: String,
    },

    #[serde(rename = "handoff.context_transferred")]
    ContextTransferred {
        from: AgentId,
        to: AgentId,
        messages_count: usize,
        tool_results_count: usize,
        artifacts_count: usize,
        estimated_tokens: usize,
    },
}
```

These events are consumed by xauft's SSE bridge and displayed in real-time
on the dashboard, providing full visibility into the agent coordination flow.
