# Signal Protocols

## Purpose

Signals are xaft's observability backbone. They provide a typed, asynchronous event system that decouples producers (the agent runtime, tool implementations, git operations) from consumers (the TUI, logging subsystems, external integrations). Every significant state transition in xaft—from an LLM call starting to a file being committed—emits a signal. This document catalogs all signal types, explains the emission and subscription patterns, and describes how signals flow between the xaft layer and the underlying agtrs runtime.

Understanding the signal protocol is essential for debugging agent behavior, building custom UIs, integrating with CI/CD systems, and adding new observability features. Signals are the single source of truth for what happened, when, and why.

## Mental Model

Think of signals as a **typed broadcast bus**. Producers emit signals without knowing or caring who receives them. Consumers subscribe to specific signal types and receive every instance of that type. The bus is asynchronous—emitters never block on consumers—and fire-and-forget, meaning a slow consumer cannot slow down the agent runtime.

```
Producer                    SignalBus                    Consumers
┌─────────────┐         ┌──────────────┐          ┌──────────────┐
│ LLM Client  │──emit──▶│              │──on()───▶│ TUI          │
│ Tool        │──emit──▶│  SignalBus   │──on()───▶│ Logger       │
│ Git Repo    │──emit──▶│              │──sub()──▶│ EventBridge  │
│ Planner     │──emit──▶│              │──sub()──▶│ Custom Hook  │
└─────────────┘         └──────────────┘          └──────────────┘
```

The bus supports two subscription patterns:

- **`bus.on::<T>(handler).await`**: Registers an async handler that is called for every signal of type `T`. The handler runs in a spawned task and should not block.
- **`bus.subscribe::<T>().await`**: Returns a broadcast receiver that yields signals of type `T`. Useful for consumers that need to batch or buffer signals.

## Xaft-Level Signals

These are the high-level signals emitted by the xaft agent framework:

### XaftLlmCallStarting

Emitted just before an LLM API call is made. Useful for showing "thinking..." indicators and tracking token usage.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftLlmCallStarting {
    pub conversation_key: String,
    pub model: String,
    pub prompt_tokens_estimate: usize,
}
```

### XaftCommitCreated

Emitted after a git commit is successfully created in the worktree. Contains the commit hash and message.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftCommitCreated {
    pub commit_hash: String,
    pub message: String,
    pub files_changed: Vec<String>,
}
```

### XaftPlanCreated

Emitted when the planning cascade produces a non-empty plan. Contains the goal and step descriptions.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftPlanCreated {
    pub goal: String,
    pub steps: Vec<String>,
    pub estimated_complexity: f64,
}
```

### XaftPlanEmpty

Emitted when the planning cascade cannot produce a plan for the given goal.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftPlanEmpty {
    pub goal: String,
}
```

### XaftAgentOutput

Emitted when an agent produces a text output (not a tool call). This is the agent's "speaking" output.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftAgentOutput {
    pub agent_name: String,
    pub content: String,
    pub turn: usize,
}
```

## Agtrs-Level Signals

These are lower-level signals from the agtrs runtime that underpins xaft. They provide fine-grained visibility into the model and tool execution loop:

### ModelCallStarted / ModelCallComplete

Emitted around every LLM API call at the transport level. `ModelCallStarted` includes the request metadata; `ModelCallComplete` includes the response and token counts.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCallStarted {
    pub request_id: String,
    pub model: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCallComplete {
    pub request_id: String,
    pub model: String,
    pub response_tokens: usize,
    pub prompt_tokens: usize,
    pub latency_ms: u64,
}
```

### ToolCallStarted / ToolCallComplete

