# 05 — Streaming Engine

> Real-time output, backpressure, token rendering, tool execution streaming,
> SSE bridging, and how streaming interacts with approval gates.

---

## Overview

Streaming is xaft's most user-facing technical challenge. The entire value proposition of an autonomous coding agent hinges on the user being able to **see what's happening in real time** — the agent's reasoning, its tool calls, the results, and the cost accumulating. If the user has to wait for a batch response, trust erodes and intervention becomes impossible.

The streaming engine is built on three layers:

1. **Producer layer**: `AgentExecutor::run_stream` produces `StreamEvent`s from the ReAct loop
2. **Transport layer**: `StreamEvent`s flow through bounded channels with backpressure
3. **Consumer layer**: TUI, SSE, and headless JSON sinks consume events and render output

---

## StreamEvent Enum

The `StreamEvent` is the fundamental unit of streaming data. Every notable occurrence in the agent loop produces one:

```rust
/// Events produced by AgentExecutor::run_stream.
/// Each variant represents a discrete, renderable piece of information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    // ── LLM Events ────────────────────────────────────────────

    /// A single token from the LLM response.
    /// This is the highest-frequency event (potentially 100+ per second).
    LlmToken {
        token: String,
        /// Which model produced this token.
        model: String,
        /// Running total of output tokens so far this turn.
        output_tokens_so_far: u32,
    },

    /// The LLM is "thinking" (extended thinking mode for models that support it).
    LlmThinking {
        content: String,
    },

    /// The LLM response is complete (all tokens received).
    LlmResponseComplete {
        /// Full text of the response (for consumers that don't track tokens).
        full_text: String,
        /// Total tokens used.
        usage: TokenUsage,
        /// Cost of this LLM call in USD.
        cost_usd: f64,
        /// Tool calls in the response (if any).
        tool_calls: Vec<ToolCallInfo>,
    },

    // ── Tool Events ───────────────────────────────────────────

    /// A tool call has been parsed from the LLM response.
    ToolCall {
        /// Name of the tool being called.
        tool_name: String,
        /// Arguments as JSON string.
        args: String,
        /// Unique identifier for this tool call.
        call_id: CallId,
    },

    /// A tool has started executing.
    ToolExecuting {
        call_id: CallId,
        tool_name: String,
    },

    /// Intermediate progress from a tool (e.g., partial shell output).
    ToolProgress {
        call_id: CallId,
        /// Progress update (stdout line, partial result, etc.)
        update: ToolProgressUpdate,
    },

    /// A tool has finished executing.
    ToolResult {
        call_id: CallId,
        result: ToolOutput,
    },

    /// A tool call was rejected by the approval gate.
    ToolRejected {
        call_id: CallId,
        reason: String,
    },

    // ── Turn Events ───────────────────────────────────────────

    /// A turn has completed.
    TurnComplete {
        turn: u32,
        /// Cost accumulated this turn.
        turn_cost_usd: f64,
        /// Cumulative cost for the session.
        cumulative_cost_usd: f64,
    },

    // ── Agent Events ──────────────────────────────────────────

    /// The agent has finished its work.
    AgentComplete {
        result: AgentOutcome,
    },

    /// The agent encountered an error.
    Error {
        error: String,
        /// Whether this error is recoverable (agent will retry).
        recoverable: bool,
    },

    // ── Plan Events ───────────────────────────────────────────

    /// Plan step has started.
    PlanStepStarted {
        step: usize,
        total: usize,
        description: String,
    },

    /// Plan step has completed.
    PlanStepComplete {
        step: usize,
        total: usize,
    },

    // ── Cost Events ───────────────────────────────────────────

    /// Cost has been incremented.
    CostUpdate {
        incremental_usd: f64,
        cumulative_usd: f64,
        session_budget_usd: f64,
    },

    // ── Lifecycle Events ──────────────────────────────────────

    /// Agent has started.
    AgentStarted {
        agent_id: AgentId,
    },

    /// Cancellation has been requested.
    Cancelled {
        reason: String,
    },
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
}

/// Tool call information from LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub call_id: CallId,
    pub tool_name: String,
    pub args: String,
}

/// Intermediate progress from tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolProgressUpdate {
    /// A line of stdout from a shell command.
    StdoutLine(String),
    /// A line of stderr from a shell command.
    StderrLine(String),
    /// Generic progress message.
    Message(String),
    /// Percentage-based progress (0-100).
    Percentage(u8),
    /// File being processed.
    ProcessingFile(PathBuf),
}

/// Unique identifier for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CallId(String);
```

