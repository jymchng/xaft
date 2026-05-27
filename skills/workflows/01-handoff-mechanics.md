# Handoff Mechanics

## Purpose

Handoff mechanics enable multi-agent workflows in xaft by allowing one agent to delegate execution to another specialized agent. Rather than a single monolithic agent trying to handle every task, the system partitions work among agents with focused expertise—planning, coding, fixing, reviewing—and hands control between them in a structured, auditable way. This document covers the core handoff protocol: how `HandoffTool` validates and executes a transfer, how `HandoffAgentStore` tracks the active agent and pending summary, how `RequestFixTool` triggers the fixer sub-agent, how the handoff budget prevents infinite delegation loops, and how the orchestrator determines which agent runs next.

Understanding handoff mechanics is critical for anyone building multi-agent workflows, debugging delegation failures, or reasoning about the control flow of a xaft session that spans multiple specialized agents.

## Mental Model

Think of a handoff as a **controlled baton pass** in a relay race. The currently running agent (the "source") decides it cannot or should not continue, selects a target agent, writes a summary of what it has done and what the target should do, and then the orchestrator wakes up the target agent with that summary as context.

```
Agent A (running)
    │
    ├─ Decides to hand off
    │
    ├─ Calls HandoffTool { target: "agent_b", summary: "I've analyzed the bug..." }
    │
    ├─ HandoffTool validates:
    │   ├─ target in allowed_targets? ✓
    │   ├─ handoff count < max_handoffs? ✓
    │   └─ target agent exists in registry? ✓
    │
    ├─ HandoffTool writes to HandoffAgentStore:
    │   ├─ active_agent = "agent_b"
    │   └─ pending_summary = "I've analyzed the bug..."
    │
    ├─ Agent A's turn ends
    │
    ▼
HandoffOrchestrator
    │
    ├─ Reads store: active_agent = "agent_b"
    ├─ Looks up agent_b in AgentRegistry
    ├─ Injects pending_summary as system/user message
    └─ Starts agent_b's event loop
    │
    ▼
Agent B (running with context from Agent A)
```

The key data structure is `HandoffAgentStore`, which holds:

- **`active_agent: String`** — The name of the agent currently in control.
- **`pending_summary: Option<String>`** — A summary the previous agent wrote for the next agent.
- **`handoff_count: usize`** — How many handoffs have occurred in this session.
- **`conversation_keys: HashMap<String, String>`** — Per-agent conversation identifiers so each agent's message history is isolated.

## Extension Patterns

### HandoffTool: The Primary Delegation Mechanism

`HandoffTool` is the tool that agents use to transfer control. Its input schema requires:

```rust
#[derive(Deserialize)]
struct HandoffInput {
    target: String,     // Name of the target agent
    summary: String,    // Context summary for the target
}
```

The tool's `call()` method performs these steps:

1. **Validate target against `allowed_targets`**: The current agent's `AgentDefinition` specifies which agents it can hand off to. If the target is not in that set, the handoff is rejected with `ToolResult::Error`.

2. **Check handoff budget**: If `handoff_count >= max_handoffs` (default: 14), the handoff is rejected. This prevents infinite delegation loops.

3. **Verify target exists in registry**: The target agent name must correspond to a registered agent. Unknown targets are rejected.

4. **Write to store**: Update `active_agent`, set `pending_summary`, increment `handoff_count`.

5. **Return success**: The current agent's turn ends, and the orchestrator picks up the new active agent.

```rust
async fn call(&self, input: HandoffInput, ctx: &ToolContext) -> ToolResult {
    // Step 1: validate target
    if !self.allowed_targets.contains(&input.target) {
        return ToolResult::Error(format!(
            "Cannot hand off to '{}': not in allowed targets {:?}", 
            input.target, self.allowed_targets
        ));
    }

    // Step 2: check budget
    let store = self.store.read().await;
    if store.handoff_count >= self.max_handoffs {
        return ToolResult::Error(format!(
            "Handoff budget exhausted ({} handoffs used)", store.handoff_count
        ));
    }

    // Step 3: verify target exists
    if !self.registry.contains(&input.target) {
        return ToolResult::Error(format!("Unknown agent: '{}'", input.target));
    }

    // Step 4: write to store
    drop(store); // release read lock
    let mut store = self.store.write().await;
    store.active_agent = input.target.clone();
    store.pending_summary = Some(input.summary.clone());
    store.handoff_count += 1;

    // Step 5: return success
    ToolResult::Ok(json!({
        "handed_off_to": input.target,
        "handoff_number": store.handoff_count,
    }))
}
```

### RequestFixTool: Specialized Fixer Handoff

`RequestFixTool` is a convenience tool that performs a handoff to the built-in "fixer" agent. It's equivalent to calling `HandoffTool` with `target: "fixer"`, but with a simplified API:

```rust
#[derive(Deserialize)]
struct RequestFixInput {
    error_description: String,  // What went wrong
    file_path: Option<String>,  // Optional file to fix
}
```

Internally, `RequestFixTool` sets `active_agent = "fixer"` and constructs a summary from the error description and file path. This is useful when an agent encounters a compilation error or test failure and wants to delegate the fix.

```rust
async fn call(&self, input: RequestFixInput, ctx: &ToolContext) -> ToolResult {
    let summary = match input.file_path {
        Some(path) => format!("Error in {}: {}", path, input.error_description),
        None => input.error_description.clone(),
    };

    let mut store = self.store.write().await;
    store.active_agent = "fixer".to_string();
    store.pending_summary = Some(summary);
    store.handoff_count += 1;

    ToolResult::Ok(json!({ "fixer_active": true }))
}
```