Emitted around every tool invocation. `ToolCallStarted` includes the tool name and input; `ToolCallComplete` includes the result.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallStarted {
    pub tool_name: String,
    pub input_summary: String,  // Truncated for display
    pub call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallComplete {
    pub tool_name: String,
    pub call_id: String,
    pub result_summary: String,  // Truncated for display
    pub duration_ms: u64,
    pub success: bool,
}
```

### ToolPendingApproval

Emitted when a tool with `requires_confirmation() -> true` is called and the approval gate is waiting for user input.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPendingApproval {
    pub tool_name: String,
    pub input_summary: String,
    pub call_id: String,
}
```

### AgentRunComplete

Emitted when an agent finishes its run—either by completing its task, hitting the turn limit, or encountering an error.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunComplete {
    pub agent_name: String,
    pub outcome: String,  // "completed", "max_turns", "error"
    pub turns_used: usize,
    pub total_tokens: usize,
}
```

### AgentCancelled

Emitted when an agent run is cancelled by the user (Ctrl+C) or by a timeout.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCancelled {
    pub agent_name: String,
    pub reason: String,  // "user_cancel", "timeout", "handoff_budget"
}
```

### FileEditsCommitted

Emitted when file edits are committed to the workspace. This is a higher-fidelity version of the git commit signal for cases where the change tracking is at the file level.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEditsCommitted {
    pub files: Vec<FileEdit>,
    pub commit_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub edit_type: String,  // "create", "modify", "delete"
    pub lines_added: usize,
    pub lines_removed: usize,
}
```

## Emission Pattern

Signals are emitted using `try_emit_signal()`, which is a fire-and-forget pattern:

```rust
signal_bus.try_emit_signal(XaftLlmCallStarting {
    conversation_key: key.clone(),
    model: self.model_name.clone(),
    prompt_tokens_estimate: tokens,
}).await;
```

The `try_` prefix indicates that emission is best-effort. If no consumers are registered, the signal is silently dropped. If a consumer's handler panics, the signal is logged and the consumer is removed. This ensures that the signal system never blocks or crashes the producer.

Internally, `try_emit_signal` uses `tokio::spawn` to run each handler in its own task:

```rust
impl SignalBus {
    pub async fn try_emit_signal<T: Signal + 'static>(&self, signal: T) {
        let handlers = self.handlers.read().await;
        if let Some(handlers_for_type) = handlers.get::<Vec<Handler<T>>>() {
            for handler in handlers_for_type {
                let handler = handler.clone();
                let signal = signal.clone();
                tokio::spawn(async move {
                    if let Err(e) = handler.call(signal).await {
                        tracing::warn!("Signal handler error: {}", e);
                    }
                });
            }
        }

        // Also send to broadcast subscribers
        if let Some(sender) = self.broadcast_senders.get::<tokio::sync::broadcast::Sender<T>>() {
            let _ = sender.send(signal); // Ignore if no receivers
        }
    }
}
```

## Subscription Patterns

### Handler-Based Subscription

Register a closure that runs for every signal:

```rust
signal_bus.on::<XaftCommitCreated>(|signal| {
    println!("Committed: {} - {}", signal.commit_hash, signal.message);
}).await;
```

The handler runs in a spawned task. Do not perform blocking operations inside it—use `tokio::spawn` for any additional async work.

### Broadcast Receiver Subscription

Get a channel receiver for batching or buffering:

```rust
let mut rx = signal_bus.subscribe::<ToolCallComplete>().await;

tokio::spawn(async move {
    while let Ok(signal) = rx.recv().await {
        metrics::record_tool_duration(signal.tool_name, signal.duration_ms);
    }
});
```

Broadcast receivers use a bounded buffer. If the receiver falls behind, older signals are dropped to prevent unbounded memory growth. This is intentional—signals are observability data, not reliable messaging.

## Common Pitfalls

1. **Relying on signal delivery guarantees.** Signals are fire-and-forget. If you need reliable delivery (e.g., for audit logging), write to a persistent store in the handler, not in the signal bus.

2. **Slow signal handlers.** A handler that takes seconds to run will accumulate spawned tasks. Keep handlers fast (< 10ms) and delegate heavy work to separate tasks or queues.

3. **Forgetting to subscribe before emission starts.** If you subscribe after the agent has already started running, you'll miss early signals. Subscribe before calling `agent.run()`.

4. **Confusing xaft-level and agtrs-level signals.** `XaftLlmCallStarting` is a high-level signal with estimated tokens. `ModelCallStarted` is a lower-level signal with a request ID. They are emitted at different points and have different granularity. Choose the one that matches your needs.

5. **Not handling broadcast receiver lag.** If you use `subscribe()` and your receiver can't keep up, the broadcast channel will drop old messages. Check `recv()` for `RecvError::Lagged` and handle it gracefully.

6. **Subscribing to the wrong type.** The bus uses Rust's type system for routing. Subscribing to `XaftCommitCreated` won't receive `FileEditsCommitted` even though they're related. Subscribe to each type you care about independently.

## Invariants

- **Signal emission never blocks the producer.** `try_emit_signal` always returns immediately, regardless of how many consumers exist or how slow they are.
- **Signal handlers run concurrently.** Two handlers for the same signal type run in parallel; order is not guaranteed.
- **Signals are Clone.** Every signal type implements `Clone` so it can be sent to multiple consumers.
- **Broadcast receivers have a bounded buffer.** Default capacity is 256 signals. Older signals are dropped when the buffer overflows.
- **Signal structs are plain data.** They contain no references, no `Arc<Mutex<...>>`, and no interior mutability. They are snapshots of state at emission time.
- **Each signal type has a unique `std::any::TypeId`.** The bus uses type IDs for routing; two different struct types never collide.

## Examples

### TUI Integration: Displaying Tool Activity

```rust
// Subscribe to tool signals for the status bar
let mut tool_started = bus.subscribe::<ToolCallStarted>().await;
let mut tool_complete = bus.subscribe::<ToolCallComplete>().await;

tokio::spawn(async move {
    loop {
        tokio::select! {
            Ok(signal) = tool_started.recv() => {
                app_state.set_active_tool(signal.tool_name, signal.input_summary);
            }
            Ok(signal) = tool_complete.recv() => {
                app_state.clear_active_tool();
                app_state.add_tool_result(signal.tool_name, signal.success);
            }
        }
    }
});
```

### Metrics Collection

```rust
bus.on::<ModelCallComplete>(|signal| {
    HISTOGRAM_LLMS_LATENCY.record(signal.latency_ms as f64);
    COUNTER_PROMPT_TOKENS.increment(signal.prompt_tokens as u64);
    COUNTER_RESPONSE_TOKENS.increment(signal.response_tokens as u64);
}).await;
```

### Debug Logging

```rust
bus.on::<ToolPendingApproval>(|signal| {
    tracing::info!(
        tool = %signal.tool_name,
        input = %signal.input_summary,
        "Tool pending user approval"
    );
}).await;
```
