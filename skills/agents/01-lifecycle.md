# Agent Lifecycle

## Purpose

This document describes the complete lifecycle of an xaft agent, from construction through execution to completion. Every hook, every signal emission, and every state transition is documented here. Understanding the agent lifecycle is essential for debugging agent behavior, adding custom lifecycle hooks, and implementing new agent types that integrate correctly with the orchestrator and the signal bus.

The agent lifecycle is the most complex subsystem in xaft because it is the intersection of LLM interaction, tool execution, signal emission, session persistence, and git operations. Every other subsystem participates in the lifecycle at some point: the config crate provides parameters, the tools crate provides capabilities, the session crate records history, the signal bus distributes events, and the TUI renders the current state. If you understand the lifecycle, you understand how all these subsystems collaborate.

---

## Mental Model

Think of an agent's lifecycle as a state machine with well-defined transitions:

```
Constructed → Started → [LLM Call → Tool Execution → Observation]* → Completed
```

The asterisk denotes the ReAct loop: the agent repeatedly calls the LLM, executes the resulting tool calls, and feeds the observations back to the LLM. Each iteration of this loop is called a "turn." The loop continues until the LLM produces a response without tool calls (indicating the agent considers its work done) or until a configured limit is reached (max turns, cost limit).

At every state transition, the agent executes a lifecycle hook. These hooks are the extension points where custom behavior can be injected without modifying the core loop. They also serve as signal emission points, ensuring that every external observer is notified of the agent's progress in real time.

---

## Architecture Explanation

### Construction via `AgentBuilder.build()`

An agent is constructed using the `AgentBuilder` fluent API. The builder collects all configuration parameters and produces an `XaftAgent` struct when `build()` is called. The construction process performs validation: required fields (name, role, tools) must be set, and optional fields (temperature, max_turns, cost_limit) receive defaults from the configuration if not explicitly provided.

The `XaftAgent` struct contains:

- **name** — The agent's identifier, used for conversation key construction, session partitioning, and signal attribution.
- **role** — The system prompt that defines the agent's behavior. This is the most important field: it tells the LLM what kind of specialist it is and what tools it should use.
- **tools** — A `Vec<Box<dyn Tool>>` containing the agent's available tools. These are the only tools the LLM can invoke during the ReAct loop.
- **commit_policy** — Determines whether and when the agent auto-commits its changes. Options include `Never`, `OnSuccess`, `Always`, and `OnExplicitRequest`.
- **stream_sink** — A `mpsc::Sender<Signal>` that forwards tool results and lifecycle events to the signal bus in real time.
- **max_turns** — The maximum number of ReAct iterations before the agent is forcefully terminated.
- **temperature** — The LLM sampling temperature for this agent.
- **cost_limit** — An optional dollar limit on LLM API costs. If exceeded, the agent terminates with a `CostLimitExceeded` error.
- **call_index** — A counter that tracks the number of LLM calls made during this agent's execution, initialized to zero at construction.

The builder pattern ensures that the `XaftAgent` is always fully configured before it starts executing. There is no "default agent" that can be accidentally constructed with missing fields.

### Agent Trait and `XaftAgent` Implementation

The `Agent` trait defines the lifecycle interface:

```rust
#[async_trait]
pub trait Agent {
    async fn on_start(&mut self, context: &AgentContext);
    async fn before_llm_call(&mut self, call_index: usize) -> Vec<Message>;
    async fn on_tool_result(&mut self, result: &ToolResult);
    async fn on_turn_complete(&mut self, usage: &TokenUsage);
    async fn on_finish(&mut self, output: AgentOutput);
}
```

The `XaftAgent` struct implements this trait with the following behavior at each hook:

### `on_start` — Setting Context

Called once, before the first LLM call. The agent receives an `AgentContext` that contains the session ID, the conversation key, the worktree root, and any handoff context from the previous agent. The `on_start` hook:

1. **Sets the internal context** — Stores the session ID, conversation key, and worktree root in the agent's fields for use in subsequent hooks.
2. **Emits `XaftAgentStarting` signal** — Notifies subscribers that the agent is about to begin its ReAct loop. This signal includes the agent name, the session ID, and the conversation key.
3. **Constructs the initial message list** — Combines the system prompt (from the role field), the handoff context (if any), and the user's original prompt into the message sequence that will be sent to the LLM on the first call.

This hook is the last chance to modify the agent's state before the ReAct loop begins. Common uses include: injecting additional context from the session store, loading project-specific configuration, or adding dynamic system messages based on the worktree state.

### `before_llm_call` — Pre-Call Hook

