# Async Conventions

This document describes the async programming conventions used throughout xaft. These conventions ensure consistent behavior across the codebase, prevent common async pitfalls (deadlocks, cancelled operations that keep running, unbounded concurrency), and make the code easier to reason about for contributors.

---

## Tokio as the Async Runtime

xaft uses `tokio` as its async runtime. The binary crate (`xaft`) initializes the runtime using `#[tokio::main]`, which creates a multi-threaded runtime with the default configuration (one worker thread per CPU core). The runtime is never created manually — this ensures consistent configuration across all entry points (the binary, integration tests, and benchmarks).

```rust
// xaft/src/main.rs
#[tokio::main]
async fn main() -> ExitCode {
    let result = xaft_cli::run().await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::from(e.exit_code() as u8)
        }
    }
}
```

The multi-threaded runtime is important because xaft runs multiple concurrent tasks: the LLM streaming task, the TUI render loop, the signal bus dispatch loop, and the cost tracker aggregation task. These tasks must run concurrently to avoid blocking each other. A single-threaded runtime would serialize all tasks, causing the TUI to freeze while waiting for an LLM response.

---

## tokio::spawn for Fire-and-Forget

The `tokio::spawn` function is used for fire-and-forget tasks that should run independently of the current task's lifetime. In xaft, fire-and-forget tasks are used for:

1. **Stream consumers** — Background tasks that read from the broadcast channel and process events (logging, cost tracking).
2. **Signal bus dispatch** — When a signal is published, each subscriber's callback is invoked in a spawned task.
3. **Health checks** — Provider health checks run in the background at startup, and the runtime does not block on their completion.
4. **File I/O** — Tool implementations that perform file operations spawn a blocking task using `tokio::task::spawn_blocking` to avoid blocking the async runtime.

```rust
// Fire-and-forget stream consumer
tokio::spawn(async move {
    let mut rx = broadcast_rx.resubscribe();
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Err(e) = process_event(event).await {
                    tracing::warn!("Event processing error: {}", e);
                }
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("Stream consumer lagged, dropped {} events", n);
            }
            Err(RecvError::Closed) => break,
        }
    }
});
```

**Important convention:** Every spawned task must handle the `CancellationToken` or check for channel closure. Spawned tasks that run indefinitely without a termination condition are a resource leak. The most common pattern is to loop on `rx.recv().await` and break when the channel closes, or to `select!` on both the work and the cancellation token.

Spawned tasks must be `Send + 'static`. This means they cannot capture references to stack-local data. If a spawned task needs access to shared state, it must capture an `Arc` clone. This is a deliberate constraint — `Send + 'static` ensures that the task can be moved to any thread in the tokio thread pool and that it does not hold dangling references after the spawning scope exits.

---

## CancellationToken Usage

The `tokio_util::sync::CancellationToken` is xaft's primary cancellation mechanism. It is used instead of `tokio::task::JoinHandle::abort()` because cancellation is cooperative — the cancelled task receives the signal and can perform cleanup before exiting, rather than being abruptly terminated.

### Cancellation Propagation

The `CancellationToken` is created at the runtime level and cloned for each agent turn. When the user presses Ctrl+C or the runtime initiates a shutdown, the root token is cancelled, and all cloned tokens are notified. This hierarchical propagation ensures that cancellation reaches every running task without requiring explicit wiring.

```rust
// Runtime-level cancellation
let root_cancel = CancellationToken::new();
let agent_cancel = root_cancel.child_token(); // clone for the agent

// When the user presses Ctrl+C
root_cancel.cancel(); // cancels all child tokens

// Inside the agent's turn
async fn turn(&self, input: TurnInput, cancel: CancellationToken) -> Result<TurnOutput, AgentError> {
    let tool_cancel = cancel.child_token(); // clone for each tool call
    
    loop {
        // Check cancellation at the top of each iteration
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        
        // Call the LLM
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            r = self.provider.stream(request) => r?,
        };
        
        // Execute tools
        for tool_call in response.tool_calls {
            let result = self.execute_tool(tool_call, tool_cancel.clone()).await?;
            // ...
        }
    }
}
```

### Child Tokens for Nested Cancellation

Child tokens allow fine-grained cancellation. The runtime can cancel a specific agent's turn without cancelling other agents. This is used when the user cancels a tool approval (which should cancel the current tool call but not the entire agent session) and when the planner reassigns a task from one agent to another (which should cancel the original agent's turn).

```rust
// Cancel a specific tool call without cancelling the entire agent
let tool_cancel = CancellationToken::new();

// If the user rejects the approval, cancel the tool
if user_rejects {
    tool_cancel.cancel();
}

// The tool checks its own cancellation token
let result = tokio::select! {
    biased;
    _ = tool_cancel.cancelled() => Err(ToolError::Cancelled),
    r = tool.execute(input, tool_cancel.clone()) => r,
};
```

### Cancellation Cleanup

When a task is cancelled, it should clean up any resources it holds. The most common cleanup pattern is to use a `scopeguard` or a `defer` block that runs when the function exits, regardless of whether it exits normally or via cancellation.