---

## AgentExecutor::run_stream

The `run_stream` method is the core producer. It wraps the ReAct loop and yields `StreamEvent`s:

```rust
impl AgentExecutor {
    /// Run the agent in streaming mode.
    /// Returns a stream of StreamEvents that the consumer can process at its own pace.
    pub fn run_stream(
        &self,
        agent: &dyn Agent,
        prompt: &str,
        provider: &dyn LlmProvider,
        cancellation_token: CancellationToken,
    ) -> impl Stream<Item = StreamEvent> {
        let (tx, rx) = mpsc::channel::<StreamEvent>(self.config.stream_buffer_size);

        // Spawn the ReAct loop in a background task
        let agent_id = agent.id().clone();
        let max_turns = agent.max_turns();
        let system_prompt = agent.system_prompt().to_string();

        tokio::spawn(async move {
            let mut ctx = AgentContext::new(/* ... */);
            let mut turn = 0u32;
            let mut cumulative_cost = 0.0f64;
            let mut conversation = vec![Message::system(&system_prompt)];
            conversation.push(Message::user(prompt));

            // ── on_start ──────────────────────────────────
            if let Err(e) = agent.on_start(&mut ctx).await {
                let _ = tx.send(StreamEvent::Error {
                    error: e.to_string(),
                    recoverable: false,
                }).await;
                return;
            }

            let _ = tx.send(StreamEvent::AgentStarted {
                agent_id: agent_id.clone(),
            }).await;

            // ── Main ReAct Loop ───────────────────────────
            loop {
                // Check cancellation
                if cancellation_token.is_cancelled() {
                    let _ = tx.send(StreamEvent::Cancelled {
                        reason: "cancellation token".to_string(),
                    }).await;
                    break;
                }

                // Check max turns
                if turn >= max_turns {
                    break;
                }

                // ── before_llm_call ───────────────────────
                let mut request = LlmRequest {
                    model: provider.default_model(),
                    messages: conversation.clone(),
                    stream: true,
                    ..Default::default()
                };

                if let Err(e) = agent.before_llm_call(&mut request, &mut ctx).await {
                    let _ = tx.send(StreamEvent::Error {
                        error: e.to_string(),
                        recoverable: true,
                    }).await;
                    break;
                }

                // ── Stream LLM Response ────────────────────
                let mut full_response = String::new();
                let mut tool_calls = Vec::new();
                let mut usage = TokenUsage::default();
                let mut turn_cost = 0.0f64;

                match provider.stream(request).await {
                    Ok(mut token_stream) => {
                        while let Some(chunk) = token_stream.next().await {
                            match chunk {
                                LlmStreamChunk::Token(token) => {
                                    full_response.push_str(&token);
                                    let _ = tx.send(StreamEvent::LlmToken {
                                        token,
                                        model: provider.default_model(),
                                        output_tokens_so_far: usage.output_tokens,
                                    }).await;
                                }
                                LlmStreamChunk::ToolCallStart { call_id, name } => {
                                    tool_calls.push(ToolCallInfo {
                                        call_id: CallId(call_id),
                                        tool_name: name,
                                        args: String::new(),
                                    });
                                }
                                LlmStreamChunk::ToolCallArgs { call_id, args_chunk } => {
                                    if let Some(tc) = tool_calls.iter_mut()
                                        .find(|tc| tc.call_id.0 == call_id)
                                    {
                                        tc.args.push_str(&args_chunk);
                                    }
                                }
                                LlmStreamChunk::Usage(u) => {
                                    usage = u;
                                }
                                LlmStreamChunk::Cost(c) => {
                                    turn_cost = c;
                                }
                                LlmStreamChunk::Done => break,
                                LlmStreamChunk::Error(e) => {
                                    let _ = tx.send(StreamEvent::Error {
                                        error: e,
                                        recoverable: true,
                                    }).await;
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamEvent::Error {
                            error: e.to_string(),
                            recoverable: true,
                        }).await;
                        break;
                    }
                }

                // ── LlmResponseComplete ────────────────────
                let _ = tx.send(StreamEvent::LlmResponseComplete {
                    full_text: full_response.clone(),
                    usage: usage.clone(),
                    cost_usd: turn_cost,
                    tool_calls: tool_calls.clone(),
                }).await;

                // ── after_llm_call ─────────────────────────
                let response = LlmResponse {
                    content: full_response.clone(),
                    tool_calls: tool_calls.iter().map(|tc| ToolCall {
                        id: tc.call_id.0.clone(),
                        name: tc.tool_name.clone(),
                        arguments: tc.args.clone(),
                    }).collect(),
                    usage: usage.clone(),
                    cost: turn_cost,
                };

                if let Err(e) = agent.after_llm_call(&response, &mut ctx).await {
                    let _ = tx.send(StreamEvent::Error {
                        error: e.to_string(),
                        recoverable: true,
                    }).await;
                }

                // Update conversation
                conversation.push(Message::assistant(&full_response, &response.tool_calls));
                cumulative_cost += turn_cost;

                // ── Tool Dispatch ──────────────────────────
                if !tool_calls.is_empty() {
                    let mut tool_results = Vec::new();

                    for tc in &tool_calls {
                        // Emit ToolCall event
                        let _ = tx.send(StreamEvent::ToolCall {
                            tool_name: tc.tool_name.clone(),
                            args: tc.args.clone(),
                            call_id: tc.call_id.clone(),
                        }).await;

                        // before_tool hook
                        let verdict = agent.before_tool(
                            &tc.tool_name, &tc.args, &mut ctx,
                        ).await;

                        match verdict {
                            Ok(ToolVerdict::Allow) => {}
                            Ok(ToolVerdict::Deny { reason }) => {
                                let _ = tx.send(StreamEvent::ToolRejected {
                                    call_id: tc.call_id.clone(),
                                    reason,
                                }).await;
                                tool_results.push((
                                    tc.call_id.clone(),
                                    ToolOutput::error("Tool call denied by guardrail"),
                                ));
                                continue;
                            }
                            Ok(ToolVerdict::Redirect { tool_name, args }) => {
                                // Execute the redirected tool instead
                                // (implementation elided for brevity)
                            }
                            Err(e) => {
                                let _ = tx.send(StreamEvent::Error {
                                    error: e.to_string(),
                                    recoverable: true,
                                }).await;
                                continue;
                            }
                        }

                        // Emit ToolExecuting event
                        let _ = tx.send(StreamEvent::ToolExecuting {
                            call_id: tc.call_id.clone(),
                            tool_name: tc.tool_name.clone(),
                        }).await;

                        // Execute the tool with streaming progress
                        let result = self.execute_tool_streaming(
                            &tc.tool_name,
                            &tc.args,
                            &tc.call_id,
                            &mut ctx,
                            &tx,
                        ).await;

                        // after_tool hook
                        let mut mutable_result = result;
                        let _ = agent.after_tool(
                            &tc.tool_name, &mut mutable_result, &mut ctx,
                        ).await;

                        // Emit ToolResult event
                        let _ = tx.send(StreamEvent::ToolResult {
                            call_id: tc.call_id.clone(),
                            result: mutable_result.clone(),
                        }).await;

                        tool_results.push((tc.call_id.clone(), mutable_result));
                    }

                    // on_tool_result hook
                    let results_ref: Vec<(String, ToolOutput)> = tool_results.iter()
                        .map(|(id, r)| (id.0.clone(), r.clone()))
                        .collect();
                    let _ = agent.on_tool_result(&results_ref, &mut ctx).await;

                    // Add tool results to conversation
                    for (id, result) in &tool_results {
                        conversation.push(Message::tool_result(&id.0, &result.to_string()));
                    }
                }

                // ── Turn Complete ──────────────────────────
                turn += 1;
                let _ = tx.send(StreamEvent::TurnComplete {
                    turn,
                    turn_cost_usd: turn_cost,
                    cumulative_cost_usd: cumulative_cost,
                }).await;

                let should_stop = agent.on_turn_complete(turn, &mut ctx).await
                    .unwrap_or(true);

                if should_stop {
                    break;
                }
            }

            // ── on_finish ─────────────────────────────────
            let outcome = AgentOutcome {
                success: true,
                summary: "Task completed".to_string(),
                total_cost_usd: cumulative_cost,
                total_turns: turn,
                ..Default::default()
            };

            let _ = agent.on_finish(&outcome, &mut ctx).await;
            let _ = tx.send(StreamEvent::AgentComplete {
                result: outcome,
            }).await;
        });

        // Return the receiver as a stream
        tokio_stream::wrappers::ReceiverStream::new(rx)
    }

    /// Execute a tool with streaming progress.
    async fn execute_tool_streaming(
        &self,
        tool_name: &str,
        args: &str,
        call_id: &CallId,
        ctx: &mut AgentContext,
        tx: &mpsc::Sender<StreamEvent>,
    ) -> ToolOutput {
        let tool = match self.tools.iter().find(|t| t.name() == tool_name) {
            Some(t) => t,
            None => return ToolOutput::error(&format!("Unknown tool: {}", tool_name)),
        };

        let tool_ctx = ToolContext {
            workspace: ctx.workspace.clone(),
            git: ctx.git.clone(),
            signal_bus: ctx.signal_bus.clone(),
            cancellation_token: ctx.cancellation_token.clone(),
            progress_callback: Some(Box::new({
                let tx = tx.clone();
                let call_id = call_id.clone();
                move |update: ToolProgressUpdate| {
                    let _ = tx.try_send(StreamEvent::ToolProgress {
                        call_id: call_id.clone(),
                        update,
                    });
                }
            })),
        };

        tool.execute(args, &tool_ctx).await
    }
}
```