Called before every LLM API call. The agent receives the current `call_index` (zero-based, incremented after each call). The `before_llm_call` hook:

1. **Increments the call counter** — `self.call_index += 1`. This counter is used for logging, debugging, and enforcing the max_turns limit.
2. **Emits `XaftLlmCallStarting` signal** — Includes the agent name, the call index, and the number of messages in the conversation. This signal enables subscribers to track the agent's progress and estimate remaining turns.
3. **Returns additional messages** — The hook can return a `Vec<Message>` that will be prepended to the conversation before the LLM call. This is used for dynamic context injection: for example, the Planner agent might inject a "You have made N refinement iterations so far" message to help the LLM track its own progress.

This hook is called on every iteration of the ReAct loop, including the first one. The call index starts at zero and increments by one for each call, so the first call has index 0, the second has index 1, and so on.

### The ReAct Loop

Between `before_llm_call` and `on_tool_result`, the agent runs its ReAct loop:

1. **Send messages to the LLM** — The conversation history (including any messages injected by `before_llm_call`) is serialized into the provider's request format, along with the tool definitions.
2. **Receive the LLM response** — The response may contain text content, tool calls, or both.
3. **If the response contains tool calls**, execute each tool call:
   a. Look up the tool by name in the agent's tool set.
   b. Deserialize the tool call parameters.
   c. Call `tool.execute(params)`.
   d. Collect the result.
4. **If the response contains no tool calls**, the agent considers its work done. The loop terminates, and `on_finish` is called.
5. **Append the LLM response and tool results to the conversation history** — This ensures that the next LLM call has full context of what happened in previous turns.
6. **Check termination conditions** — If `call_index >= max_turns` or cumulative cost exceeds `cost_limit`, terminate the loop with an appropriate error.

The ReAct loop is the core of the agent's intelligence. By iteratively calling the LLM and executing tools, the agent can handle complex tasks that require multiple steps of reasoning and action. Each turn builds on the results of previous turns, creating a chain of evidence that the LLM uses to make progressively better decisions.

### `on_tool_result` — Result Processing

Called after each tool execution. The agent receives a `ToolResult` containing the tool name, the input parameters, the output (or error), and the execution duration. The `on_tool_result` hook:

1. **Forwards the result to `stream_sink`** — The tool result is packaged as a signal and sent to the signal bus. This enables the TUI to display tool results in real time, even before the agent's turn completes.
2. **Checks for special tool results** — If the tool result is a `HandoffResult` or a `RequestFixResult`, the hook sets internal flags that the ReAct loop checks to determine whether to continue or terminate.
3. **Logs the result** — The tool name, execution duration, and result summary are logged at the debug level for post-hoc analysis.

This hook is critical for real-time observability. Without it, the TUI would have no way to show what the agent is doing until the entire turn completes. The stream sink ensures that every tool invocation is visible as it happens.

### `on_turn_complete` — Turn Summary

Called after each complete turn of the ReAct loop (i.e., after all tool calls in a single LLM response have been executed). The agent receives the cumulative `TokenUsage` for the session. The `on_turn_complete` hook:

1. **Logs usage metrics** — The total prompt tokens, completion tokens, and estimated cost are logged at the info level. This helps users understand the resource consumption of their tasks.
2. **Emits `XaftTurnComplete` signal** — Includes the agent name, the turn number, and the usage metrics. This enables the TUI to update its cost and progress displays.
3. **Persists the turn to the session store** — The conversation history (including the LLM response and tool results) is appended to the session record. This ensures that the session can be resumed even if the process crashes mid-turn.

This hook is called on every turn, including the last one. It is the primary mechanism for cost tracking and session persistence. If you need to add custom metrics or alerts, this is the hook to use.

### `on_finish` — Completion and Cleanup

Called once, after the ReAct loop terminates (either normally or due to a limit). The agent receives an `AgentOutput` that summarizes the agent's work. The `on_finish` hook:

1. **Emits `Done` signal** — A generic signal indicating that the agent has finished its work. This is used by the orchestrator to know when to check for handoffs.
2. **Emits `XaftAgentOutput` signal** — A detailed signal containing the agent's final output: the text response, any modified files, the cumulative token usage, and the termination reason (normal, max_turns_exceeded, cost_limit_exceeded, or error).
3. **Triggers `maybe_auto_commit`** — Based on the agent's `CommitPolicy`, the hook may auto-commit the changes in the worktree. The commit message is generated from the agent's output summary.
4. **Persists the final state** — The complete conversation history and the agent's output are written to the session store. This is the definitive record of what the agent did.

The `maybe_auto_commit` logic deserves special attention. The commit policies work as follows:

