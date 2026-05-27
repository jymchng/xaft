# Streaming Pipeline

## Purpose

The streaming pipeline is how xaft delivers real-time progress updates from the LLM and tool execution to consumers like the TUI, API clients, and test harnesses. Instead of waiting for an entire agent run to complete, the pipeline emits events as they happen—token by token from the LLM, tool call by tool call from the executor. This document explains the pipeline's architecture: how `StreamEvent` types flow from the LLM provider through `AgentExecutor::run_stream()` to sinks, how `EventLoop::consume()` drains the stream, and how different sink implementations serve different use cases.

Understanding the streaming pipeline is essential for anyone building custom consumers of agent output, debugging response latency, or implementing real-time UI features like token-by-token text display or live tool progress indicators.

## Mental Model

Think of the streaming pipeline as a **water pipeline from source to tap**. The LLM provider is the reservoir, producing a stream of `StreamEvent` drops. The `AgentExecutor::run_stream()` is the treatment plant, enriching raw LLM events with tool results and agent state. The `XaftAgent` is the pumping station, forwarding events through a `stream_sink`. The `EventLoop::consume()` is the valve that controls flow rate. And sinks are the taps—each one serves a different purpose.

```
LLM Provider
     │
     │ StreamEvent: TextDelta, ToolCall, Done, Error
     ▼
AgentExecutor::run_stream()
     │
     │ Enriches with tool results, agent metadata
     ▼
XaftAgent
     │
     │ Forwards ToolResult and Done via stream_sink
     ▼
EventLoop::consume()
     │
     │ tokio::select! drains with backpressure
     ▼
ChannelSink ──────▶ TUI / API Client
CollectSink ──────▶ Test assertions
NopSink ──────────▶ /dev/null (benchmarking)
```

## StreamEvent Types

The fundamental unit of the pipeline is `StreamEvent`, an enum representing each meaningful event in the agent's execution:

```rust
pub enum StreamEvent {
    /// A chunk of text from the LLM's response
    TextDelta { content: String },

    /// The LLM is requesting a tool call
    ToolCallRequested {
        tool_name: String,
        call_id: String,
        input: Value,
    },

    /// A tool has started executing
    ToolCallStarted {
        tool_name: String,
        call_id: String,
    },

    /// A tool has finished executing
    ToolCallComplete {
        tool_name: String,
        call_id: String,
        result: ToolResult,
        duration_ms: u64,
    },

    /// A tool is waiting for user approval
    ToolPendingApproval {
        tool_name: String,
        call_id: String,
        input_summary: String,
    },

    /// The agent's turn is complete
    TurnComplete {
        turn: usize,
        reason: TurnEndReason,
    },

    /// The agent run is complete
    Done {
        outcome: AgentRunOutcome,
    },

    /// An error occurred during streaming
    Error {
        message: String,
        recoverable: bool,
    },
}
```

## Extension Patterns

### AgentExecutor::run_stream()

The `AgentExecutor` provides a streaming interface that returns a `Stream<Item = StreamEvent>` instead of a single `AgentRunOutcome`:

```rust
impl AgentExecutor {
    pub fn run_stream(
        &self,
        agent_def: &AgentDefinition,
        ctx: AgentContext,
    ) -> impl Stream<Item = StreamEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        tokio::spawn(async move {
            let mut turn = 0;
            while turn < agent_def.max_turns {
                // Make LLM call
                let mut llm_stream = self.llm_client.chat_stream(
                    agent_def.system_prompt_fn(&ctx.workspace_context),
                    &ctx.message_store.get_messages(&ctx.conversation_key).await,
                ).await;

                while let Some(event) = llm_stream.next().await {
                    match event {
                        LlmStreamEvent::TextDelta(content) => {
                            let _ = tx.send(StreamEvent::TextDelta { content }).await;
                        }
                        LlmStreamEvent::ToolCall { name, call_id, input } => {
                            let _ = tx.send(StreamEvent::ToolCallRequested {
                                tool_name: name,
                                call_id,
                                input,
                            }).await;
                        }
                        LlmStreamEvent::Done => break,
                        LlmStreamEvent::Error(e) => {
                            let _ = tx.send(StreamEvent::Error {
                                message: e.to_string(),
                                recoverable: true,
                            }).await;
                            break;
                        }
                    }
                }

                // Process tool calls from the LLM response
                // ... (tool execution, sending ToolCallStarted/Complete events)

                turn += 1;
                let _ = tx.send(StreamEvent::TurnComplete {
                    turn,
                    reason: TurnEndReason::ToolCallsCompleted,
                }).await;
            }

            let _ = tx.send(StreamEvent::Done {
                outcome: AgentRunOutcome::Completed {
                    final_agent: agent_def.name.clone(),
                    summary: String::new(),
                },
            }).await;
        });

        tokio_stream::wrappers::ReceiverStream::new(rx)
    }
}
```