---

## SSE Bridge for Axum

For remote access, xaft can serve `StreamEvent`s via Server-Sent Events:

```rust
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use futures::stream::Stream;

/// SSE handler that converts StreamEvents to SSE events.
async fn stream_handler(
    State(state): State<Arc<StreamState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.signal_bus.subscribe::<StreamEvent>();

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|event| {
            let data = serde_json::to_string(&event).unwrap_or_default();
            let event_type = match &event {
                StreamEvent::LlmToken { .. } => "token",
                StreamEvent::ToolCall { .. } => "tool_call",
                StreamEvent::ToolResult { .. } => "tool_result",
                StreamEvent::TurnComplete { .. } => "turn_complete",
                StreamEvent::AgentComplete { .. } => "agent_complete",
                StreamEvent::Error { .. } => "error",
                _ => "other",
            };
            Ok(Event::default()
                .event(event_type)
                .data(data))
        });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Build the Axum SSE router.
pub fn sse_router(state: Arc<StreamState>) -> Router {
    Router::new()
        .route("/stream", get(stream_handler))
        .route("/status", get(status_handler))
        .with_state(state)
}
```

### SSE Client Protocol

```javascript
// Example client connecting to xaft SSE stream
const eventSource = new EventSource('http://localhost:8080/stream');

eventSource.addEventListener('token', (e) => {
    const data = JSON.parse(e.data);
    appendToOutput(data.token);
});

eventSource.addEventListener('tool_call', (e) => {
    const data = JSON.parse(e.data);
    showToolCall(data.tool_name, data.args);
});

eventSource.addEventListener('tool_result', (e) => {
    const data = JSON.parse(e.data);
    showToolResult(data.call_id, data.result);
});

eventSource.addEventListener('turn_complete', (e) => {
    const data = JSON.parse(e.data);
    updateCostDisplay(data.cumulative_cost_usd);
});

eventSource.addEventListener('agent_complete', (e) => {
    const data = JSON.parse(e.data);
    showFinalResult(data.result);
    eventSource.close();
});

eventSource.addEventListener('error', (e) => {
    // Reconnect is handled automatically by EventSource
});
```

