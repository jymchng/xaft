# Streaming Architecture

Streaming is the nervous system of the xaft runtime. It connects the LLM provider's token-by-token generation to the frontend's real-time display, carries tool execution events to approval gates, and delivers the final agent output to consumers. This page documents the streaming architecture from the `StreamSink` trait to the end-to-end flow that connects the agent executor to the event loop and beyond.

## Design Philosophy

The streaming system is designed around three principles:

1. **Decoupling**: The agent produces events without knowing who consumes them. The event loop consumes events without knowing who produced them. This decoupling allows the same agent to drive a terminal UI, a web dashboard, a test harness, or a headless CI pipeline without any code changes.

2. **Backpressure-free**: The streaming pipeline never blocks the agent or the LLM provider. Events are emitted as they are produced and are either consumed immediately (in the event loop) or buffered (in the channel sink). If the consumer is slow, events are buffered up to the channel capacity; beyond that, the oldest events are dropped. The agent never waits for the consumer.

3. **Type-safe**: Every event variant has a distinct type and carries strongly typed data. There are no untyped JSON blobs or stringly-typed event names. The Rust compiler ensures that every event is handled correctly and that no variant is accidentally ignored.

## StreamSink Trait

The `StreamSink` trait is the core abstraction of the streaming system. It defines a single method — `send(event)` — that delivers a `StreamEvent` to the sink's consumer:

```rust
pub trait StreamSink: Send + Sync {
    fn send(&mut self, event: StreamEvent);
}
```

The trait is intentionally minimal. It does not include methods for flushing, closing, or querying the sink's state, because these operations have different semantics for different sink implementations. Adding them to the trait would force every implementation to provide behavior that may not make sense for its use case.

