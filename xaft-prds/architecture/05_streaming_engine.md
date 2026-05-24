# Streaming Engine

## Streaming-First Architecture

Every user-visible agent action in `xaft` is streamed in real time. From the moment an LLM begins generating a response, tokens appear in the TUI. From the moment a tool begins executing, its status updates in the activity pane. The user is never staring at a spinner.

## StreamEvent Pipeline

```
AgentExecutor::run_stream()
    │
    │ Pin<Box<dyn Stream<Item = StreamEvent> + Send>>
    ▼
PlanExecutor::consume_stream()
    │ pattern-matches on each event
    ▼
mpsc::Sender<UiEvent>           ← non-blocking send
    │
    ▼
TUI render loop                  ← drains UiEvent queue on each tick
    │ mutates AppState
    ▼
terminal.draw(|frame| render(frame, &state))   ← 30fps
```

## StreamEvent Types

From `agtrs-runtime`:

```rust
pub enum StreamEvent {
    TextDelta { delta: String },
    ThinkingDelta { delta: String },
    ToolCallDelta { delta: ToolCallDelta },
    ToolExecution { tool_name: String, tool_use_id: String },
    ToolResult { result: ToolResult },
    PendingApproval { agent_run_id: String, tool_name: String, tool_use_id: String },
    GuardrailOverride { content: String },
    Done {
        content: String,
        stop_reason: StopReason,
        usage: TokenUsage,
        turns: usize,
        agent_name: String,
        messages: Vec<Message>,
    },
    Error { message: String },
}
```

`xaft` maps `StreamEvent` to `UiEvent` with additional context:

```rust
pub enum UiEvent {
    // Agent streaming
    AgentTextDelta { agent_idx: usize, agent_name: String, delta: String },
    AgentThinkingDelta { agent_idx: usize, delta: String },
    AgentToolCall { agent_idx: usize, tool_name: String, tool_use_id: String, input: serde_json::Value },
    AgentToolResult { agent_idx: usize, tool_name: String, result: ToolResult },
    AgentDone { agent_idx: usize, response: AgentResponse },
    AgentError { agent_idx: usize, message: String },

    // Plan events (from SignalBus)
    PlanStepStarted { step_id: String, description: String },
    PlanStepCompleted { step_id: String, duration_ms: f64 },
    PlanStepFailed { step_id: String, reason: String },

    // Workspace events
    FileWritten { path: PathBuf, lines_changed: usize },
    PatchApplied { path: PathBuf, stats: PatchStats },

    // Shell events
    ShellOutput { command: String, chunk: String, is_stderr: bool },
    ShellComplete { command: String, exit_code: i32 },

    // Approval
    ApprovalRequired { tool_name: String, input: serde_json::Value, risk: RiskLevel },
    ApprovalDecided { approved: bool },

    // Session
    CostUpdate { session_total: f64, task_total: f64, last_call: f64 },
    CheckpointSaved { step: usize, total: usize },

    // UI control
    Tick,
    KeyEvent(crossterm::event::KeyEvent),
}
```

## PlanExecutor Streaming Consumer