---

## TUI Consumption

The TUI consumes `StreamEvent`s in its 60fps render loop:

```rust
/// TUI streaming event processor.
pub struct TuiStreamConsumer {
    /// Channel receiver for stream events.
    rx: mpsc::Receiver<StreamEvent>,

    /// Buffered tokens for efficient rendering.
    token_buffer: String,

    /// Maximum tokens to buffer before forcing a render.
    token_buffer_limit: usize,

    /// Current tool being executed (for progress display).
    active_tool: Option<ActiveToolInfo>,

    /// Cost display state.
    cost_state: CostDisplayState,

    /// Plan progress state.
    plan_state: PlanDisplayState,
}

pub struct ActiveToolInfo {
    pub name: String,
    pub call_id: CallId,
    pub start_time: Instant,
    pub stdout_lines: Vec<String>,
}

pub struct CostDisplayState {
    pub session_spent: f64,
    pub session_budget: f64,
    pub daily_spent: f64,
    pub daily_budget: f64,
}

pub struct PlanDisplayState {
    pub current_step: usize,
    pub total_steps: usize,
    pub step_descriptions: Vec<String>,
}

impl TuiStreamConsumer {
    /// Process all pending stream events.
    /// Called from the TUI's main render loop at ~60fps.
    /// Returns a list of UI updates to apply.
    pub fn process_pending(&mut self) -> Vec<TuiUpdate> {
        let mut updates = Vec::new();

        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.handle_event(event, &mut updates);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Closed) => {
                    updates.push(TuiUpdate::StreamEnded);
                    break;
                }
            }
        }

        // Flush any buffered tokens
        if !self.token_buffer.is_empty() {
            updates.push(TuiUpdate::AppendText {
                content: std::mem::take(&mut self.token_buffer),
            });
        }

        updates
    }

    fn handle_event(&mut self, event: StreamEvent, updates: &mut Vec<TuiUpdate>) {
        match event {
            StreamEvent::LlmToken { token, .. } => {
                // Buffer tokens for batch rendering
                self.token_buffer.push_str(&token);
                if self.token_buffer.len() >= self.token_buffer_limit {
                    updates.push(TuiUpdate::AppendText {
                        content: std::mem::take(&mut self.token_buffer),
                    });
                }
            }

            StreamEvent::LlmThinking { content } => {
                updates.push(TuiUpdate::AppendThinking { content });
            }

            StreamEvent::LlmResponseComplete { tool_calls, .. } => {
                // Flush remaining tokens
                if !self.token_buffer.is_empty() {
                    updates.push(TuiUpdate::AppendText {
                        content: std::mem::take(&mut self.token_buffer),
                    });
                }

                if !tool_calls.is_empty() {
                    updates.push(TuiUpdate::ToolCallsExpected {
                        count: tool_calls.len(),
                    });
                }
            }

            StreamEvent::ToolCall { tool_name, args, call_id } => {
                updates.push(TuiUpdate::ToolStarted {
                    name: tool_name.clone(),
                    args: args.clone(),
                });
                self.active_tool = Some(ActiveToolInfo {
                    name: tool_name,
                    call_id,
                    start_time: Instant::now(),
                    stdout_lines: Vec::new(),
                });
            }

            StreamEvent::ToolProgress { update, .. } => {
                match update {
                    ToolProgressUpdate::StdoutLine(line) => {
                        if let Some(ref mut tool) = self.active_tool {
                            tool.stdout_lines.push(line.clone());
                        }
                        updates.push(TuiUpdate::ToolOutput { line });
                    }
                    ToolProgressUpdate::StderrLine(line) => {
                        updates.push(TuiUpdate::ToolError { line });
                    }
                    ToolProgressUpdate::Percentage(pct) => {
                        updates.push(TuiUpdate::ToolProgress { percent: pct });
                    }
                    ToolProgressUpdate::Message(msg) => {
                        updates.push(TuiUpdate::ToolMessage { message: msg });
                    }
                    ToolProgressUpdate::ProcessingFile(path) => {
                        updates.push(TuiUpdate::ToolProcessingFile {
                            path: path.to_string_lossy().to_string(),
                        });
                    }
                }
            }

            StreamEvent::ToolResult { result, .. } => {
                self.active_tool = None;
                updates.push(TuiUpdate::ToolCompleted {
                    success: result.is_ok(),
                    summary: result.summary(),
                });
            }

            StreamEvent::ToolRejected { reason, .. } => {
                updates.push(TuiUpdate::ToolRejected { reason });
            }

            StreamEvent::TurnComplete { turn, cumulative_cost_usd, .. } => {
                updates.push(TuiUpdate::TurnComplete {
                    turn,
                    cost: cumulative_cost_usd,
                });
            }

            StreamEvent::AgentComplete { result } => {
                updates.push(TuiUpdate::AgentComplete { result });
            }

            StreamEvent::Error { error, recoverable } => {
                updates.push(TuiUpdate::Error { error, recoverable });
            }

            StreamEvent::CostUpdate { incremental_usd, cumulative_usd, .. } => {
                self.cost_state.session_spent = cumulative_usd;
                updates.push(TuiUpdate::CostUpdate {
                    incremental: incremental_usd,
                    cumulative: cumulative_usd,
                });
            }

            StreamEvent::PlanStepStarted { step, total, description } => {
                self.plan_state.current_step = step;
                self.plan_state.total_steps = total;
                updates.push(TuiUpdate::PlanStepStarted { step, total, description });
            }

            StreamEvent::Cancelled { reason } => {
                updates.push(TuiUpdate::Cancelled { reason });
            }

            _ => {} // Other events handled elsewhere
        }
    }
}
```