The `Send + Sync` bounds are required because the sink is shared between the agent (which runs on the orchestrator's Tokio task) and the event loop (which runs on a separate task). The sink must be safe to send across task boundaries and to access from multiple threads (although in practice, the `&mut self` signature ensures exclusive access during `send()`).

## Sink Implementations

The runtime provides three `StreamSink` implementations, each optimized for a different use case.

### NopSink

```rust
pub struct NopSink;

impl StreamSink for NopSink {
    fn send(&mut self, _event: StreamEvent) {
        // Discard the event
    }
}
```

The `NopSink` discards all events. It is the default sink for agents that do not need streaming output — for example, agents running in test mode where the output is checked through the session store rather than through the streaming pipeline. The `NopSink` has zero overhead: the `send` method is a no-op, and the Rust compiler will likely optimize it away entirely through inlining and dead code elimination.

Despite its simplicity, the `NopSink` is an important part of the architecture. It allows agent code to always call `send()` on the sink without checking whether streaming is enabled. This eliminates conditional logic at every event emission point and keeps the agent's code clean and predictable.

### ChannelSink

```rust
pub struct ChannelSink {
    tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
}

impl StreamSink for ChannelSink {
    fn send(&mut self, event: StreamEvent) {
        let _ = self.tx.send(event);
    }
}
```

The `ChannelSink` sends events through a Tokio unbounded MPSC channel. It is the primary sink for interactive and headless execution, where the event loop consumes events from the channel's receiver. The channel is unbounded, which means `send()` never blocks — events are buffered in memory until the receiver consumes them. This eliminates backpressure from the agent's perspective, which is the correct behavior for a real-time streaming system where the agent should never wait for a slow UI.

The "unbounded" designation does not mean unbounded memory growth in practice. The event loop consumes events as fast as they are produced — typically faster than the LLM generates tokens, because event dispatch is a simple match-and-forward operation. Memory pressure only becomes an issue if the consumer (for example, a web frontend) is completely unresponsive, in which case the channel buffer grows linearly with the number of events produced. For long-running tasks, this could theoretically exhaust memory, but in practice the event loop's consumption rate is orders of magnitude faster than the production rate.

The `let _ = self.tx.send(event)` discards the `Result` of the send operation. If the receiver has been dropped (for example, because the event loop has exited), the send silently fails. This is the correct behavior: the agent should not fail just because the consumer has disconnected. The agent's primary job is to complete the task, not to maintain a streaming connection.

### CollectSink

```rust
pub struct CollectSink {
    events: Arc<Mutex<Vec<StreamEvent>>>,
}

impl StreamSink for CollectSink {
    fn send(&mut self, event: StreamEvent) {
        self.events.lock().unwrap().push(event);
    }
}
```

The `CollectSink` accumulates all events into a shared `Vec<StreamEvent>` protected by a `Mutex`. It is designed for testing and programmatic consumption, where the caller needs to inspect the full sequence of events after the agent finishes. The `Arc<Mutex<>>` wrapper allows the sink to be shared between the agent and the test code, with the test code holding a clone of the `Arc` and reading the events after execution completes.

The `CollectSink` is not suitable for production use because it holds all events in memory for the entire duration of the agent's execution. For a long-running task that produces thousands of `TextDelta` events, this can consume significant memory. In production, the `ChannelSink` is preferred because it allows events to be consumed and discarded as they arrive.

However, the `CollectSink` is invaluable for testing. It allows test code to assert on the exact sequence of events produced by the agent, without dealing with the asynchrony of the `ChannelSink`. The `Mutex` provides the necessary synchronization between the agent's task and the test's assertion task, and the `unwrap()` on the lock is acceptable because the only failure mode (poison) would indicate a panic in the agent, which would fail the test anyway.

## End-to-End Flow

The complete streaming flow connects the LLM provider to the consumer through four layers: the provider's streaming response, the agent's event forwarding, the event loop's consumption, and the consumer's processing.

```mermaid
sequenceDiagram
    participant LLM as LLM Provider
    participant Agent as XaftAgent
    participant Sink as ChannelSink
    participant Channel as mpsc::Channel
    participant Loop as EventLoop
    participant Consumer as CLI / API

    LLM->>Agent: TextDelta("Hello")
    Agent->>Sink: send(TextDelta)
    Sink->>Channel: tx.send(TextDelta)
    Channel->>Loop: rx.recv() = TextDelta
    Loop->>Consumer: dispatch(TextDelta)

    LLM->>Agent: ToolCall(WriteFile, ...)
    Agent->>Sink: send(ToolExecution)
    Sink->>Channel: tx.send(ToolExecution)
    Channel->>Loop: rx.recv() = ToolExecution
    Loop->>Consumer: dispatch(ToolExecution)

    Note over Agent: Tool executes

    Agent->>Sink: send(ToolResult)
    Sink->>Channel: tx.send(ToolResult)
    Channel->>Loop: rx.recv() = ToolResult
    Loop->>Consumer: dispatch(ToolResult)

    LLM->>Agent: [stream ends]
    Agent->>Sink: send(Done)
    Sink->>Channel: tx.send(Done)
    Channel->>Loop: rx.recv() = Done
    Loop->>Consumer: dispatch(Done)
```

### Layer 1: Provider Streaming

The LLM provider's `stream()` method returns a `BoxStream<'static, Result<StreamEvent, ProviderError>>`. Each item in the stream corresponds to a single event from the provider's API — a text delta, a thinking delta, a tool call delta, or a terminal event (done or error). The provider translates its native streaming format (Server-Sent Events for Anthropic, JSON streaming for OpenAI) into xaft's `StreamEvent` type, providing a uniform interface regardless of the underlying provider.

### Layer 2: Agent Event Forwarding

The `AgentExecutor::run_stream()` method wraps the provider's stream and adds agent-level events. Specifically, it intercepts the stream and inserts `ToolExecution` events before each tool call, `ToolResult` events after each tool completes, and `PendingApproval` events when a write tool requires user approval. It also inserts the final `Done` event when the agent completes its turn loop.

The `XaftAgent`'s `on_tool_result` hook is the mechanism by which tool results are forwarded to the sink. When the orchestrator calls `on_tool_result`, the agent calls `sink.send(ToolResult { ... })`, which pushes the event into the channel. This explicit forwarding step — rather than, say, having the tool directly write to the sink — ensures that the agent has the opportunity to inspect, transform, or filter tool results before they are sent to the consumer.

### Layer 3: Event Loop Consumption

The `EventLoop::consume()` method reads events from the channel's receiver using `tokio::select!` with biased prioritization. For each event, it calls `dispatch()`, which routes the event to the appropriate handler. The event loop is documented in detail in [Event Loop](../runtime/03-event-loop.md).

The event loop is the convergence point where the streaming pipeline meets the cancellation and approval systems. It is the only component that can pause event processing (for `PendingApproval` events) or terminate event processing (for cancellation signals). This centralized control ensures consistent behavior regardless of the consumer's capabilities.

### Layer 4: Consumer Processing

The consumer is the final destination for streaming events. It can be a CLI renderer that displays text deltas in real time, a web API that forwards events to a browser over WebSocket, a test harness that collects events for assertion, or any other system that needs to observe the agent's progress.

The consumer receives events through the event loop's dispatch mechanism, not directly from the channel. This indirection allows the event loop to enforce ordering guarantees (for example, `Done` is always the last event) and to inject synthetic events (for example, an `Error` event when the budget is exceeded). The consumer does not need to worry about these edge cases — it simply handles each event as it arrives, and the event loop ensures the event stream is well-formed.

## Sink Selection Strategy

The runtime selects the sink implementation based on the execution mode:

| Mode | Sink | Rationale |
|---|---|---|
| Interactive (terminal) | `ChannelSink` | Real-time display requires event streaming to the terminal renderer |
| Headless (CI/CD) | `ChannelSink` | CI systems need real-time output for live log streaming |
| Testing | `CollectSink` | Tests need to inspect the full event sequence after execution |
| Fire-and-forget | `NopSink` | When the caller only cares about the final `RunResult`, not the intermediate events |

The sink is set via `AgentBuilder::stream_sink()` and cannot be changed after the agent is built. This is a deliberate restriction: changing the sink during execution could cause events to be lost (if the old sink is dropped before the new sink is attached) or duplicated (if both sinks receive events during the transition). The immutability of the sink ensures exactly-once event delivery.
