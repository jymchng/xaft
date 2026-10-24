//! Test support shared by the session MCP and subagent integration tests.
//!
//! [`ScriptedLlm`] is agtrs's `MockLlmProvider` with routes and a memory:
//!
//! * every request is **recorded** — its system prompt, the tool names it
//!   offered, and its messages — so a test asserts what a model was actually
//!   shown;
//! * each request is answered from the script of the first **route** whose
//!   key appears in its system prompt (the parent's script otherwise), so a
//!   parent and concurrent subagent workers never race for one FIFO queue;
//! * a step may **compute** its tool input from the conversation so far, so
//!   a script can answer with ids (pool ids, question ids) minted at runtime.
//!
//! Each step is queued on a private `MockTransport` and answered by a
//! `MockLlmProvider`, so completions and streams are agtrs's own.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::{LlmOptions, LlmProvider, LlmResponse, StreamChunk};
use agtrs_runtime::testing::{MockLlmProvider, MockTransport};
use agtrs_runtime::transport::{
    Completion, ContentBlock, Embedding, Message, MessageContent, Role, StopReason, TokenUsage,
    ToolCall,
};
use async_trait::async_trait;
use futures::Stream;
use serde_json::Value;

type InputFn = Box<dyn Fn(&[Message]) -> Value + Send + Sync>;

/// One scripted answer.
pub enum Step {
    /// End the turn with this text.
    Text(String),
    /// Call a tool with this input.
    Tool(String, Value),
    /// Call a tool with an input computed from the messages so far.
    ToolWith(String, InputFn),
}

/// `Step::Text`.
pub fn text(content: &str) -> Step {
    Step::Text(content.into())
}

/// `Step::Tool`.
pub fn tool(name: &str, input: Value) -> Step {
    Step::Tool(name.into(), input)
}

/// `Step::ToolWith`.
pub fn tool_with(name: &str, input: impl Fn(&[Message]) -> Value + Send + Sync + 'static) -> Step {
    Step::ToolWith(name.into(), Box::new(input))
}

/// One request the model received.
#[derive(Debug, Clone)]
pub struct Request {
    /// Which route answered (`""` for the parent).
    pub route: String,
    /// The system prompt (the first system message).
    pub system: String,
    /// Names of the tools offered.
    pub tools: Vec<String>,
    /// Every message, serialised (tool results included).
    pub transcript: String,
}

impl Request {
    /// Whether the tool `name` was offered.
    pub fn offers(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t == name)
    }
}

/// The content of the newest tool result in `messages`, parsed as JSON when
/// it is JSON.
pub fn last_tool_result(messages: &[Message]) -> Value {
    tool_results(messages).pop().unwrap_or(Value::Null)
}

/// Every tool result in `messages`, oldest first, parsed as JSON when it is.
pub fn tool_results(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for message in messages {
        if let MessageContent::MultiPart(blocks) = &message.content {
            for block in blocks {
                if let ContentBlock::ToolResult { content, .. } = block {
                    out.push(
                        serde_json::from_str(content)
                            .unwrap_or_else(|_| Value::String(content.clone())),
                    );
                }
            }
        }
    }
    out
}

/// A routed, recording, scripted provider.
pub struct ScriptedLlm {
    parent: Mutex<VecDeque<Step>>,
    routes: Mutex<Vec<(String, VecDeque<Step>)>>,
    requests: Mutex<Vec<Request>>,
    transport: Arc<MockTransport>,
    inner: MockLlmProvider,
    /// Queue-then-answer is one critical section, so concurrent callers
    /// cannot take each other's queued completion.
    serial: tokio::sync::Mutex<()>,
}

impl ScriptedLlm {
    /// An empty script.
    pub fn new() -> Arc<Self> {
        let transport = Arc::new(MockTransport::new());
        Arc::new(Self {
            parent: Mutex::default(),
            routes: Mutex::default(),
            requests: Mutex::default(),
            inner: MockLlmProvider::new(Arc::clone(&transport)),
            transport,
            serial: tokio::sync::Mutex::new(()),
        })
    }

