# xaft-runtime — Dynamic Agent Handoff PRD

## Background

xaft-runtime today runs a **fixed four-agent pipeline**:

```
Planner → Coder → QA ↔ Fixer
```

This is implemented in `orchestrator.rs::run_workflow()`.  The QA↔Fixer cycle
already uses `agtrs_runtime::HandoffOrchestrator`, so the primitives exist.
What is missing is a **general-purpose handoff mechanism** that lets any
registered agent invoke any other agent at runtime — modelled on
`lauren-ai-chatbot-rs` where `UnauthenticatedCrmAgent → AuthenticatedCrmAgent
→ BankTransferAgent ↔ DisputesAgent` all hand off freely.

## Goal

Enable arbitrary **agent-to-agent handoffs** inside xaft-runtime:

- Any agent can transfer control to any other registered agent by calling a
  generic `HandoffTool`.
- New agents can be registered without touching `orchestrator.rs`.
- The existing planner→coder→QA↔Fixer workflow continues working unchanged.
- A `WorkflowConfig` replaces the hardcoded agent wiring and becomes the
  single place to add new agents.

---

## Reference: agtrs + lauren-ai-chatbot-rs handoff model

### How it works today in agtrs

1. **HandoffAgentStore** (in-process shared state, `agtrs-runtime/src/team.rs`)
   - `set_active_agent(conv_id, agent_name)` — written by the handoff tool
   - `get_active_agent(conv_id)` — polled by the orchestrator after each turn
   - `set_pending_summary / get_and_clear_summary` — reason for the handoff

2. **Handoff tool** — a dedicated tool given to each agent that can initiate
   a handoff.  On `call()` it writes to `HandoffAgentStore`.  Input schema:
   `{ "target": "agent_name", "reason": "why we're transferring" }`.

3. **HandoffOrchestrator** loop (simplified):
   ```
   for turn in 0..max_handoffs:
     agent_conv_key = "{conv_id}::{current_agent}"
     history = conv_store.load(agent_conv_key)
     run agent with history
     conv_store.save(agent_conv_key, new_history)
     if store.get_active_agent() changed:
       summary = store.get_and_clear_summary()
       next_prompt = prompt_fn(HandoffContext { from, to, summary, original })
       current_agent = new_agent
     else:
       break
   ```

4. **Conversation isolation**: each agent in the same run gets its own history
   under `"{conv_id}::{agent_name}"`.  The original user message is threaded
   through every agent prompt so no context is lost.

5. **Signal emission**: `HandoffEvent::AgentHandoff { from, to, summary }` is
   broadcast so the TUI and callers can observe the transition.

---

## Target Design

### New file: `xaft-runtime/src/agent_registry.rs`

```rust
/// Definition for a registrable xaft agent.
pub struct AgentDefinition {
    pub name: String,
    pub system_prompt_fn: Box<dyn Fn(&str /* task */, &str /* working_dir */) -> String + Send + Sync>,
    pub tool_set: AgentToolSet,
    pub max_turns: usize,
    pub can_handoff_to: Vec<String>,  // allowed handoff targets; empty = any
}

/// Which tool category an agent receives.
pub enum AgentToolSet { ReadOnly, ReadWrite, Custom(Vec<String>) }

/// Registry of all named agents available to a run.
pub struct AgentRegistry {
    definitions: IndexMap<String, AgentDefinition>,
}

impl AgentRegistry {
    pub fn new() -> Self { ... }
    pub fn register(mut self, def: AgentDefinition) -> Self { ... }
    pub fn default_xaft() -> Self { /* pre-registers planner, coder, qa, fixer */ }

    /// Instantiate a `NamedAgent` for use in an orchestrator.
    pub fn build_agent(
        &self,
        name: &str,
        task: &str,
        working_dir: &str,
        read_tools: &[Arc<ErasedTool>],
        write_tools: &[Arc<ErasedTool>],
        handoff_store: Arc<HandoffAgentStore>,
        signals: Arc<SignalBus>,
    ) -> Result<Arc<dyn Agent>, RuntimeError> { ... }
}
```

### New tool: `HandoffTool` (in `orchestrator.rs` or new `tools/handoff.rs`)

```rust
/// Generic handoff tool given to every agent that participates in a
/// multi-agent workflow.  On call it writes to HandoffAgentStore so the
/// orchestrator can switch agents after the current turn finishes.
pub struct HandoffTool {
    store: Arc<HandoffAgentStore>,
    allowed_targets: Vec<String>,  // enforced at call time
}

impl Tool for HandoffTool {
    fn name(&self) -> &str { "handoff_to_agent" }
    fn description(&self) -> &str {
        "Transfer the conversation to another agent. \
         Use when the current task is better handled by a specialist."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target_agent": { "type": "string",
                    "description": "Name of the agent to hand off to." },
                "reason": { "type": "string",
                    "description": "Why this handoff is needed. Will be shown to the next agent." }
            },
            "required": ["target_agent", "reason"]
        })
    }
    async fn call(&self, input: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let target = input["target_agent"].as_str().unwrap_or("");
        let reason  = input["reason"].as_str().unwrap_or("Agent handoff");
        // Validate target is allowed
        if !self.allowed_targets.is_empty() && !self.allowed_targets.contains(&target.to_string()) {
            return Ok(ToolResult::err(format!("Handoff to '{target}' not permitted")));
        }
        let conv_id = ctx.state["conversation_id"].as_str().unwrap_or("").to_string();
        self.store.set_active_agent(&conv_id, target).await;
        self.store.set_pending_summary(&conv_id, reason).await;
        Ok(ToolResult::ok(format!("Handing off to {target}: {reason}")))
    }
}
```