```rust
use scopeguard::defer;

async fn turn(&self, input: TurnInput, cancel: CancellationToken) -> Result<TurnOutput, AgentError> {
    // Acquire a lock on shared state
    let mut state = self.state.lock().await;
    defer! {
        // This runs even if the task is cancelled
        // Note: defer! doesn't work across await points in the general case,
        // but it works here because we drop the guard before the next await
    };
    
    // ... do work ...
    
    // Drop the lock explicitly before awaiting
    drop(state);
    
    // Now it's safe to await
    let response = self.provider.stream(request).await?;
    
    Ok(TurnOutput::new(response))
}
```

In practice, xaft uses `Drop` implementations on guard types rather than `scopeguard`. The `AgentGuard` type, for example, releases the agent's lock and updates its status in the `Drop` implementation. This ensures that cleanup always runs, even if the task is cancelled mid-turn.

---

## tokio::select!{biased} Pattern

The `tokio::select!` macro is used extensively in xaft to race multiple async operations against each other. The `biased` modifier ensures that the branches are checked in declaration order, with the first ready branch winning. Without `biased`, `select!` randomly picks between ready branches, which makes the system non-deterministic and harder to debug.

The convention is to always put the cancellation branch first, followed by other control branches (timeouts, signal bus events), and the work branch last. This ordering ensures that cancellation is always responsive — if both the cancellation token and the work are ready simultaneously, cancellation wins.

```rust
tokio::select! {
    biased;

    // 1. Cancellation — always checked first
    _ = cancel.cancelled() => {
        tracing::info!("Agent turn cancelled");
        return Err(AgentError::Cancelled);
    }

    // 2. Approval responses — high-priority control events
    approval = approval_rx.recv() => {
        self.handle_approval(approval?).await?;
    }

    // 3. Timeout — prevents indefinite waits
    _ = tokio::time::sleep(Duration::from_secs(300)) => {
        tracing::warn!("Agent turn timed out");
        return Err(AgentError::Internal("turn timeout".into()));
    }

    // 4. The actual work — lowest priority
    result = self.provider.stream(request) => {
        self.handle_stream_response(result?).await?;
    }
}
```

**Why biased?** Without `biased`, if both cancellation and the work future are ready, `select!` would randomly pick one. In 50% of cases, the work would proceed despite the cancellation signal, which is incorrect behavior. With `biased`, cancellation always wins, ensuring deterministic and correct cancellation handling.

**When not to use biased:** The unbiased `select!` is appropriate when the branches are symmetric and there is no priority between them. For example, when waiting for the first response from two redundant LLM providers, unbiased selection provides natural load balancing. However, xaft does not currently use redundant providers, so all `select!` calls use `biased`.

---

## Oneshot Channels for Approval

Approval requests use `tokio::sync::oneshot` channels for the response. The runtime creates a oneshot channel, includes the `Sender` half in the `ApprovalRequest` event, and awaits the `Receiver` half. The TUI (or the approval gate implementation) receives the event, presents the approval dialog, and sends the decision through the oneshot sender.

```rust
// Runtime side — request approval and wait for the response
let (tx, rx) = tokio::sync::oneshot::channel();

let request = ApprovalRequest {
    tool_name: tool.name().to_string(),
    input: input.clone(),
    response_tx: tx,  // TUI will send the decision through this
};

// Publish the approval request
self.stream_sink.send(StreamEvent::ApprovalRequest {
    tool_name: request.tool_name.clone(),
    input: request.input.clone(),
    response_tx: request.response_tx,
}).await?;

// Wait for the response
let decision = tokio::select! {
    biased;
    _ = cancel.cancelled() => ApprovalDecision::Reject,
    result = rx => {
        match result {
            Ok(decision) => decision,
            Err(_) => {
                // Sender was dropped (TUI crashed or closed)
                tracing::warn!("Approval channel closed, rejecting by default");
                ApprovalDecision::Reject
            }
        }
    }
};
```

The oneshot channel is used instead of the signal bus for approvals because it provides direct request-response semantics without requiring a subscription mechanism. The signal bus is pub-sub (one-to-many), while approval is request-response (one-to-one). Using the signal bus for approvals would require the approval gate to subscribe, wait for the specific request, and publish a response — a much more complex pattern than a simple oneshot channel.

When the sender is dropped without sending a response (the `Err(_)` case), the runtime treats it as a rejection. This happens when the TUI crashes or when the session is terminated while an approval request is pending. Treating a dropped sender as rejection is the safe default — if we cannot reach the approver, we should not proceed with the potentially dangerous operation.

---

## Arc<Mutex> for Shared State

Shared mutable state in xaft is protected by `Arc<Mutex<T>>`, where `Mutex` is `tokio::sync::Mutex` (not `std::sync::Mutex`). The tokio mutex is chosen because it can be held across `.await` points, which is necessary for any mutex that protects async operations.

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct CostAccumulator {
    inner: Arc<Mutex<CostAccumulatorInner>>,
}