---

## Backpressure Handling

When the consumer (TUI, SSE) is slower than the producer (LLM stream), backpressure must be managed:

```
LLM Provider (fast: ~100 tokens/sec)
    │
    │  StreamEvent::LlmToken (high frequency)
    │
    ▼
┌──────────────────────────────────────┐
│ Bounded Channel (buffer_size: 1024)  │
│                                      │
│  ┌───┬───┬───┬───┬───┬───┬───┬───┐  │
│  │ T │ T │ T │ T │...│ T │ T │ T │  │
│  └───┴───┴───┴───┴───┴───┴───┴───┘  │
│                                      │
│  When full:                          │
│  ├── DropOldest: evict oldest token  │
│  ├── Block: wait for consumer        │
│  └── Batch: combine adjacent tokens  │
└──────────────┬───────────────────────┘
               │
               ▼
    Consumer (TUI: ~60fps = ~17ms/frame)
```

### Backpressure Strategy

xaft uses a **tiered** backpressure strategy:

| Event Type | Buffer Size | Full Strategy | Rationale |
|---|---|---|---|
| `LlmToken` | 1024 | Drop oldest + batch | Tokens are additive; dropping old ones means slightly delayed display |
| `ToolProgress` | 256 | Drop oldest | Progress is informational; stale progress can be discarded |
| `ToolResult` | 64 | Block | Must not lose tool results |
| `TurnComplete` | 32 | Block | Must not lose turn boundaries |
| `AgentComplete` | 8 | Block | Must not lose final result |
| `Error` | 16 | Block | Must not lose errors |