    /// Append steps to the parent's script.
    pub fn parent(self: &Arc<Self>, steps: impl IntoIterator<Item = Step>) -> Arc<Self> {
        self.parent.lock().unwrap().extend(steps);
        Arc::clone(self)
    }

    /// Answer requests whose system prompt contains `key` from `steps`.
    pub fn route(self: &Arc<Self>, key: &str, steps: impl IntoIterator<Item = Step>) -> Arc<Self> {
        self.routes
            .lock()
            .unwrap()
            .push((key.to_string(), steps.into_iter().collect()));
        Arc::clone(self)
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The requests one route answered.
    pub fn requests_for(&self, route: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.route == route)
            .collect()
    }

    fn next(&self, messages: &[Message], options: &LlmOptions) -> Result<Completion, AgtrsError> {
        let system = messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.text())
            .unwrap_or_default();
        let (route, step) = {
            let mut routes = self.routes.lock().unwrap();
            match routes
                .iter_mut()
                .find(|(key, _)| system.contains(key.as_str()))
            {
                Some((key, steps)) => (key.clone(), steps.pop_front()),
                None => (String::new(), self.parent.lock().unwrap().pop_front()),
            }
        };
        self.requests.lock().unwrap().push(Request {
            route: route.clone(),
            system,
            tools: options.tools.iter().map(|t| t.name.clone()).collect(),
            transcript: serde_json::to_string(messages).unwrap_or_default(),
        });
        let step = step.ok_or_else(|| AgtrsError::LlmCallFailed {
            reason: format!("script for route '{route}' is exhausted"),
        })?;
        let (content, tool_calls, stop_reason) = match step {
            Step::Text(text) => (text, Vec::new(), StopReason::EndTurn),
            Step::Tool(name, input) => {
                (String::new(), vec![call(name, input)], StopReason::ToolUse)
            }
            Step::ToolWith(name, input) => (
                String::new(),
                vec![call(name, input(messages))],
                StopReason::ToolUse,
            ),
        };
        Ok(Completion {
            id: uuid::Uuid::new_v4().to_string(),
            model: "mock".into(),
            content,
            tool_calls,
            stop_reason,
            usage: TokenUsage::new(10, 20),
            thinking_blocks: Vec::new(),
        })
    }
}

fn call(name: String, input: Value) -> ToolCall {
    ToolCall {
        tool_use_id: uuid::Uuid::new_v4().to_string(),
        name,
        input,
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn complete(
        &self,
        messages: &[Message],
        options: &LlmOptions,
    ) -> Result<LlmResponse, AgtrsError> {
        let completion = self.next(messages, options)?;
        let _serial = self.serial.lock().await;
        self.transport.queue_response(completion).await;
        self.inner.complete(messages, options).await
    }

    async fn stream(
        &self,
        messages: &[Message],
        options: &LlmOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, AgtrsError>> + Send>>, AgtrsError>
    {
        let completion = self.next(messages, options)?;
        let _serial = self.serial.lock().await;
        self.transport.queue_response(completion).await;
        self.inner.stream(messages, options).await
    }

    async fn embed(
        &self,
        inputs: &[String],
        model: Option<&str>,
    ) -> Result<Vec<Embedding>, AgtrsError> {
        self.inner.embed(inputs, model).await
    }

    async fn count_tokens(&self, messages: &[Message]) -> Result<usize, AgtrsError> {
        self.inner.count_tokens(messages).await
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn supports_tool_calling(&self) -> bool {
        self.inner.supports_tool_calling()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_structured_output(&self) -> bool {
        self.inner.supports_structured_output()
    }

    fn supports_thinking(&self) -> bool {
        self.inner.supports_thinking()
    }

    fn supports_vision(&self) -> bool {
        self.inner.supports_vision()
    }

    fn max_output_tokens(&self) -> u32 {
        self.inner.max_output_tokens()
    }

    fn provider_name(&self) -> &str {
        self.inner.provider_name()
    }
}
