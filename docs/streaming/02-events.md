# Stream Events

`StreamEvent` is the universal event type that flows through the xaft streaming pipeline. Every observable occurrence during agent execution — from a single token of LLM output to a tool approval request to a terminal completion signal — is represented as a `StreamEvent` variant. This page documents each variant in detail, including its data payload, emission semantics, consumer handling recommendations, and interaction with the cancellation system.

## StreamEvent Enum

```rust
pub enum StreamEvent {
    TextDelta { content: String },
    ThinkingDelta { content: String },
    ToolExecution { call_id: String, tool_name: String, input: serde_json::Value },
    ToolResult { call_id: String, output: ToolOutput },
    PendingApproval { call_id: String, tool_name: String, input: serde_json::Value },
    GuardrailOverride { guardrail: String, reason: String, tool_name: String },
    ToolCallDelta { call_id: String, delta: String },
    Done { summary: String, exit_code: ExitCode },
    Error { message: String, code: Option<String> },
}
```

## Variant Reference

### TextDelta

```
TextDelta { content: String }
```

A `TextDelta` event carries a fragment of the LLM's text output. These events are emitted at high frequency during active generation — typically every 10-50 milliseconds, depending on the provider's streaming configuration and the model's generation speed. Each delta contains a small substring of the complete response (often a single word or a few characters), and the consumer is responsible for concatenating consecutive deltas to reconstruct the full response.

**Emission source**: The LLM provider's streaming response. The provider translates its native streaming format into `TextDelta` events as tokens arrive from the API.

**Consumer handling**: Real-time consumers (CLI renderers, web dashboards) should display each delta immediately upon receipt, without waiting for the full response. This provides the "typewriter" effect that users expect from AI chat interfaces. Batch consumers (test harnesses, log aggregators) should accumulate deltas and reconstruct the full response at the end of the turn.

**Token counting**: `TextDelta` events are not individually counted for billing purposes. Token counting is performed by the `CostedProvider` at the provider level, using the provider's reported token counts. This avoids the inaccuracy that would result from trying to count tokens from raw text deltas (the provider may use a different tokenizer than the runtime).

**Cancellation interaction**: If the cancellation token is signaled during a `TextDelta` sequence, the event loop processes the cancellation before the next delta. The remaining deltas are never emitted — the stream is dropped, and the event loop returns `RuntimeError::Cancelled`. The consumer will see a partial response, ending at the last delta that was dispatched before cancellation.

### ThinkingDelta

```
ThinkingDelta { content: String }
```

A `ThinkingDelta` event carries a fragment of the LLM's internal reasoning or "thinking" output. Models like Claude produce this output before their final response, providing visibility into the model's chain-of-thought reasoning. The event is structurally identical to `TextDelta` but is semantically distinct: it represents the agent's internal deliberation, not its outward-facing output.

**Emission source**: The LLM provider's streaming response, specifically the "thinking" or "reasoning" content blocks that some models support. Not all models produce thinking output; for models that don't, `ThinkingDelta` events are never emitted.

**Consumer handling**: Interactive consumers should display thinking content differently from text content — for example, in a collapsible "Thinking" section, with a dimmed text style, or in a separate panel. The thinking content is not part of the agent's final output and should not be presented as such. Headless consumers may choose to discard thinking deltas entirely, or to log them for debugging purposes.

**Token counting**: Thinking tokens are counted separately from output tokens by the `CostedProvider`. Some providers charge different rates for thinking tokens and output tokens, so the separate accounting is important for accurate cost tracking. The thinking token count is included in the `ModelCallComplete` signal and is persisted in the session store.

**Model compatibility**: The provider's `supports_thinking()` method indicates whether the current model produces thinking output. The runtime uses this method to decide whether to include a "show your thinking" directive in the system prompt. If the model does not support thinking, the directive is omitted to avoid confusing the model with instructions it cannot follow.

### ToolExecution

```
ToolExecution { call_id: String, tool_name: String, input: serde_json::Value }
```

A `ToolExecution` event signals that a tool has begun executing. It is emitted after the LLM produces a tool call and before the tool's execution starts. The event carries the call ID (which uniquely identifies this tool call within the session), the tool's name, and the input parameters that the LLM provided.

**Emission source**: The `AgentExecutor::run_stream()` method, when it intercepts a tool call from the LLM's response and before it dispatches the tool for execution.

**Consumer handling**: Interactive consumers should display a progress indicator for the tool — for example, "Running WriteFile..." or a spinner. The `call_id` allows the consumer to correlate the `ToolExecution` event with the subsequent `ToolResult` event, enabling the progress indicator to be replaced with the result.

**Input sanitization**: The `input` field may contain sensitive information (for example, file contents that include API keys or credentials). The runtime does not sanitize the input before including it in the event, because sanitization is lossy and would prevent the consumer from displaying the full tool call for debugging purposes. Consumers that display tool inputs to end users should implement their own sanitization — for example, redacting lines that match common secret patterns.