```rust
/// Backpressure-aware channel configuration.
pub struct StreamChannelConfig {
    pub token_buffer: usize,       // 1024
    pub tool_progress_buffer: usize, // 256
    pub tool_result_buffer: usize, // 64
    pub turn_buffer: usize,       // 32
    pub completion_buffer: usize,  // 8
    pub error_buffer: usize,      // 16
}

impl Default for StreamChannelConfig {
    fn default() -> Self {
        Self {
            token_buffer: 1024,
            tool_progress_buffer: 256,
            tool_result_buffer: 64,
            turn_buffer: 32,
            completion_buffer: 8,
            error_buffer: 16,
        }
    }
}

/// Smart emit with backpressure handling.
impl StreamProducer {
    async fn emit_with_backpressure(
        &self,
        event: StreamEvent,
    ) -> Result<(), StreamError> {
        match &event {
            StreamEvent::LlmToken { .. } => {
                // Try to send; if channel full, batch with previous token
                match self.tx.try_send(event) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(event)) => {
                        // Buffer is full; batch this token with the last one
                        self.token_batcher.add(event);
                        Ok(())
                    }
                    Err(e) => Err(StreamError::ChannelClosed),
                }
            }
            StreamEvent::ToolProgress { .. } => {
                // Drop oldest if full
                match self.tx.try_send(event) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Drop this progress update
                        Ok(())
                    }
                    Err(e) => Err(StreamError::ChannelClosed),
                }
            }
            _ => {
                // Block for important events
                self.tx.send(event).await
                    .map_err(|_| StreamError::ChannelClosed)
            }
        }
    }
}
```