### XaftAgent Forwards to stream_sink

The `XaftAgent` wraps the executor stream and forwards events to a `stream_sink` that consumers can plug into:

```rust
impl XaftAgent {
    pub async fn run_with_sink(
        &self,
        sink: &mut dyn StreamSink,
    ) -> Result<AgentRunOutcome> {
        let mut stream = self.executor.run_stream(&self.agent_def, self.ctx.clone());

        while let Some(event) = stream.next().await {
            match &event {
                StreamEvent::ToolCallComplete { result, .. } => {
                    sink.on_tool_result(result).await;
                }
                StreamEvent::Done { outcome } => {
                    sink.on_done(outcome).await;
                    return Ok(outcome.clone());
                }
                _ => {}
            }
            sink.on_event(&event).await;
        }

        // Stream ended without Done event — treat as error
        sink.on_done(&AgentRunOutcome::Error("stream ended unexpectedly".into())).await;
        Err(anyhow!("stream ended without Done event"))
    }
}
```

### EventLoop::consume() with tokio::select!

The `EventLoop` drains the stream using `tokio::select!`, which allows concurrent handling of stream events and other async tasks (like cancellation signals or user input):

```rust
impl EventLoop {
    pub async fn consume(
        &self,
        stream: impl Stream<Item = StreamEvent> + Unpin,
        sink: &mut dyn StreamSink,
    ) -> Result<()> {
        let mut stream = Box::pin(stream);

        loop {
            tokio::select! {
                // Process stream events
                event = stream.next() => {
                    match event {
                        Some(event) => {
                            sink.on_event(&event).await;
                            if matches!(event, StreamEvent::Done { .. }) {
                                return Ok(());
                            }
                        }
                        None => return Ok(()),
                    }
                }

                // Handle cancellation
                _ = self.cancellation_token.cancelled() => {
                    sink.on_event(&StreamEvent::Done {
                        outcome: AgentRunOutcome::Cancelled,
                    }).await;
                    return Ok(());
                }

                // Handle user input (for approval gates)
                approval = self.approval_rx.recv() => {
                    if let Ok(approval) = approval {
                        self.process_approval(approval).await;
                    }
                }
            }
        }
    }
}
```

### Sink Implementations

#### ChannelSink: Bridge to TUI / API

`ChannelSink` forwards events through an `mpsc` channel to a consumer running on another task or thread:

```rust
pub struct ChannelSink {
    tx: mpsc::Sender<StreamEvent>,
}

impl StreamSink for ChannelSink {
    async fn on_event(&mut self, event: &StreamEvent) {
        // Clone the event and send; drop if channel is full (backpressure)
        let _ = self.tx.try_send(event.clone());
    }

    async fn on_tool_result(&mut self, result: &ToolResult) {
        // Tool results are handled via on_event; no special processing needed
    }

    async fn on_done(&mut self, outcome: &AgentRunOutcome) {
        // Signal completion via the channel
        let _ = self.tx.send(StreamEvent::Done { outcome: outcome.clone() }).await;
    }
}
```

#### CollectSink: For Testing

`CollectSink` accumulates all events in a `Vec` for assertion-based testing:

```rust
pub struct CollectSink {
    events: Vec<StreamEvent>,
}

impl StreamSink for CollectSink {
    async fn on_event(&mut self, event: &StreamEvent) {
        self.events.push(event.clone());
    }

    async fn on_done(&mut self, outcome: &AgentRunOutcome) {
        self.events.push(StreamEvent::Done { outcome: outcome.clone() });
    }
}

impl CollectSink {
    pub fn assert_text_contains(&self, substring: &str) {
        let full_text: String = self.events.iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert!(full_text.contains(substring),
            "Expected text to contain '{}', got: {}", substring, full_text);
    }

    pub fn tool_calls(&self) -> Vec<&str> {
        self.events.iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallRequested { tool_name, .. } => Some(tool_name.as_str()),
                _ => None,
            })
            .collect()
    }
}
```

#### NopSink: Discard Everything

`NopSink` is the `/dev/null` of sinks. It discards all events, useful for benchmarking the agent runtime without the overhead of event processing:

```rust
pub struct NopSink;

impl StreamSink for NopSink {
    async fn on_event(&mut self, _event: &StreamEvent) {}
    async fn on_tool_result(&mut self, _result: &ToolResult) {}
    async fn on_done(&mut self, _outcome: &AgentRunOutcome) {}
}
```

## Common Pitfalls

1. **Blocking the stream with a slow sink.** If `on_event` takes too long (e.g., rendering a complex widget), backpressure builds up in the channel and the agent runtime stalls. Keep `on_event` fast and delegate heavy work to a separate task.

2. **Not handling `StreamEvent::Error`.** Errors in the stream are events, not panics. A consumer that only matches `TextDelta` and `Done` will silently skip errors. Always handle the `Error` variant.

3. **Assuming events arrive in a specific order within a turn.** `TextDelta` events may interleave with `ToolCallRequested` events in a single LLM response. Don't assume all text comes before all tool calls.

4. **Forgetting to check for `Done` in the EventLoop.** If the EventLoop doesn't exit on `Done`, it will hang forever waiting for more events from a stream that has already ended.

5. **ChannelSink dropping events under load.** `try_send` drops events when the channel is full. If this is unacceptable for your use case, use `send.await` instead (which applies backpressure) or increase the channel capacity.

6. **Not cloning events when forwarding.** `StreamEvent` implements `Clone`, but if you try to move an event into a sink and also forward it to another consumer, you'll get a borrow error. Always clone.

## Invariants

- **Every stream ends with a `Done` or `Error` event.** A stream that simply stops producing events is a bug.
- **`StreamEvent` implements `Clone`.** All events can be duplicated for multiple consumers.
- **Sinks never block the agent runtime.** `on_event` must return quickly. Heavy processing should be offloaded.
- **`EventLoop::consume()` always checks the cancellation token.** A cancelled run always produces a `Done { outcome: Cancelled }` event.
- **Channel capacity is bounded.** Unbounded channels can cause memory leaks if the producer outpaces the consumer. Default capacity is 128 events.
- **`CollectSink` is for testing only.** It accumulates unbounded events in memory. Never use it in production.

## Examples

### Custom Sink for WebSocket Forwarding

```rust
pub struct WebSocketSink {
    ws_tx: tokio_tungstenite::tungstenite::Message,
}

impl StreamSink for WebSocketSink {
    async fn on_event(&mut self, event: &StreamEvent) {
        let json = serde_json::to_string(event).unwrap();
        let _ = self.ws_tx.send(Message::Text(json)).await;
    }

    async fn on_tool_result(&mut self, result: &ToolResult) {
        // Tool results are forwarded via on_event
    }

    async fn on_done(&mut self, outcome: &AgentRunOutcome) {
        let json = serde_json::to_string(&StreamEvent::Done { outcome: outcome.clone() }).unwrap();
        let _ = self.ws_tx.send(Message::Text(json)).await;
    }
}
```

### Testing with CollectSink

```rust
#[tokio::test]
async fn test_agent_reads_file() {
    let workspace = InMemoryWorkspaceStore::new();
    workspace.write_file("hello.txt", "world").await;

    let agent = XaftAgent::new(/* ... */);
    let mut sink = CollectSink::new();

    agent.run_with_sink(&mut sink).await.unwrap();

    // Assert the agent read the file
    assert!(sink.tool_calls().contains(&"read_file"));
    sink.assert_text_contains("world");
}
```
