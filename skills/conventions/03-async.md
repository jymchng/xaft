# Async Conventions

## Purpose

The xaft runtime is fundamentally asynchronous: it manages concurrent LLM streaming, tool executions, user approvals, and TUI rendering—all within a single tokio runtime. Without strict async conventions, you get deadlocks (two tasks waiting on each other's `Mutex`), leaked tasks (fire-and-forget spawns that never complete), and unresponsive cancellations (a `CancellationToken` that nobody checks). This document specifies the async primitives, patterns, and rules that every contributor must follow so that the runtime remains responsive, cancellable, and deadlock-free under load.

## Mental Model

Think of the async runtime as a city's traffic system. `tokio::spawn` is a one-way street—once you send a task down it, you don't wait for it to return (fire-and-forget for signal emissions, background bookkeeping). `CancellationToken` is the emergency brake—it propagates from the user's Ctrl+C through every level of the call stack. `tokio::select!{biased}` is the intersection with a priority lane: cancellation always wins, then the primary work, then secondary concerns. Channels are the delivery trucks: `mpsc::unbounded_channel` for continuous event streams (many messages, no backpressure needed), `oneshot` for single-response exchanges (approval yes/no). Shared state uses the lightest synchronization possible: `AtomicUsize` for counters, `Arc<Mutex>` for complex mutable state, never `Mutex<Blocking>` inside an async context.

## Extension Patterns

When adding a new background task (e.g., periodic session cleanup), use `tokio::spawn` with a cloned `CancellationToken`. The task's main loop should be `tokio::select! { _ = cancel.cancelled() => break, _ = sleep(duration) => do_work() }`. When adding a new event stream (e.g., tool progress updates), create an `mpsc::unbounded_channel` and pass the `Sender` to the producer and the `Receiver` to the consumer. When adding a new request-response interaction (e.g., a confirmation dialog), use a `oneshot` channel: the requester creates `(tx, rx)`, sends `tx` to the responder, and awaits `rx`. When adding shared mutable state (e.g., a cost accumulator), wrap it in `Arc<Mutex<T>>` and always hold the lock for the minimum time—never across an `.await` point. For simple counters (e.g., request IDs), use `AtomicUsize` with `fetch_add` to avoid locks entirely.

## Common Pitfalls

- **Holding a `Mutex` across an `.await`**: This is the #1 source of deadlocks in async Rust. If you must mutate state and then await, drop the lock before the `.await` by scoping it with a block: `{ let mut guard = state.lock().await; guard.update(); } some_async_work().await;`.
- **Using `mpsc::channel` (bounded) for event streams**: Bounded channels introduce backpressure that can deadlock the event loop if the consumer is slow. Use `mpsc::unbounded_channel` for event streams where messages are small and loss is worse than memory growth.
- **Forgetting `biased` in `select!`**: Without `biased`, tokio randomly chooses between ready branches. This means cancellation might lose to a new message, causing the task to process one more item after it should have stopped. Always use `tokio::select!{biased;` with cancellation first.
- **Spawning without `CancellationToken`**: A spawned task without a cancellation mechanism will run forever, leaking resources. Every `tokio::spawn` in the runtime must receive a cloned `CancellationToken` and check it in its main loop.
- **Using `std::sync::Mutex` in async code**: The standard library `Mutex` can block the tokio thread if held across an `.await`. Always use `tokio::sync::Mutex` for async contexts, or `std::sync::Mutex` only for purely synchronous, short-lived locks (e.g., a quick counter update with no awaits).

## Invariants

1. Every `tokio::spawn` must receive a cloned `CancellationToken` and must exit when `cancel.cancelled()` completes.
2. Every `tokio::select!` in the runtime must use `biased` and must check cancellation as the first branch.
3. `Arc<Mutex<T>>` locks must never be held across an `.await` point. Scope the lock to a synchronous block.
4. `AtomicUsize` (or other atomics) must be used for lock-free counters. Never use `Mutex<u64>` for a simple counter.
5. `mpsc::unbounded_channel` is the default for event streams. Use bounded channels only when backpressure is explicitly desired and the consumer can handle it.
6. `oneshot` channels must be used for single-response interactions (approval gates, configuration queries). Never reuse a oneshot channel.
7. Fire-and-forget spawns (signal emissions, background bookkeeping) must log errors internally—they must not propagate errors to the caller.

## Examples

```rust
// Fire-and-forget signal emission
tokio::spawn({
    let bus = signal_bus.clone();
    let cancel = cancel_token.clone();
    async move {
        tokio::select! {
            _ = cancel.cancelled() => {},
            _ = async {
                if let Err(e) = bus.emit(XaftToolCallStarted { tool: name }).await {
                    tracing::warn!("signal emission failed: {e}");
                }
            } => {},
        }
    }
});

// CancellationToken with biased select
async fn run_tool_loop(
    mut receiver: mpsc::UnboundedReceiver<ToolRequest>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                tracing::info!("tool loop cancelled");
                break;
            }
            Some(req) = receiver.recv() => {
                execute_tool(req).await;
            }
        }
    }
}

// Oneshot for approval
let (tx, rx) = oneshot::channel();
approval_gate.request(ApprovalRequest { tool: "bash_exec".into(), response: tx }).await;
match rx.await {
    Ok(true) => proceed(),
    Ok(false) | Err(_) => abort(),
}

// Arc<Mutex> for shared state (lock NOT held across await)
async fn accumulate_cost(state: Arc<Mutex<CostTracker>>, tokens: u64) {
    {
        let mut guard = state.lock().await;
        guard.add_tokens(tokens);
    } // lock dropped here
    some_async_work().await; // safe: no lock held
}

// AtomicUsize for lock-free counter
static REQUEST_COUNTER: AtomicUsize = AtomicUsize::new(0);
let id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
```