struct CostAccumulatorInner {
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cost_usd: f64,
    by_model: HashMap<String, ModelCost>,
}

impl CostAccumulator {
    pub async fn record(&self, model: &str, input: u64, output: u64, cost: f64) {
        let mut inner = self.inner.lock().await;
        inner.total_input_tokens += input;
        inner.total_output_tokens += output;
        inner.total_cost_usd += cost;
        inner.by_model
            .entry(model.to_string())
            .or_default()
            .record(input, output, cost);
    }

    pub async fn snapshot(&self) -> CostSnapshot {
        let inner = self.inner.lock().await;
        CostSnapshot {
            total_input_tokens: inner.total_input_tokens,
            total_output_tokens: inner.total_output_tokens,
            total_cost_usd: inner.total_cost_usd,
            by_model: inner.by_model.clone(),
        }
    }
}
```

### Lock Ordering Convention

To prevent deadlocks, xaft follows a strict lock ordering convention. Locks must always be acquired in the following order, and a lock must never be held while acquiring a lock that comes earlier in the order:

1. `SessionStore` locks (outermost — acquired first, released last)
2. `Agent` state locks
3. `CostAccumulator` locks
4. `StreamSink` locks (innermost — acquired last, released first)

This total ordering ensures that there are no cycles in the lock dependency graph, which eliminates the possibility of deadlocks. The convention is enforced by code review, not by the compiler, because Rust's type system cannot express lock ordering constraints.

### Lock Duration

Locks should be held for the minimum possible time. The pattern is: acquire the lock, read or mutate the state, release the lock, then perform any async operations (like I/O or LLM calls) without holding the lock. This prevents lock contention — other tasks can access the shared state while the current task is performing slow operations.

```rust
// BAD: Holding the lock across an await point
async fn bad_example(&self) {
    let mut state = self.state.lock().await;
    let data = state.compute_something();
    let result = self.provider.complete(data).await; // Lock held during LLM call!
    state.update(result);
    // Lock released here
}

// GOOD: Release the lock before the await
async fn good_example(&self) {
    let data = {
        let state = self.state.lock().await;
        state.compute_something()
    }; // Lock released here
    
    let result = self.provider.complete(data).await; // No lock held
    
    {
        let mut state = self.state.lock().await;
        state.update(result);
    } // Lock released here
}
```

---

## AtomicUsize for Lock-Free Counters

For simple counters that need to be updated from multiple tasks, xaft uses `std::sync::atomic::AtomicUsize` (or `AtomicU64`) instead of `Mutex<usize>`. Atomic operations are lock-free and do not cause contention, making them ideal for high-frequency counters like the iteration counter, the token counter, and the event counter.

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct AgentMetrics {
    iterations: AtomicUsize,
    tool_calls: AtomicUsize,
    errors: AtomicUsize,
    tokens_generated: AtomicU64,
}

impl AgentMetrics {
    pub fn increment_iterations(&self) {
        self.iterations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn iterations(&self) -> usize {
        self.iterations.load(Ordering::Relaxed)
    }
}
```

### Memory Ordering

xaft uses `Ordering::Relaxed` for all atomic operations on counters. Relaxed ordering provides no synchronization guarantees — it only ensures that the operation is atomic (no torn reads or writes). This is sufficient for counters because:

1. **Counter values are approximate.** A counter read might return a slightly stale value if another thread is concurrently incrementing it. This is acceptable because counters are used for monitoring and heuristics, not for correctness-critical decisions.

2. **No ordering dependencies.** The counter value does not control the order of other memory operations. For example, incrementing the iteration counter does not need to be visible to other threads before the agent's state update — these are independent operations.

3. **Performance.** Relaxed ordering is the cheapest memory ordering on all architectures. On x86, it compiles to a plain `inc` instruction with no memory fence. On ARM, it compiles to an `ldaxr/stlxr` loop without barriers. This matters for counters that are updated on every iteration of the agent loop (potentially thousands of times per second).

If an atomic is used for synchronization (not just counting), `Ordering::Acquire`/`Release` or `Ordering::SeqCst` should be used instead. xaft currently has no such use case — all synchronization is done through `Mutex` or channels.

---

## Summary

| Pattern | Use Case | Key Property |
|---------|----------|-------------|
| `tokio::spawn` | Fire-and-forget tasks | Independent lifetime from spawner |
| `CancellationToken` | Cooperative cancellation | Hierarchical propagation, cleanup before exit |
| `select! { biased }` | Racing async operations | Cancellation always wins, deterministic priority |
| `oneshot` channels | Request-response (approval) | Direct backchannel, drop = rejection |
| `Arc<Mutex<T>>` | Shared mutable state | Hold briefly, release before await |
| `AtomicUsize` | Lock-free counters | Relaxed ordering, approximate values |

These conventions work together to create a consistent, correct, and performant async codebase. The cancellation token ensures that every operation can be stopped cleanly. The biased select ensures that cancellation is always responsive. The lock ordering prevents deadlocks. And the atomic counters provide efficient metrics without contention.
