# Channel Selection Guide

## Purpose

The xaft runtime uses several different channel types, and choosing the wrong one leads to deadlocks, lost messages, or unnecessary complexity. An unbounded channel where a bounded one is needed causes memory growth; a bounded channel where an unbounded one is needed causes backpressure deadlocks; a broadcast channel where a oneshot would suffice adds unnecessary overhead. This guide specifies which channel type to use for each communication pattern in the runtime, so that every inter-task message path is correct by construction.

## Mental Model

Think of channels as different shapes of plumbing. `mpsc::unbounded` is a firehose—water flows as fast as the source can push, and the bucket at the end just gets bigger. Use it when messages are small, loss is unacceptable, and the consumer will eventually catch up (event streams, TUI events). `mpsc::bounded` is a pipe with a pressure valve—if the bucket is full, the source waits. Use it when you want to apply backpressure (rate-limiting LLM requests). `oneshot` is a courier—a single package delivered once, then the channel is done. Use it for request-response patterns (approval gates). `broadcast` is a PA system—one message goes to every listener. Use it for type-safe event distribution (SignalBus). `watch` is a thermostat—it holds the latest reading and notifies on change. Use it for config hot-reload where consumers only care about the current value. `RwLock` is a shared whiteboard—multiple readers can look, one writer can update. Use it for shared state that is read frequently and written rarely (session store). `Mutex` is a shared ledger—one person at a time, used for cross-task accumulation (cost tracker).

## Extension Patterns

When adding a new event stream (e.g., tool progress updates), use `mpsc::unbounded_channel`. The producer (tool executor) sends events, and the consumer (TUI renderer) receives them. The TUI will always consume events faster than tools produce them, so backpressure is not needed. When adding a new request-response interaction (e.g., a confirmation dialog), use a `oneshot` channel. The requester creates `(tx, rx)`, sends `tx` to the responder, and awaits `rx.await`. When adding a new system-wide event (e.g., a config change notification), use the `SignalBus` broadcast mechanism. Producers emit signals, and subscribers receive them with type-safe downcasting. When adding a shared data structure that is read often and written rarely (e.g., the active session list), use `Arc<RwLock<T>>` so readers don't block each other. When adding a cross-task accumulator (e.g., cost tracking across multiple concurrent agent runs), use `Arc<Mutex<T>>` for exclusive access during updates.

## Common Pitfalls

- **Using `mpsc::bounded` for event streams**: The TUI renderer processes events in a batch. If the bounded channel fills up while the TUI is rendering a frame, the tool executor blocks, which blocks the agent loop, which blocks the entire session. Use `mpsc::unbounded` for event streams.
- **Reusing a `oneshot` channel**: A oneshot channel delivers exactly one message. If you try to send twice, the second send fails silently or panics. Create a new oneshot for each request.
- **Using `broadcast` for point-to-point communication**: Broadcast sends to all subscribers. If only one consumer needs the message, you're wasting CPU on unnecessary receives and risking confusion about who handled the message. Use `mpsc` for point-to-point.
- **Holding an `RwLock` write guard across an `.await`**: This blocks all readers for the duration of the async operation. Always drop the guard before awaiting.
- **Using `watch` for event streams**: `watch` only retains the latest value. If three events arrive before the consumer reads, the consumer sees only the last one. Use `mpsc` for event streams where every message matters.
- **Forgetting to drain channels on shutdown**: If a producer drops its sender, the consumer's `recv()` returns `None`. If the consumer doesn't handle `None`, it may spin in a loop. Always handle channel closure gracefully in shutdown paths.

## Invariants

1. `mpsc::unbounded_channel` is the default for event streams (TUI events, task requests). Use bounded only when backpressure is explicitly desired and the consumer handles it.
2. `oneshot` channels must be used for single-response interactions (approval gates, configuration queries). Never reuse a oneshot.
3. `SignalBus` (broadcast) must be used for type-safe event distribution. Subscribers must handle `Lagged` errors gracefully.
4. `watch` channels must be used only for "latest value" semantics (config hot-reload). Never use `watch` for event streams where every message must be delivered.
5. `Arc<RwLock<T>>` must be used for shared state that is read frequently and written rarely (session store). Readers must not block each other.
6. `Arc<Mutex<T>>` must be used for cross-task accumulation where exclusive access is needed during updates (cost tracker). The lock must not be held across `.await` points.
7. Every channel consumer must handle closure (sender dropped) gracefully to prevent spin loops on shutdown.

## Examples

```rust
// Unbounded channel for event streams
let (event_tx, event_rx) = mpsc::unbounded_channel::<XaftEvent>();

// Tool executor sends events
event_tx.send(XaftEvent::ToolCallStarted { tool: "read_file".into() })?;

// TUI renderer receives events
tokio::select! { biased;
    _ = cancel.cancelled() => break,
    Some(event) = event_rx.recv() => {
        tui.handle_event(event);
    }
}

// Oneshot for approval gates
let (approval_tx, approval_rx) = oneshot::channel();
approval_gate.request(ApprovalRequest {
    operation: "bash_exec".into(),
    detail: "rm -rf /tmp/test".into(),
    response: approval_tx,
}).await;

match approval_rx.await {
    Ok(true) => execute_bash_command(command).await,
    Ok(false) => ToolResult::soft_error("User denied approval"),
    Err(_) => ToolResult::soft_error("Approval cancelled"),
}

// SignalBus for type-safe broadcast
pub struct SignalBus {
    sender: broadcast::Sender<Arc<dyn Any + Send + Sync>>,
}

impl SignalBus {
    pub fn emit<T: 'static + Send + Sync>(&self, signal: T) -> Result<(), broadcast::error::SendError<...>> {
        self.sender.send(Arc::new(signal))?;
        Ok(())
    }

    pub fn subscribe<T: 'static + Clone + Send + Sync>(&self) -> SignalStream<T> {
        let receiver = self.sender.subscribe();
        SignalStream { receiver, _phantom: PhantomData }
    }
}

// Watch for config hot-reload
let (config_tx, config_rx) = watch::channel(initial_config);

// Config updater pushes new values
config_tx.send(new_config)?;

// Consumer reads latest value on change
if config_rx.changed().await.is_ok() {
    let latest = config_rx.borrow().clone();
    apply_config(latest);
}

// RwLock for session store (read-heavy)
pub struct SessionStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl SessionStore {
    pub async fn get(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.get(id).cloned()
    }
    pub async fn insert(&self, session: Session) {
        self.sessions.write().await.insert(session.id().into(), session);
    }
}

// Mutex for cost tracker (cross-task accumulation)
pub struct CostTracker {
    total: Arc<Mutex<f64>>,
}

impl CostTracker {
    pub async fn add(&self, cost: f64) {
        let mut guard = self.total.lock().await;
        *guard += cost;
    } // lock dropped here, before any .await
}
```