**Relationship to ToolCallDelta**: For models that support streaming tool call inputs, `ToolCallDelta` events precede the `ToolExecution` event. The `ToolExecution` event contains the complete, assembled input, while the `ToolCallDelta` events contain the incremental fragments. Consumers that want live preview of tool call construction should handle `ToolCallDelta` events; consumers that only need the final input should wait for `ToolExecution`.

### ToolResult

```
ToolResult { call_id: String, output: ToolOutput }
```

A `ToolResult` event carries the output of a completed tool execution. The `call_id` matches the `call_id` from the corresponding `ToolExecution` event, allowing consumers to link the result to its invocation. The `output` field contains the tool's output, which may be a successful result (text, JSON, or binary metadata) or an error.

**Emission source**: The `XaftAgent`'s `on_tool_result` hook, which is called by the orchestrator after each tool execution completes.

**Consumer handling**: Interactive consumers should replace the progress indicator (from the `ToolExecution` event) with the result. For successful results, the consumer may display a summary (for example, "Wrote 3 files"). For errors, the consumer should display the error message prominently, so the user can understand why the tool failed and whether manual intervention is needed.

**Error semantics**: A `ToolResult` with an error does not terminate the agent's execution. The LLM receives the error as a tool result and can decide how to handle it — for example, by retrying with different parameters, by trying a different approach, or by reporting the failure to the user. This is a key difference from the `Error` stream event, which terminates the entire execution.

**Ordering guarantee**: `ToolResult` events are emitted in the order that tool executions complete, which may differ from the order they were initiated if tools execute concurrently. The `call_id` is the only reliable way to match a result to its invocation — positional ordering is not guaranteed.

### PendingApproval

```
PendingApproval { call_id: String, tool_name: String, input: serde_json::Value }
```

A `PendingApproval` event signals that a tool call requires user approval before it can execute. This event is the primary mechanism of the permission system. It is emitted when a write tool is called and neither `auto_approve` nor `dangerously_skip_permissions` is set in the `RunRequest`.

**Emission source**: The `AgentExecutor::run_stream()` method, when it intercepts a write tool call that requires approval. The executor pauses the tool's execution and emits `PendingApproval` instead of `ToolExecution`.

**Consumer handling**: Interactive consumers must display an approval prompt to the user and collect a yes/no decision. The decision is communicated back to the event loop through an approval channel (a `tokio::sync::oneshot` channel). The event loop awaits the decision before proceeding — if approved, it emits `ToolExecution` and executes the tool; if denied, it synthesizes a `ToolResult` with a "user denied approval" error and continues the agent's turn loop.

**Timeout behavior**: In headless mode, `PendingApproval` events are handled by an automated approval policy (which may auto-approve, auto-deny, or time out). The default headless policy auto-approves all write tools, but this can be configured per-tool through the `RunConfig`. The timeout is configurable — if the user does not respond within the timeout period, the event loop treats the approval as denied.

**Cancellation interaction**: If the cancellation token is signaled while an approval is pending, the event loop returns `RuntimeError::Cancelled` immediately, without waiting for the approval decision. The pending approval is discarded, and the tool is never executed.

### GuardrailOverride

```
GuardrailOverride { guardrail: String, reason: String, tool_name: String }
```

A `GuardrailOverride` event signals that a content guardrail has been triggered and the agent is requesting an override. Guardrails are safety checks that run before tool execution — for example, checking that the agent is not writing outside the workspace, that the file is not in a protected directory, or that the proposed change does not match a pattern of potentially harmful modifications (like deleting configuration files).

**Emission source**: The tool execution pipeline, when a guardrail check fails and the tool is configured to request an override rather than silently deny the operation.

**Consumer handling**: Similar to `PendingApproval`, the consumer must display the override request and collect a decision. The event includes the guardrail's name and the reason it was triggered, which should be displayed to help the user make an informed decision. Override requests are rare and should be treated with caution — approving an override means bypassing a safety check that was triggered for a reason.

**Default behavior**: If no consumer is connected (for example, in headless mode with no approval policy), guardrail override requests are automatically denied. This is the safe default — it is better to fail the tool call than to silently bypass a safety check without human review.

### ToolCallDelta

```
ToolCallDelta { call_id: String, delta: String }
```

A `ToolCallDelta` event carries a fragment of a tool call's input parameters as they are being streamed by the LLM. Not all models support streaming tool call inputs; for models that do (currently Anthropic's Claude models with tool use), these deltas allow the frontend to show a live preview of the tool call as the LLM constructs it. For models that don't support this feature, the tool call input arrives atomically in the `ToolExecution` event.

**Emission source**: The LLM provider's streaming response, when the provider emits a partial tool call input.

**Consumer handling**: Consumers that support live tool call preview should accumulate `ToolCallDelta` events (keyed by `call_id`) and display the partially constructed input. Consumers that don't support live preview can safely ignore these events — the complete input will always be available in the subsequent `ToolExecution` event.