### Updated `orchestrator.rs::run_workflow()`

Replace the hardcoded `QA_AGENT` + `FIXER_AGENT` + `RequestFixTool` block with a
call to a new generic function:

```rust
pub async fn run_dynamic_handoff(
    task: &str,
    initial_agent: &str,
    registry: &AgentRegistry,
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    resolve_ctx: Arc<ResolveContext>,
    read_tools: Vec<Arc<ErasedTool>>,
    write_tools: Vec<Arc<ErasedTool>>,
    session: &mut AgentSession,
    conversation_store: Arc<dyn ConversationStore>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    max_handoffs: usize,
) -> Result<HandoffResult, RuntimeError> {
    let handoff_store = Arc::new(HandoffAgentStore::new());

    // Instantiate all agents in the registry
    let mut orchestrator_builder = HandoffOrchestrator::builder()
        .conv_store(Arc::clone(&conversation_store))
        .agent_store(Arc::clone(&handoff_store))
        .max_handoffs(max_handoffs)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .with_approval_gate_opt(approval_gate)
        .prompt_fn(handoff_prompt_fn);

    for name in registry.agent_names() {
        let agent = registry.build_agent(
            name, task, &session.workspace_root.to_string_lossy(),
            &read_tools, &write_tools,
            Arc::clone(&handoff_store), Arc::clone(&signals),
        )?;
        orchestrator_builder = orchestrator_builder.agent(name, agent);
    }

    let orchestrator = orchestrator_builder.build();
    let result = orchestrator.run(HandoffRunParams {
        message: task.to_string(),
        conversation_id: format!("{}::{}", session.id, initial_agent),
        initial_agent: initial_agent.to_string(),
        context_state: HashMap::new(),
        signals: Some(Arc::clone(&signals)),
        max_handoffs_override: None,
    }).await.map_err(|e| RuntimeError::Agent(e.to_string()))?;

    Ok(result)
}
```

### WorkflowConfig (new type)

```rust
/// Describes the high-level workflow to execute for a task.
pub enum WorkflowConfig {
    /// Classic fixed pipeline (current behaviour, default).
    Standard,
    /// Dynamic handoff starting from `initial_agent`.
    Dynamic {
        initial_agent: String,
        max_handoffs: usize,
        /// Agents to include from the registry (None = all).
        agent_subset: Option<Vec<String>>,
    },
    /// Custom: caller provides the fully-built HandoffOrchestrator.
    Custom(Arc<HandoffOrchestrator>),
}
```

`WorkflowConfig::Standard` preserves 100% backwards compatibility.

---

## Signal Events (TUI observability)

Extend the existing `SignalBus` events with:

```rust
// In xaft-runtime/src/orchestrator.rs or a new signals.rs
XaftAgentHandoff {
    from_agent: String,
    to_agent: String,
    summary: String,
    turn: usize,
}
```

The TUI's `state.rs` `handle_event` handler maps this to:
- Push `OutputLine { kind: AgentMarker, text: "  ↝ {from} → {to}: {summary}" }` to output_lines
- Emit a `TuiEvent::AgentHandoff` for the status bar

---

## Backwards Compatibility

`run_workflow()` signature does **not change**.  The new code paths are
activated only when `XaftRuntime::run_task()` is given a `WorkflowConfig`
other than `Standard`.  A blanket `Default` impl returns `Standard`.

The existing `RequestFixTool` inside the QA↔Fixer cycle is NOT removed — it
stays as-is and continues to drive the standard workflow.

---

## Files to create / modify

| File | Change |
|---|---|
| `xaft-runtime/src/agent_registry.rs` | **New** — `AgentDefinition`, `AgentRegistry`, `AgentToolSet` |
| `xaft-runtime/src/orchestrator.rs` | Add `run_dynamic_handoff()`, `HandoffTool`, `WorkflowConfig` |
| `xaft-runtime/src/runtime.rs` | Accept optional `WorkflowConfig` in `run_task()` |
| `xaft-runtime/src/lib.rs` | Re-export new public types |
| `xaft-runtime/tests/handoff_e2e.rs` | **New** — integration tests (see below) |

---

## Testing Requirements

### Unit tests (`agent_registry.rs`)