### HandoffOrchestrator: Determining the Next Agent

The `HandoffOrchestrator` runs after each agent turn. It reads the `HandoffAgentStore` and decides what to do:

1. If `active_agent` changed (a handoff occurred), look up the new agent in the registry.
2. Retrieve or create the agent's conversation key so it has its own isolated message history.
3. Inject the `pending_summary` as the first user message for the new agent.
4. Start the new agent's event loop.

```rust
impl HandoffOrchestrator {
    pub async fn run_next(&self) -> Result<AgentRunOutcome> {
        let store = self.agent_store.read().await;

        if store.active_agent != self.last_active_agent {
            // Handoff detected
            let agent_def = self.registry.get(&store.active_agent)?;
            let conversation_key = store.conversation_keys
                .get(&store.active_agent)
                .cloned()
                .unwrap_or_else(|| {
                    let key = format!("{}-{}", store.active_agent, uuid::Uuid::new_v4());
                    key
                });

            if let Some(summary) = &store.pending_summary {
                self.message_store.inject_user_message(
                    &conversation_key, summary.clone()
                ).await?;
            }

            let outcome = self.executor.run(
                &agent_def, &conversation_key
            ).await?;

            self.last_active_agent = store.active_agent.clone();
            return Ok(outcome);
        }

        Ok(AgentRunOutcome::NoHandoff)
    }
}
```

## Common Pitfalls

1. **Circular handoff loops.** Agent A hands off to Agent B, which hands off back to Agent A, creating an infinite cycle. The `max_handoffs=14` budget prevents this from running forever, but the budget is generous enough that the cycle can waste significant time and tokens before it's caught. Design your `allowed_targets` graphs to be acyclic or at least to converge.

2. **Omitting the summary.** A handoff without a meaningful summary leaves the target agent blind. The target has no memory of the source's reasoning, so the summary is its only context. A summary like "see above" is useless because each agent has its own conversation history.

3. **Forgetting to set `allowed_targets`.** If an agent's `AgentDefinition` has an empty `can_handoff_to` list, it cannot hand off at all. This is sometimes intentional (leaf agents) but often an oversight that causes confusing "not in allowed targets" errors.

4. **Sharing conversation history incorrectly.** Each agent should have its own conversation key. If two agents share a key, they'll see each other's messages, leading to confusion and duplicated work. The `conversation_keys` map in `HandoffAgentStore` ensures isolation.

5. **Ignoring handoff count in the orchestrator.** The orchestrator must respect `max_handoffs` and terminate the session gracefully when the budget is exhausted, rather than continuing with the last active agent indefinitely.

6. **Handoff during tool execution.** A handoff should only occur at the end of an agent turn, not in the middle of a tool call. If a tool triggers a handoff mid-execution, the tool's result may be lost.

## Invariants

- **`max_handoffs` is a hard upper bound.** No session can exceed this number of handoffs. The default is 14, which accommodates complex multi-step workflows while preventing runaway loops.
- **`active_agent` always names a valid registered agent.** The `HandoffTool` validates this before updating the store.
- **`pending_summary` is always set when a handoff occurs.** The HandoffTool requires a non-empty summary string.
- **Conversation keys are unique per agent per session.** No two agents share the same conversation key.
- **The orchestrator runs after every agent turn.** It is the single point of truth for which agent runs next.
- **`allowed_targets` is a static property of the agent definition.** It cannot change at runtime.

## Examples

### Three-Agent Planning-Coding-Review Workflow

```rust
let planner = AgentDefinition {
    name: "planner".into(),
    system_prompt_fn: Arc::new(|_| "You are a planning agent. Analyze the task and create a plan.".into()),
    tool_set: vec!["handoff".into(), "read_file".into()],
    max_turns: 5,
    can_handoff_to: vec!["coder".into()],
};

let coder = AgentDefinition {
    name: "coder".into(),
    system_prompt_fn: Arc::new(|_| "You are a coding agent. Implement the plan.".into()),
    tool_set: vec!["handoff".into(), "read_file".into(), "write_file".into(), "shell".into()],
    max_turns: 20,
    can_handoff_to: vec!["reviewer".into(), "fixer".into()],
};

let reviewer = AgentDefinition {
    name: "reviewer".into(),
    system_prompt_fn: Arc::new(|_| "You review code for correctness and style.".into()),
    tool_set: vec!["handoff".into(), "read_file".into(), "shell".into()],
    max_turns: 5,
    can_handoff_to: vec!["coder".into(), "fixer".into()],
};
```

### Handoff Flow Trace

```
[Turn 1] planner: "I need to read the existing code first."
[Turn 2] planner: reads src/main.rs
[Turn 3] planner calls HandoffTool { target: "coder", summary: "The task requires adding error handling to src/main.rs. The function `process()` on line 42 lacks Result propagation. Add proper error types and propagate errors up to main()." }
[Turn 4] coder: "I'll add error handling to process()."
[Turn 5] coder: writes src/main.rs with error handling
[Turn 6] coder calls HandoffTool { target: "reviewer", summary: "I've added Result<(), AppError> return type to process() and propagated errors. Please review the error type definitions in src/errors.rs." }
[Turn 7] reviewer: reads src/main.rs and src/errors.rs
[Turn 8] reviewer: "The error handling looks correct. I'll hand off to confirm completion." → calls HandoffTool { target: "planner", summary: "Review passed. Error handling is correct and idiomatic." }
[Turn 9] planner: "Task complete. The error handling has been implemented and reviewed."
```

This trace shows a 4-handoff cycle (planner→coder→reviewer→planner), well within the 14-handoff budget.
