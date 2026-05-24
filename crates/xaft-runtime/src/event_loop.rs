//! `EventLoop` — consumes a `Stream<Item = StreamEvent>` from `AgentExecutor`.
//!
//! Responsibilities:
//! - Print incremental output to stdout (text deltas, tool executions)
//! - Update the `AgentSession` with usage/cost/turns
//! - Return the final response content
//! - Map `StreamEvent::Error` → `RuntimeError`
//! - Respect cancellation via `CancellationToken`

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use agtrs_runtime::streaming::StreamEvent;

use crate::error::RuntimeError;
use crate::session::{AgentSession, SessionStatus};
use crate::types::ExitCode;

// ── EventLoop ─────────────────────────────────────────────────────────────────

/// Drives a streaming agent run to completion.
///
/// Create one per run, then call [`EventLoop::consume()`].
pub struct EventLoop {
    /// Whether to suppress all stdout output.
    pub headless: bool,
    /// Print tool names as they execute.
    pub show_tool_executions: bool,
    /// Optional cancellation token.
    pub cancel: Option<CancellationToken>,
}

impl Default for EventLoop {
    fn default() -> Self {
        Self {
            headless: false,
            show_tool_executions: true,
            cancel: None,
        }
    }
}

impl EventLoop {
    /// Create with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create in headless mode (no stdout output).
    pub fn headless() -> Self {
        Self {
            headless: true,
            ..Default::default()
        }
    }

    /// Set a cancellation token.
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Consume the stream, update `session`, and return `(content, exit_code)`.
    ///
    /// Blocks until the stream produces a `Done` or `Error` event, or until
    /// the cancellation token is triggered.
    pub async fn consume(
        &self,
        mut stream: impl futures::Stream<Item = StreamEvent> + Unpin,
        session: &mut AgentSession,
    ) -> Result<(String, ExitCode), RuntimeError> {
        let mut accumulated_text = String::new();
        let mut exit_code = ExitCode::SUCCESS;
        let cancel = self.cancel.clone().unwrap_or_default();

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::info!("xaft: event loop cancelled");
                    session.status = SessionStatus::Cancelled;
                    return Err(RuntimeError::Cancelled("interrupted by user".into()));
                }
                item = stream.next() => match item {
                    Some(e) => e,
                    None => break, // stream ended without Done
                }
            };

            self.handle_event(event, session, &mut accumulated_text, &mut exit_code)?;

            // Break after Done or Error
            if matches!(
                session.status,
                SessionStatus::Completed { .. } | SessionStatus::Failed { .. }
            ) {
                break;
            }
        }

        Ok((accumulated_text, exit_code))
    }

    fn handle_event(
        &self,
        event: StreamEvent,
        session: &mut AgentSession,
        text_buf: &mut String,
        exit_code: &mut ExitCode,
    ) -> Result<(), RuntimeError> {
        match event {
            StreamEvent::TextDelta { delta } => {
                if !self.headless {
                    print!("{delta}");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                text_buf.push_str(&delta);
            }

            StreamEvent::ThinkingDelta { delta } => {
                if !self.headless {
                    tracing::debug!(delta = %delta, "xaft: thinking");
                }
            }

            StreamEvent::ToolExecution {
                tool_name,
                tool_use_id,
            } => {
                tracing::info!(tool = %tool_name, id = %tool_use_id, "xaft: executing tool");
                if !self.headless && self.show_tool_executions {
                    eprintln!("\n  [{}]", tool_name);
                }
            }

            StreamEvent::ToolResult { result } => {
                tracing::debug!(
                    tool_use_id = %result.tool_use_id,
                    is_error = result.is_error,
                    content_len = result.content.len(),
                    "xaft: tool result"
                );
                if result.is_error {
                    tracing::warn!(tool_use_id = %result.tool_use_id, content = %result.content, "xaft: tool returned error");
                }
            }

            StreamEvent::PendingApproval {
                tool_name,
                tool_use_id,
                ..
            } => {
                tracing::info!(tool = %tool_name, id = %tool_use_id, "xaft: pending approval");
                if !self.headless {
                    eprintln!("\n  [approval required: {}]", tool_name);
                }
            }

            StreamEvent::GuardrailOverride { content } => {
                tracing::warn!(content = %content, "xaft: guardrail override");
            }

            StreamEvent::ToolCallDelta { .. } => {
                // streaming tool call assembly — no output needed
            }

            StreamEvent::Done {
                content,
                stop_reason,
                usage,
                turns,
                agent_name,
                ..
            } => {
                tracing::info!(
                    agent = %agent_name,
                    turns,
                    stop_reason = ?stop_reason,
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    "xaft: agent run complete"
                );

                if !self.headless && !content.is_empty() {
                    // TextDelta already printed character by character;
                    // if we got here without deltas, print the final content
                    if text_buf.is_empty() {
                        println!("{content}");
                    } else {
                        println!(); // ensure newline after last delta
                    }
                }

                // Update session
                session.total_tokens += (usage.input_tokens + usage.output_tokens) as u64;
                session.turn_count = turns as u32;
                session.status = SessionStatus::Completed {
                    summary: content.chars().take(200).collect(),
                };
                *text_buf = content;
            }

            StreamEvent::Error { message } => {
                tracing::error!(message = %message, "xaft: agent stream error");
                *exit_code = ExitCode::TASK_FAILED;
                session.status = SessionStatus::Failed {
                    error: message.clone(),
                };
                return Err(RuntimeError::AgentFailed(message));
            }
        }

        Ok(())
    }
}