- **`CommitPolicy::Never`** — Never auto-commit. Changes remain in the worktree for the user to review and commit manually.
- **`CommitPolicy::OnSuccess`** — Auto-commit only if the agent completed successfully (no error termination). This is the default for the Coder and Fixer agents.
- **`CommitPolicy::Always`** — Auto-commit regardless of termination reason. Useful for agents that make incremental progress even on failure.
- **`CommitPolicy::OnExplicitRequest`** — Auto-commit only if the agent explicitly called a `CommitTool` during its execution. This gives the LLM control over when to commit, enabling it to make intermediate commits during long coding sessions.

---

## Extension Patterns

### Adding a Custom Lifecycle Hook

The `Agent` trait's hooks are virtual methods with default implementations that do nothing. To add custom behavior, implement the trait for your own agent type and override the hooks you need:

```rust
struct InstrumentedAgent {
    inner: XaftAgent,
    metrics: MetricsCollector,
}

#[async_trait]
impl Agent for InstrumentedAgent {
    async fn on_start(&mut self, context: &AgentContext) {
        self.metrics.record_start(self.inner.name(), context.session_id);
        self.inner.on_start(context).await;
    }

    async fn on_turn_complete(&mut self, usage: &TokenUsage) {
        self.metrics.record_usage(self.inner.name(), usage);
        self.inner.on_turn_complete(usage).await;
    }

    // Delegate all other hooks to the inner agent
}
```

### Adding a Custom Signal at a Lifecycle Boundary

To emit a custom signal when an agent reaches a specific state, override the relevant hook and emit the signal before or after calling the base implementation:

```rust
async fn before_llm_call(&mut self, call_index: usize) -> Vec<Message> {
    if call_index == 0 {
        self.stream_sink.send(XaftFirstLlmCall {
            agent_name: self.name.clone(),
        }).await.ok();
    }
    Vec::new() // No additional messages
}
```

### Adding a Custom Commit Policy

To add a new commit policy (e.g., `OnTestPass`), extend the `CommitPolicy` enum and implement the decision logic in `maybe_auto_commit`. The logic should check the session's test results and only commit if all tests passed. This requires access to the session store from within the `on_finish` hook, which is available through the `AgentContext` set during `on_start`.

---

## Common Pitfalls

1. **Modifying the conversation history outside of hooks.** The ReAct loop manages the conversation history. If you modify it outside of the `before_llm_call` hook (which can prepend messages), the history may become inconsistent, causing the LLM to produce unexpected responses or tool calls.

2. **Ignoring the `call_index` in `before_llm_call`.** The call index is the only way to track progress within the ReAct loop. If you need to inject context that depends on how many calls have been made (e.g., "you have 3 turns remaining"), use the call index to compute the remaining turns.

3. **Blocking in `on_tool_result`.** This hook runs synchronously within the ReAct loop. If it blocks (e.g., on a network call or a lock), the entire agent stalls. Always use async operations or forward work to a background task.

4. **Forgetting to call the base implementation when overriding hooks.** If you override a hook without calling the base `XaftAgent` implementation, you will lose the default behavior (signal emission, usage logging, session persistence). Always delegate to the base unless you intentionally want to suppress the default behavior.

5. **Relying on tool execution order.** When the LLM returns multiple tool calls in a single response, they may be executed in any order (or in parallel, depending on the configuration). Do not assume that tool calls are executed in the order they appear in the LLM response.

6. **Assuming `on_finish` is always called.** If the agent panics or the process is killed, `on_finish` is not called. If you need guaranteed cleanup, implement it in the `Drop` trait, not in `on_finish`.

---

## Invariants

- **I1: Hook call order.** Hooks are called in the order: `on_start` → (`before_llm_call` → tool execution → `on_tool_result` → `on_turn_complete`)* → `on_finish`. There are no deviations from this order.
- **I2: Single execution of `on_start` and `on_finish`.** These hooks are called exactly once per agent invocation. They are never retried or skipped.
- **I3: `call_index` monotonicity.** The call index starts at 0 and increases by exactly 1 for each LLM call. It never decreases or skips values.
- **I4: Session persistence per turn.** After each `on_turn_complete`, the session store has a complete record of all turns up to and including the current one.
- **I5: Signal emission guarantees.** Every hook emits its designated signal before returning. If a signal emission fails (e.g., the channel is full), it is logged but does not prevent the hook from completing.

---

## Lifecycle Expectations

**Initialization (Construction → `on_start`):** This phase is instantaneous. The agent is constructed, `on_start` is called, and the ReAct loop begins. There is no "idle" state between construction and execution.