---

## Token-by-Token Rendering

The TUI renders tokens incrementally for a smooth, responsive feel:

```rust
/// Token renderer for the TUI output panel.
pub struct TokenRenderer {
    /// Current line buffer (not yet committed to the output).
    current_line: String,

    /// Committed lines (rendered to screen).
    committed_lines: Vec<RenderedLine>,

    /// Syntax highlighter for the current context.
    highlighter: Box<dyn Highlighter>,

    /// Whether we're in a code block.
    in_code_block: bool,

    /// Current code language (for syntax highlighting).
    code_language: Option<String>,
}

impl TokenRenderer {
    /// Append a token to the current line.
    /// Handles line breaks, code block detection, and syntax highlighting.
    pub fn append_token(&mut self, token: &str) -> RenderUpdate {
        let mut updates = Vec::new();

        for ch in token.chars() {
            if ch == '\n' {
                // Commit the current line
                let rendered = self.render_line(&self.current_line);
                self.committed_lines.push(rendered);
                updates.push(RenderUpdate::NewLine);
                self.current_line.clear();
            } else {
                self.current_line.push(ch);
            }
        }

        // Render partial line (for display before newline)
        if !self.current_line.is_empty() {
            let rendered = self.render_line(&self.current_line);
            updates.push(RenderUpdate::PartialLine {
                content: rendered.content,
                cursor_pos: rendered.content.len(),
            });
        }

        RenderUpdate::Batch(updates)
    }

    fn render_line(&self, line: &str) -> RenderedLine {
        // Detect code block boundaries
        if line.trim().starts_with("```") {
            self.in_code_block = !self.in_code_block;
            if self.in_code_block {
                self.code_language = Some(
                    line.trim().trim_start_matches('`').trim().to_string()
                );
            } else {
                self.code_language = None;
            }
        }

        // Apply syntax highlighting for code blocks
        if self.in_code_block {
            if let Some(ref lang) = self.code_language {
                return self.highlighter.highlight(line, lang);
            }
        }

        // Markdown rendering for non-code text
        RenderedLine::markdown(line)
    }
}
```

---

## Streaming and Approval Gates

When a tool requires approval, the stream must pause and wait for the user:

```
StreamEvent::ToolCall { tool_name: "bash_exec", args: "rm -rf /tmp/test", call_id }
    │
    ▼
AgentExecutor checks: approval_policy.requires_approval("bash_exec")?
    │
    ├── No → proceed with execution
    │
    └── Yes → pause stream, emit ApprovalRequired event
            │
            ▼
        ┌──────────────────────────────────────────────┐
        │  Approval Gate (TUI renders confirmation)     │
        │                                                │
        │  Tool: bash_exec                               │
        │  Args: rm -rf /tmp/test                        │
        │                                                │
        │  [a]pprove  [r]eject  [e]dit  [v]iew details  │
        └──────────────────────┬─────────────────────────┘
                               │
                    ┌──────────┼──────────┐
                    ▼          ▼          ▼
                approve     reject      edit
                    │          │          │
                    ▼          ▼          ▼
              execute tool  emit         allow user
              normally      ToolRejected to modify args
                                   │    before approving
                                   ▼
                            inject "denied"
                            into agent context
```

### Implementation

```rust
/// Approval gate that integrates with streaming.
pub struct ApprovalGate {
    policy: ApprovalPolicy,
    output_sink: Arc<dyn OutputSink>,
    pending_approvals: HashMap<CallId, PendingApproval>,
}

pub struct PendingApproval {
    tool_name: String,
    args: String,
    created_at: Instant,
    timeout: Option<Duration>,
    result_tx: oneshot::Sender<ApprovalDecision>,
}

pub enum ApprovalDecision {
    Approved,
    Rejected { reason: String },
    Modified { new_args: String },
}