**Implementation note**: The `delta` field contains raw JSON fragments, not complete JSON objects. The consumer must concatenate all deltas for a given `call_id` before attempting to parse the result as JSON. The runtime does not guarantee that individual deltas are valid JSON — only that the concatenation of all deltas for a call produces a valid JSON object.

### Done

```
Done { summary: String, exit_code: ExitCode }
```

A `Done` event signals that the agent has completed its work. It is always the last event in the stream. The `summary` field contains a human-readable description of the agent's outcome, and the `exit_code` field indicates whether the task succeeded, failed, or was cancelled.

**Emission source**: The `XaftAgent`'s `on_finish` hook, which is called by the orchestrator when the turn loop terminates.

**Consumer handling**: Consumers should treat `Done` as the termination signal for the streaming session. After receiving `Done`, the consumer should perform any final cleanup (closing WebSocket connections, flushing output buffers, updating the session status in the UI) and then release the stream. Any events received after `Done` should be discarded — they are the result of race conditions in the streaming pipeline and are not meaningful.

**Exit code mapping**: The `exit_code` in the `Done` event maps directly to the `ExitCode` in the `RunResult`. The event loop copies this value from the `Done` event into the `RunResult` that it returns to the caller. This ensures that the exit code seen by the consumer (through the streaming pipeline) is always consistent with the exit code seen by the caller (through the `RunResult`).

**Guaranteed delivery**: The event loop guarantees that `Done` is always delivered to the consumer, even if the agent crashes or the cancellation token is signaled. In the crash case, the event loop synthesizes a `Done` event with `ExitCode::TASK_FAILED` and a summary describing the crash. In the cancellation case, it synthesizes a `Done` event with `ExitCode::CANCELLED`. This guarantee ensures that consumers always receive a termination signal and do not hang indefinitely waiting for events.

### Error

```
Error { message: String, code: Option<String> }
```

An `Error` event signals an unrecoverable error in the agent execution pipeline. Unlike `ToolResult` errors (which are per-tool and recoverable), `Error` events represent failures at the orchestration level — for example, the provider returning a persistent authentication error, the agent exceeding its maximum turn count, or the budget being exceeded.

**Emission source**: The `AgentExecutor::run_stream()` method, when it encounters an error that cannot be recovered through retries or alternate tools.

**Consumer handling**: Consumers should display the error message prominently and then close the streaming session. After an `Error` event, the event loop will emit a `Done` event with a matching exit code, so the consumer should not close the session until `Done` is received. The `code` field, if present, provides a machine-readable error code that can be used for automated error handling (for example, retrying the task with a different configuration for certain error codes).

**Distinction from ToolResult errors**: It is important to distinguish `Error` stream events from `ToolResult` events that contain errors. A `ToolResult` error is a normal part of the agent's execution — the LLM receives the error and can respond to it. An `Error` stream event is a terminal failure that the LLM never sees — the execution is aborted before the LLM can respond. Consumers should handle these two cases differently: `ToolResult` errors should be displayed as informational (the agent is handling the failure), while `Error` events should be displayed as critical (the agent cannot continue).

## Cancellation Semantics

The cancellation system interacts with the streaming pipeline at the event loop level. When the cancellation token is signaled, the event loop's `tokio::select!` branch picks it up before processing the next event. The behavior depends on the current state:

| Current State | Cancellation Behavior |
|---|---|
| Processing `TextDelta` or `ThinkingDelta` | Immediately return `Cancelled`; remaining deltas are dropped |
| Awaiting `PendingApproval` | Immediately return `Cancelled`; approval is discarded |
| Processing `ToolExecution` | Wait for the tool to complete, then return `Cancelled` |
| Processing `ToolResult` | Dispatch the result, then return `Cancelled` |
| Processing `Done` | Dispatch `Done` with `Cancelled` exit code, then return |
| Processing `Error` | Dispatch `Error`, synthesize `Done`, then return |

The key principle is that in-flight tool executions are allowed to complete before cancellation takes effect. This prevents data corruption — for example, a `WriteFile` tool that is halfway through writing a file should complete its write before the execution is terminated, because a partially-written file is worse than a fully-written file that can be rolled back through git.

## Event Ordering Invariants

The streaming pipeline guarantees the following ordering invariants:

1. **`ToolExecution` before `ToolResult`**: A tool's result always follows its execution event, with the same `call_id`.
2. **`ToolCallDelta` before `ToolExecution`**: All deltas for a tool call arrive before the complete `ToolExecution` event.
3. **`Done` is always last**: No events follow `Done`. If `Error` is emitted, it is followed by a synthetic `Done`.
4. **`PendingApproval` before `ToolExecution`**: If approval is required, the `PendingApproval` event arrives before the `ToolExecution` event (after approval is granted).
5. **No interleaving within a call**: Events for a single tool call (identified by `call_id`) are not interleaved with events for a different tool call. However, events for different tool calls may be interleaved if tools execute concurrently.

These invariants allow consumers to reconstruct the agent's execution trace from the event stream without ambiguity. Violations of these invariants indicate a bug in the streaming pipeline and should be reported.