**Execution (`before_llm_call` → tools → `on_tool_result` → `on_turn_complete`):** This phase is the longest. Each turn may take seconds (for simple tool calls) or minutes (for complex LLM responses or long-running shell commands). The number of turns is bounded by `max_turns`, but there is no per-turn timeout unless the agent implements one.

**Completion (`on_finish`):** This phase is brief but critical. It involves signal emission, session persistence, and potentially a git commit. The git commit can be slow if the worktree has many changes, but it is the last operation before the agent is considered "done."

**Post-completion:** After `on_finish` returns, the agent struct is dropped. The `Drop` implementation performs no additional work (all cleanup happens in `on_finish`). The agent's conversation history remains in the session store for future reference or resumption.

---

## Examples

### Full lifecycle trace for a Coder agent

```text
Agent: Coder (max_turns=15, temperature=0.3, CommitPolicy::OnSuccess)

1. AgentBuilder.build() → XaftAgent { name: "coder", call_index: 0, ... }

2. on_start(context={session_id: "abc", key: "abc::workflow", worktree: "/tmp/wt-abc"})
   → Emits XaftAgentStarting { agent: "coder", session: "abc" }
   → Sets internal context fields

3. before_llm_call(call_index=0)
   → Increments call_index to 1
   → Emits XaftLlmCallStarting { agent: "coder", call: 1 }
   → Returns [] (no additional messages)

4. LLM returns: [ToolCall { name: "read_file", params: {"path": "src/main.rs"} }]

5. on_tool_result(ToolResult { name: "read_file", output: "fn main() { ... }", duration: 12ms })
   → Forwards to stream_sink → TUI displays file contents
   → Logs at debug level

6. on_turn_complete(usage={prompt: 1500, completion: 200, cost: $0.003})
   → Emits XaftTurnComplete { agent: "coder", turn: 1, usage: ... }
   → Persists turn to session store

7. before_llm_call(call_index=1)
   → Increments call_index to 2
   → Emits XaftLlmCallStarting { agent: "coder", call: 2 }

8. LLM returns: [ToolCall { name: "edit_file", params: {"path": "src/main.rs", "edits": [...]} }]

9. on_tool_result(ToolResult { name: "edit_file", output: "File modified", duration: 5ms })
   → Forwards to stream_sink → TUI shows edit summary

10. on_turn_complete(usage={prompt: 2800, completion: 400, cost: $0.007})

11. LLM returns: [Text { content: "I have implemented the validation logic..." }]

12. on_finish(output={text: "I have implemented...", files: ["src/main.rs"], usage: ...})
    → Emits Done { agent: "coder" }
    → Emits XaftAgentOutput { agent: "coder", output: ... }
    → maybe_auto_commit(CommitPolicy::OnSuccess) → git commit -m "Add validation logic"
    → Persists final state to session store
```

### Agent terminated by max_turns

```text
Agent: QA (max_turns=5)

1-8. [Normal ReAct loop for turns 1-4]

9. before_llm_call(call_index=4) → still under max_turns
10. Turn 4 completes

11. before_llm_call(call_index=5) → call_index == max_turns
    → ReAct loop detects limit exceeded
    → Terminates with MaxTurnsExceeded

12. on_finish(output={termination: MaxTurnsExceeded, partial_results: [...]})
    → Emits Done and XaftAgentOutput (with termination reason)
    → maybe_auto_commit(CommitPolicy::Never) → no commit
```

---

## Implementation Guidance

When implementing a new agent type or modifying lifecycle behavior, follow these guidelines:

1. **Start with the `AgentBuilder`.** Use the builder to construct a standard `XaftAgent` and verify that the default lifecycle works correctly. Only then consider customizing hooks.

2. **Override hooks minimally.** Each hook has a default implementation that does the right thing (emit signals, log usage, persist data). Only override a hook if you need to add behavior, and always call the base implementation unless you have a specific reason not to.

3. **Test lifecycle hooks in isolation.** Use `XaftRuntime::for_testing()` with a mock provider that returns canned responses. Verify that each hook is called in the expected order and that signals are emitted correctly. Check the session store to confirm that conversation history is persisted after each turn.

4. **Respect the `stream_sink` backpressure.** The stream sink is an mpsc channel with a bounded capacity. If the TUI is slow to consume signals, the channel may fill up. When this happens, `send()` returns an error. Handle this gracefully by logging the dropped signal rather than panicking or blocking.

5. **Consider the cost of `on_finish` operations.** Auto-committing and session persistence can be slow. If you add expensive operations to `on_finish`, consider moving them to a background task so that the orchestrator is not blocked waiting for the agent to finish.