```
agent_registry_register_and_lookup
agent_registry_build_agent_returns_agent_with_correct_tools
agent_registry_unknown_agent_returns_error
agent_registry_default_xaft_has_four_agents
```

### Unit tests (`HandoffTool`)

```
handoff_tool_writes_to_store_on_call
handoff_tool_rejects_disallowed_target
handoff_tool_missing_reason_uses_default
handoff_tool_empty_conv_id_does_not_panic
```

### Integration tests (`tests/handoff_e2e.rs`)

Each test uses `MockLlmProvider` (from `agtrs_runtime::testing`) to pre-queue LLM responses.

```
// 1. Two-agent handoff: router → specialist
handoff_router_to_specialist_completes_task
  // Queue: router calls handoff_to_agent("specialist", "needs deep analysis")
  //        specialist produces answer
  // Assert: HandoffResult.agent_name == "specialist"
  //         XaftAgentHandoff signal emitted once
  //         conversation history persisted per agent

// 2. Bidirectional: A → B → A (ping-pong)
handoff_bidirectional_returns_to_original_agent
  // Queue: agent_a calls handoff to agent_b
  //        agent_b calls handoff back to agent_a  
  //        agent_a produces final answer
  // Assert: 2 handoffs, result from agent_a

// 3. Max handoffs cap
handoff_exceeds_max_returns_last_content
  // Queue: agents keep handing off forever
  // Assert: loop terminates at max_handoffs, returns last content

// 4. Disallowed target rejected
handoff_tool_disallowed_target_does_not_switch_agent
  // Queue: agent calls handoff to unlisted target
  // Assert: ToolResult is error, active_agent not changed

// 5. Standard workflow unaffected
standard_workflow_still_works_with_registry
  // Use WorkflowConfig::Standard (default)
  // Assert: plan→coder→QA→APPROVED path works unchanged
  //         no HandoffTool injected into coder/planner

// 6. Custom agent in registry
custom_agent_registered_and_invoked
  // Register a "summarizer" agent with read_only tools
  // Queue: initial agent calls handoff_to_agent("summarizer", ...)
  // Assert: summarizer runs, summary returned

// 7. Conversation history isolation
handoff_conversation_history_isolated_per_agent
  // Run A→B→A
  // Assert: conv_store key "conv::A" has only A's messages
  //         conv_store key "conv::B" has only B's messages
  //         neither key mixes the other agent's turns

// 8. Signal bus receives handoff events
handoff_signals_emitted_for_each_transition
  // 2-agent handoff
  // Collect signals from SignalBus
  // Assert: XaftAgentHandoff emitted with correct from/to/summary

// 9. End-to-end: 3-agent chain (banking CRM style)
handoff_three_agent_chain_completes
  // Agents: "intake" → "specialist" → "approver"
  // Queue: intake hands off, specialist hands off, approver outputs final
  // Assert: ExitCode::SUCCESS, 2 handoffs in signal log

// 10. Approval gate propagated through handoff chain
handoff_approval_gate_blocks_write_tool
  // Write tool in specialist requires approval
  // Assert: PendingApproval event emitted, gate queried
```

### End-to-end with real tool execution (can use tmp dir)

```
handoff_agents_share_working_directory
  // Agent A writes a file, hands off to Agent B
  // Agent B reads that file (proves shared filesystem)
  // Assert: Agent B sees content written by Agent A
```

---

## Migration guide (for callers)

### Adding a new xaft agent (e.g. "db_migrator")

```rust
let registry = AgentRegistry::default_xaft()
    .register(AgentDefinition {
        name: "db_migrator".into(),
        system_prompt_fn: Box::new(|task, _wd| format!(
            "You are a database migration specialist. Task: {task}"
        )),
        tool_set: AgentToolSet::Custom(vec!["bash_exec".into(), "read_file".into()]),
        max_turns: 15,
        can_handoff_to: vec!["qa".into()],
    });

runtime.run_task(RunRequest {
    task: "Migrate users table to new schema".into(),
    workflow: WorkflowConfig::Dynamic {
        initial_agent: "db_migrator".into(),
        max_handoffs: 6,
        agent_subset: None,
    },
    ..Default::default()
}).await?;
```

No changes to `orchestrator.rs` required.

---

## Acceptance Criteria

1. `AgentRegistry::default_xaft()` exists and produces planner/coder/qa/fixer.
2. `HandoffTool` can be given to any agent; on call it writes to `HandoffAgentStore`.
3. `run_dynamic_handoff()` correctly cycles through any sequence of registered agents.
4. `WorkflowConfig::Standard` (default) executes the existing plan→coder→QA pipeline with zero changes.
5. All 10 integration tests pass with `MockLlmProvider`.
6. `XaftAgentHandoff` signal emitted for every agent transition.
7. Conversation history is isolated per-agent under `"{conv_id}::{agent_name}"`.
8. All existing tests in `tests/session_e2e.rs` continue to pass.
9. No public API breakage in `XaftRuntime`, `RunRequest`, or `run_workflow()`.