/// Drive a stream to completion without an `EventLoop` instance (convenience wrapper).
pub async fn drain_stream(
    stream: impl futures::Stream<Item = StreamEvent> + Unpin,
    headless: bool,
    session: &mut AgentSession,
) -> Result<String, RuntimeError> {
    let event_loop = EventLoop {
        headless,
        ..Default::default()
    };
    let (content, _) = event_loop.consume(stream, session).await?;
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentSession;
    use agtrs_runtime::streaming::StreamEvent;
    use agtrs_runtime::transport::{StopReason, TokenUsage};
    use futures::stream;
    use std::path::PathBuf;

    fn make_session() -> AgentSession {
        AgentSession::new(
            "test",
            PathBuf::from("/tmp"),
            "default".into(),
            "mock".into(),
        )
    }

    fn done_event(content: &str) -> StreamEvent {
        StreamEvent::Done {
            content: content.into(),
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::new(10, 20),
            turns: 2,
            agent_name: "test-agent".into(),
            messages: vec![],
        }
    }

    #[tokio::test]
    async fn consume_done_event_returns_content() {
        let events = stream::iter(vec![done_event("hello from agent")]);
        let el = EventLoop::headless();
        let mut session = make_session();
        let (content, code) = el.consume(events, &mut session).await.unwrap();
        assert_eq!(content, "hello from agent");
        assert!(code.is_success());
        assert!(matches!(session.status, SessionStatus::Completed { .. }));
    }

    #[tokio::test]
    async fn consume_error_event_returns_err() {
        let events = stream::iter(vec![StreamEvent::Error {
            message: "agent crashed".into(),
        }]);
        let el = EventLoop::headless();
        let mut session = make_session();
        let result = el.consume(events, &mut session).await;
        assert!(result.is_err());
        assert!(matches!(session.status, SessionStatus::Failed { .. }));
    }

    #[tokio::test]
    async fn consume_accumulates_text_deltas() {
        let events = stream::iter(vec![
            StreamEvent::TextDelta {
                delta: "hello".into(),
            },
            StreamEvent::TextDelta {
                delta: " world".into(),
            },
            done_event("hello world"),
        ]);
        let el = EventLoop::headless();
        let mut session = make_session();
        let (content, _) = el.consume(events, &mut session).await.unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn consume_updates_session_usage() {
        let events = stream::iter(vec![done_event("ok")]);
        let el = EventLoop::headless();
        let mut session = make_session();
        el.consume(events, &mut session).await.unwrap();
        assert_eq!(session.total_tokens, 30); // 10 input + 20 output
        assert_eq!(session.turn_count, 2);
    }

    #[tokio::test]
    async fn consume_respects_cancellation() {
        let token = CancellationToken::new();
        let token_clone = token.clone();

        // Produce an infinite stream of text deltas
        let events = async_stream::stream! {
            loop {
                yield StreamEvent::TextDelta { delta: "x".into() };
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };

        let el = EventLoop::headless().with_cancel(token_clone);
        let mut session = make_session();

        // Cancel after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            token.cancel();
        });

        let result = el.consume(Box::pin(events), &mut session).await;
        assert!(matches!(result, Err(RuntimeError::Cancelled(_))));
        assert_eq!(session.status, SessionStatus::Cancelled);
    }

    #[tokio::test]
    async fn consume_tool_execution_events_do_not_fail() {
        let events = stream::iter(vec![
            StreamEvent::ToolExecution {
                tool_name: "read_file".into(),
                tool_use_id: "tu-1".into(),
            },
            StreamEvent::ToolResult {
                result: agtrs_runtime::tool::ToolResult::ok("file content", "tu-1"),
            },
            done_event("done"),
        ]);
        let el = EventLoop::headless();
        let mut session = make_session();
        let (content, _) = el.consume(events, &mut session).await.unwrap();
        assert_eq!(content, "done");
    }

    #[tokio::test]
    async fn empty_stream_returns_empty_string() {
        let events = stream::iter(vec![] as Vec<StreamEvent>);
        let el = EventLoop::headless();
        let mut session = make_session();
        let (content, code) = el.consume(events, &mut session).await.unwrap();
        assert_eq!(content, "");
        assert!(code.is_success());
    }
}