impl ApprovalGate {
    /// Check if a tool requires approval and handle it.
    /// This method may block waiting for user input.
    pub async fn check(
        &mut self,
        tool_name: &str,
        args: &str,
        call_id: &CallId,
    ) -> Result<ApprovalDecision, StreamError> {
        if !self.policy.requires_approval(tool_name) {
            return Ok(ApprovalDecision::Approved);
        }

        // Present approval request to the user
        let (result_tx, result_rx) = oneshot::channel();

        self.pending_approvals.insert(
            call_id.clone(),
            PendingApproval {
                tool_name: tool_name.to_string(),
                args: args.to_string(),
                created_at: Instant::now(),
                timeout: self.policy.approval_timeout(tool_name),
                result_tx,
            },
        );

        // Render approval prompt in TUI
        self.output_sink.request_approval(
            tool_name,
            args,
            call_id,
        ).await?;

        // Wait for user decision (with optional timeout)
        let decision = match self.policy.approval_timeout(tool_name) {
            Some(timeout) => {
                match tokio::time::timeout(timeout, result_rx).await {
                    Ok(Ok(decision)) => decision,
                    Ok(Err(_)) => ApprovalDecision::Rejected {
                        reason: "approval channel closed".to_string(),
                    },
                    Err(_) => ApprovalDecision::Rejected {
                        reason: "approval timed out".to_string(),
                    },
                }
            }
            None => {
                result_rx.await.unwrap_or(ApprovalDecision::Rejected {
                    reason: "approval channel closed".to_string(),
                })
            }
        };

        self.pending_approvals.remove(call_id);

        Ok(decision)
    }
}
```

---

## Cancellation During Stream

The `CancellationToken` propagates through the entire streaming pipeline:

```rust
/// How cancellation propagates through streaming.
///
/// CancellationToken::cancel()
///     │
///     ├── AgentExecutor detects cancellation
///     │   ├── Stop producing new StreamEvents
///     │   ├── Emit StreamEvent::Cancelled
///     │   └── Call agent.on_finish()
///     │
///     ├── Tool execution detects cancellation
///     │   ├── Abort running shell commands (SIGTERM → SIGKILL)
///     │   ├── Rollback pending FileEditor transactions
///     │   └── Return ToolOutput::Cancelled
///     │
///     ├── LLM provider detects cancellation
///     │   ├── Close HTTP connection
///     │   └── Return partial response
///     │
///     └── TUI detects cancellation
///         ├── Display "Cancelled by user" message
///         ├── Show partial results
///         └── Prompt: [r]esume  [d]iscard  [q]uit
```

---

## Streaming Performance

### Benchmarks (Target)

| Metric | Target | Strategy |
|---|---|---|
| First token latency | <500ms | Provider pre-warming, connection pooling |
| Token throughput | >200 tokens/sec | Zero-copy where possible, batching |
| TUI render latency | <16ms per frame | Non-blocking try_recv, deferred rendering |
| SSE overhead | <5ms per event | Minimal serialization, direct JSON |
| Memory per active stream | <1MB | Bounded channels, no unbounded buffers |
| Backpressure recovery | <100ms | Aggressive batching for token events |

### Zero-Copy Paths

For high-throughput token streaming, xaft minimizes allocations:

```rust
/// Zero-copy token forwarding for SSE.
/// Instead of serializing each token individually, we forward the raw
/// JSON chunk from the LLM provider directly to the SSE stream.
pub async fn stream_tokens_zero_copy(
    provider: &dyn LlmProvider,
    request: LlmRequest,
) -> impl Stream<Item = Result<Event, Infallible>> {
    provider.stream_raw(request)
        .await
        .map(|chunk| {
            // chunk is already SSE-formatted JSON from the provider
            Ok(Event::default()
                .event("token")
                .data(chunk))
        })
}
```

### Connection Pooling

The LLM provider maintains a connection pool for streaming:

```rust
/// Connection pool for LLM provider HTTP streams.
pub struct StreamingConnectionPool {
    /// Pool of reusable HTTP connections.
    pool: deadpool::managed::Pool<Connection>,
    /// Maximum concurrent streams.
    max_concurrent: usize,
}

impl StreamingConnectionPool {
    /// Get a streaming connection from the pool.
    pub async fn acquire(&self) -> Result<PooledConnection, PoolError> {
        self.pool.get().await
    }
}
```