```rust
pub async fn execute_step_with_streaming(
    step: &PlanStep,
    agent: &dyn Agent,
    ctx: &mut AgentContext,
    ui_tx: mpsc::Sender<UiEvent>,
    agent_idx: usize,
) -> Result<AgentResponse, XaftError> {
    let mut stream = AgentExecutor::run_stream(agent, Message::user(&step.description), ctx);

    let mut final_response = None;

    while let Some(event) = stream.next().await {
        // Map StreamEvent → UiEvent and forward
        let ui_event = match &event {
            StreamEvent::TextDelta { delta } => {
                Some(UiEvent::AgentTextDelta {
                    agent_idx,
                    agent_name: agent.name().to_string(),
                    delta: delta.clone(),
                })
            }
            StreamEvent::ToolExecution { tool_name, tool_use_id } => {
                Some(UiEvent::AgentToolCall {
                    agent_idx,
                    tool_name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    input: serde_json::Value::Null,  // filled by ToolCallStarted signal
                })
            }
            StreamEvent::PendingApproval { tool_name, .. } => {
                Some(UiEvent::ApprovalRequired {
                    tool_name: tool_name.clone(),
                    input: serde_json::Value::Null,
                    risk: RiskLevel::High,
                })
            }
            StreamEvent::Done { content, usage, turns, .. } => {
                final_response = Some(AgentResponse {
                    content: content.clone(),
                    turns: *turns,
                    total_usage: usage.clone(),
                    ..Default::default()
                });
                Some(UiEvent::AgentDone { agent_idx, response: final_response.clone().unwrap() })
            }
            StreamEvent::Error { message } => {
                Some(UiEvent::AgentError { agent_idx, message: message.clone() })
            }
            _ => None,
        };

        if let Some(evt) = ui_event {
            // Non-blocking: if TUI buffer full, log and continue
            if ui_tx.try_send(evt).is_err() {
                tracing::warn!("UI event queue full — dropping event");
            }
        }

        // Approval gate: block until user responds
        if let StreamEvent::PendingApproval { .. } = event {
            let approved = wait_for_approval(ui_tx.clone()).await?;
            if !approved {
                return Err(XaftError::Cancelled { reason: "user rejected tool call".into() });
            }
        }
    }

    final_response.ok_or_else(|| XaftError::Agtrs(AgtrsError::msg("stream ended without Done event")))
}
```

## Shell Streaming

Shell commands pipe their output to the TUI in real time:

```rust
// agtrs-shell/src/streaming.rs
pub async fn run_streaming(
    cmd: &str,
    policy: &ShellPolicy,
    ui_tx: mpsc::Sender<UiEvent>,
) -> Result<ShellOutput, ShellError> {
    let mut child = Command::new("sh")
        .arg("-c").arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Stream stdout
    let tx1 = ui_tx.clone();
    let cmd1 = cmd.to_string();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            tx1.send(UiEvent::ShellOutput {
                command: cmd1.clone(),
                chunk: line.clone(),
                is_stderr: false,
            }).await.ok();
            line.clear();
        }
    });

    // Stream stderr similarly...

    let status = child.wait().await?;
    Ok(ShellOutput { exit_code: status.code().unwrap_or(-1), ... })
}
```

## Backpressure Strategy

| Buffer | Size | Overflow behavior |
|---|---|---|
| `mpsc::Sender<UiEvent>` | 1024 | `try_send` failure → log warning, drop event |
| `SignalBus` broadcast | 256 per type | Lagged receiver drops old events |
| Shell stdout buffer | 64KB | Flush after each newline |
| TUI frame buffer | 1 frame | Always overwrites — no queue |

**Design rationale**: It is acceptable to drop TUI events under load. The TUI shows the *current* state, not a full history. Audit logs (from SignalBus subscribers with dedicated buffers) must never drop events.

## Streaming LLM Provider Requirements

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream(
        &self,
        messages: &[Message],
        options: &LlmOptions,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<CompletionChunk, AgtrsError>> + Send>>,
        AgtrsError,
    >;

    fn supports_streaming(&self) -> bool;
}
```

If `supports_streaming()` returns `false`, `AgentExecutor::run_stream` falls back to `complete()` and emits a single `TextDelta` event with the full response. The TUI shows a buffered display.

## Time-to-First-Token Target

**Target: < 500ms from tool call completion to first token in TUI.**

Bottlenecks to monitor:
1. `lm.complete()` → first streaming chunk: provider-dependent (Anthropic: ~200ms)
2. `StreamEvent::TextDelta` → `UiEvent::AgentTextDelta` → `mpsc::send`: < 1ms
3. `mpsc::recv` in TUI loop → `terminal.draw()`: 0–33ms (worst case: next frame)

Total: provider latency + ≤ 34ms. Well within 500ms target.

## References

- agtrs: `agtrs-runtime/src/streaming.rs`
- agtrs: `agtrs-runtime/src/executor.rs` (run_stream)
- Next: [Concurrency Model →](06_concurrency_model.md)