# Implementing a Custom LLM Provider

This tutorial covers implementing a custom LLM provider by implementing the `LlmProvider` trait in xaft. You will learn how to define a provider that supports streaming responses, integrate it with the `ProviderFactory` for dynamic instantiation, handle authentication, and map provider-specific errors into xaft's error hierarchy.

---

## The LlmProvider Trait

The `LlmProvider` trait is the abstraction that decouples xaft's agent runtime from any specific LLM API. Every LLM call — whether to OpenAI, Anthropic, a local model, or a custom endpoint — flows through this trait. The trait defines the provider's identity, its supported models, and the methods for invoking the LLM with both streaming and non-streaming modes.

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// The unique identifier for this provider. Used in configuration files
    /// and in the ProviderFactory registry. Must be lowercase and unique.
    fn provider_id(&self) -> &str;

    /// List the model IDs supported by this provider. The agent's model
    /// configuration must reference one of these IDs.
    fn supported_models(&self) -> &[ModelId];

    /// Send a chat completion request and return the full response.
    /// This is the non-streaming path — it waits for the entire response
    /// before returning. Used for short requests where latency is not
    /// critical, and for tool calls that need the complete response
    /// before proceeding.
    async fn complete(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse, LlmError>;

    /// Send a chat completion request and return a stream of response
    /// chunks. This is the streaming path — the caller receives tokens
    /// as they are generated, enabling real-time display in the TUI.
    /// The stream also carries tool-use decisions when the LLM decides
    /// to invoke a tool.
    async fn stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, LlmError>>, LlmError>;

    /// Return the number of tokens used by the given messages.
    /// This is used for cost estimation before making the actual API call.
    /// If the provider does not support token counting, return None.
    async fn count_tokens(
        &self,
        messages: &[ChatMessage],
    ) -> Result<Option<TokenCount>, LlmError>;

    /// Check whether the provider is available and authenticated.
    /// Called at startup to verify configuration before the first request.
    async fn health_check(&self) -> Result<(), LlmError>;
}
```

The `ChatRequest` struct carries the conversation history (as `Vec<ChatMessage>`), the tool definitions (as `Vec<ToolDefinition>`), and generation parameters (temperature, max_tokens, stop sequences). The provider is responsible for mapping these parameters to its API's format. Not all providers support all parameters — for example, a local model might not support tool use. In such cases, the provider should return `LlmError::UnsupportedFeature` with a clear message explaining what is not supported.

The `ChatResponse` struct contains the LLM's reply, which may be a text message, a tool-use request, or both. The `StreamChunk` enum represents incremental updates during streaming: a text delta, a tool-use delta, or a completion signal. The streaming API returns a `BoxStream` (a type-erased async stream) rather than a concrete type, allowing each provider to use its own streaming implementation.

---

## Implementing a Provider

This example implements a provider for a hypothetical "LocalAI" service — a self-hosted LLM endpoint that exposes an OpenAI-compatible API but runs on local hardware. This is a common pattern: many organizations run local models for privacy, cost, or latency reasons, and these models often expose an OpenAI-compatible API for compatibility.

```rust
use xaft_agent::{
    LlmProvider, ChatRequest, ChatResponse, ChatMessage,
    StreamChunk, LlmError, ModelId, TokenCount,
};
use futures::StreamExt;
use async_trait::async_trait;
use reqwest::Client;

pub struct LocalAiProvider {
    base_url: String,
    api_key: Option<String>,
    client: Client,
    models: Vec<ModelId>,
}

impl LocalAiProvider {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            api_key,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("HTTP client construction should not fail"),
            models: vec![
                ModelId::new("local-llama-3-70b"),
                ModelId::new("local-mixtral-8x7b"),
                ModelId::new("local-phi-3-medium"),
            ],
        }
    }

    fn build_request(&self, chat_request: &ChatRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": chat_request.model().as_str(),
            "messages": chat_request.messages().iter().map(|m| match m {
                ChatMessage::System { content } => {
                    serde_json::json!({"role": "system", "content": content})
                }
                ChatMessage::User { content } => {
                    serde_json::json!({"role": "user", "content": content})
                }
                ChatMessage::Assistant { content, tool_calls } => {
                    let mut msg = serde_json::json!({"role": "assistant", "content": content});
                    if let Some(calls) = tool_calls {
                        msg["tool_calls"] = serde_json::to_value(calls).unwrap();
                    }
                    msg
                }
                ChatMessage::Tool { call_id, content } => {
                    serde_json::json!({"role": "tool", "tool_call_id": call_id, "content": content})
                }
            }).collect::<Vec<_>>(),
            "temperature": chat_request.temperature().unwrap_or(0.7),
            "max_tokens": chat_request.max_tokens().unwrap_or(4096),
        });

        // Add tool definitions if present
        if !chat_request.tools().is_empty() {
            body["tools"] = serde_json::to_value(
                chat_request.tools().iter().map(|t| serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.input_schema(),
                    }
                })).collect::<Vec<_>>()
            ).unwrap();
        }

        body
    }
}

#[async_trait]
impl LlmProvider for LocalAiProvider {
    fn provider_id(&self) -> &str {
        "localai"
    }

    fn supported_models(&self) -> &[ModelId] {
        &self.models
    }

    async fn complete(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = self.build_request(&request);

        let mut http_req = self.client.post(&url)
            .json(&body);

        if let Some(ref key) = self.api_key {
            http_req = http_req.header("Authorization", format!("Bearer {}", key));
        }

        let response = http_req.send().await
            .map_err(|e| LlmError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ApiError { status, body });
        }

        let data: serde_json::Value = response.json().await
            .map_err(|e| LlmError::ParseError(e.to_string()))?;

        self.parse_response(data)
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, LlmError>>, LlmError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = self.build_request(&request);
        body["stream"] = serde_json::json!(true);

        let mut http_req = self.client.post(&url)
            .json(&body);

        if let Some(ref key) = self.api_key {
            http_req = http_req.header("Authorization", format!("Bearer {}", key));
        }

        let response = http_req.send().await
            .map_err(|e| LlmError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::ApiError { status, body: body_text });
        }

        // Convert the SSE stream into our StreamChunk type
        let stream = response.bytes_stream()
            .flat_map(|chunk_result| {
                match chunk_result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        // Parse SSE lines: "data: {json}\n\n"
                        text.split("\n\n")
                            .filter_map(|line| {
                                line.strip_prefix("data: ")
                                    .and_then(|json_str| {
                                        if json_str == "[DONE]" {
                                            return Some(Ok(StreamChunk::Done));
                                        }
                                        serde_json::from_str::<serde_json::Value>(json_str)
                                            .ok()
                                            .map(|v| self.parse_stream_chunk(v))
                                    })
                            })
                            .collect::<Vec<_>>()
                    }
                    Err(e) => vec![Err(LlmError::ConnectionFailed(e.to_string()))],
                }
            })
            .boxed();

        Ok(stream)
    }

    async fn count_tokens(
        &self,
        _messages: &[ChatMessage],
    ) -> Result<Option<TokenCount>, LlmError> {
        // LocalAI does not support token counting
        Ok(None)
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        let url = format!("{}/v1/models", self.base_url);
        let mut req = self.client.get(&url);
        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }
        req.send().await
            .map_err(|e| LlmError::ConnectionFailed(e.to_string()))?;
        Ok(())
    }
}
```

---

## Streaming Implementation Details

The streaming path is the most complex part of a provider implementation. LLM APIs typically use Server-Sent Events (SSE) to deliver token increments, and each provider has its own wire format for these events. The OpenAI-compatible format uses `data: {json}\n\n` lines, where each JSON object contains a `choices` array with a `delta` object. The `delta` may contain a text increment, a tool call increment, or nothing (keep-alive).

The `parse_stream_chunk` method maps the provider's wire format to xaft's `StreamChunk` type. This method must handle partial data gracefully — SSE events can be split across TCP packets, and the `bytes_stream()` method delivers chunks that may not align with SSE boundaries. A production implementation should use a buffered parser that accumulates bytes until a complete SSE event is available.

```rust
impl LocalAiProvider {
    fn parse_stream_chunk(&self, data: serde_json::Value) -> Result<StreamChunk, LlmError> {
        let choices = data.get("choices")
            .and_then(|c| c.as_array())
            .ok_or_else(|| LlmError::ParseError("missing choices array".to_string()))?;

        let choice = choices.first()
            .ok_or_else(|| LlmError::ParseError("empty choices array".to_string()))?;

        let delta = choice.get("delta")
            .ok_or_else(|| LlmError::ParseError("missing delta object".to_string()))?;

        // Check for finish reason
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            match reason {
                "stop" => return Ok(StreamChunk::Done),
                "tool_calls" => {
                    // Tool call completed — extract the full tool call
                    if let Some(tool_calls) = delta.get("tool_calls") {
                        return self.parse_tool_call_chunk(tool_calls);
                    }
                    return Ok(StreamChunk::Done);
                }
                _ => {}
            }
        }

        // Text delta
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                return Ok(StreamChunk::TextDelta(content.to_string()));
            }
        }

        // Tool call delta (incremental)
        if let Some(tool_calls) = delta.get("tool_calls") {
            return self.parse_tool_call_chunk(tool_calls);
        }

        // Keep-alive or empty delta
        Ok(StreamChunk::KeepAlive)
    }

    fn parse_tool_call_chunk(&self, tool_calls: &serde_json::Value) -> Result<StreamChunk, LlmError> {
        // Extract tool call ID, function name, and arguments
        let call = tool_calls.as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| LlmError::ParseError("empty tool_calls array".to_string()))?;

        let call_id = call.get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("unknown")
            .to_string();

        let function = call.get("function")
            .ok_or_else(|| LlmError::ParseError("missing function in tool_call".to_string()))?;

        let name = function.get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        let arguments = function.get("arguments")
            .and_then(|a| a.as_str())
            .unwrap_or("{}")
            .to_string();

        Ok(StreamChunk::ToolCallDelta {
            call_id,
            name,
            arguments_delta: arguments,
        })
    }

    fn parse_response(&self, data: serde_json::Value) -> Result<ChatResponse, LlmError> {
        // Parse a non-streaming response into ChatResponse
        let choices = data.get("choices")
            .and_then(|c| c.as_array())
            .ok_or_else(|| LlmError::ParseError("missing choices".to_string()))?;

        let choice = choices.first()
            .ok_or_else(|| LlmError::ParseError("empty choices".to_string()))?;

        let message = choice.get("message")
            .ok_or_else(|| LlmError::ParseError("missing message".to_string()))?;

        let content = message.get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = message.get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter().filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = tc.get("function")?.get("name")?.as_str()?.to_string();
                    let args_str = tc.get("function")?.get("arguments")?.as_str()?;
                    let args: serde_json::Value = serde_json::from_str(args_str).ok()?;
                    Some(ToolCall { id, name, arguments: args })
                }).collect::<Vec<_>>()
            });

        let usage = data.get("usage").and_then(|u| {
            Some(TokenCount {
                input: u.get("prompt_tokens")?.as_u64()? as u64,
                output: u.get("completion_tokens")?.as_u64()? as u64,
            })
        });

        Ok(ChatResponse::new(content, tool_calls, usage))
    }
}
```

---

## Integration with ProviderFactory

The `ProviderFactory` is the registry that maps provider IDs to constructor functions. When the runtime reads the configuration file and encounters a provider ID like `"localai"`, it looks up the corresponding constructor in the factory and invokes it with the provider-specific configuration. This decouples the configuration file format from the provider implementations.

```rust
use xaft_agent::{ProviderFactory, LlmProvider};

fn register_localai(factory: &mut ProviderFactory) {
    factory.register("localai", |config: &serde_json::Value| {
        let base_url = config.get("base_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "localai provider requires 'base_url' config".to_string())?
            .to_string();

        let api_key = config.get("api_key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(Box::new(LocalAiProvider::new(base_url, api_key)) as Box<dyn LlmProvider>)
    });
}

// At startup
let mut factory = ProviderFactory::new();
factory.register_builtin_providers(); // OpenAI, Anthropic, etc.
register_localai(&mut factory);

// The factory is consumed by the runtime builder
let runtime = Runtime::builder()
    .provider_factory(factory)
    .build()
    .await?;
```

The constructor function receives a `&serde_json::Value` containing the provider-specific configuration from the TOML config file. This configuration is the `[providers.localai]` section of the config, parsed as JSON. The constructor is responsible for validating the configuration and returning a ready-to-use provider instance, or an error message explaining what is misconfigured.

```toml
# xaft.toml
[providers.localai]
base_url = "http://localhost:8080"
api_key = "${LOCALAI_API_KEY}"  # environment variable substitution
default_model = "local-llama-3-70b"

[providers.localai.models.local-llama-3-70b]
max_tokens = 8192
context_window = 32768
cost_per_1k_input = 0.0    # free for local models
cost_per_1k_output = 0.0
```

The `ProviderFactory` also supports environment variable substitution in configuration values. The syntax `${VAR_NAME}` is replaced with the value of the environment variable at construction time. This is essential for API keys, which should never be stored in plain text in configuration files. If the environment variable is not set, the constructor receives the literal `${VAR_NAME}` string, which should trigger a validation error.

---

## The CostedProvider Wrapper

xaft includes a `CostedProvider` wrapper that adds cost tracking to any `LlmProvider`. It intercepts `complete()` and `stream()` calls, records the token usage and cost, and publishes `ModelCallComplete` events on the stream sink. You should wrap every provider in `CostedProvider` unless you have a specific reason not to (for example, if the provider already has built-in cost tracking).

```rust
use xaft_agent::{CostedProvider, ModelPricing};

let pricing = ModelPricing::new(
    "local-llama-3-70b",
    0.0,  // cost per 1k input tokens
    0.0,  // cost per 1k output tokens
);

let inner = LocalAiProvider::new(base_url, api_key);
let costed = CostedProvider::new(
    Box::new(inner),
    pricing,
    cost_accumulator.clone(),
);

// Register the costed wrapper instead of the raw provider
factory.register("localai", move |config| {
    let provider = LocalAiProvider::from_config(config)?;
    Ok(Box::new(CostedProvider::new(
        Box::new(provider),
        pricing.clone(),
        cost_accumulator.clone(),
    )))
});
```

The `CostedProvider` implements `LlmProvider` by delegating all calls to the inner provider and then recording the token counts. For streaming calls, it wraps the inner stream in a `CostedStream` that accumulates token counts as chunks arrive and publishes the final total when the stream completes. This means that cost events are emitted after the entire response has been received, not during streaming.

---

## Complete Example

Here is a minimal but complete provider implementation that can be registered with xaft:

```rust
use async_trait::async_trait;
use futures::StreamExt;
use xaft_agent::*;

pub struct EchoProvider;

impl EchoProvider {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl LlmProvider for EchoProvider {
    fn provider_id(&self) -> &str { "echo" }
    fn supported_models(&self) -> &[ModelId] {
        static MODELS: &[ModelId] = &[ModelId::new("echo-v1")];
        MODELS
    }

    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        let last_user_msg = request.messages().iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User { content } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();

        Ok(ChatResponse::new(
            format!("Echo: {}", last_user_msg),
            None,
            Some(TokenCount { input: 10, output: 10 }),
        ))
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, LlmError>>, LlmError> {
        let last_user_msg = request.messages().iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User { content } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let text = format!("Echo: {}", last_user_msg);
        let chunks: Vec<Result<StreamChunk, LlmError>> = text
            .chars()
            .map(|c| Ok(StreamChunk::TextDelta(c.to_string())))
            .chain(std::iter::once(Ok(StreamChunk::Done)))
            .collect();

        Ok(futures::stream::iter(chunks).boxed())
    }

    async fn count_tokens(&self, _messages: &[ChatMessage]) -> Result<Option<TokenCount>, LlmError> {
        Ok(None)
    }

    async fn health_check(&self) -> Result<(), LlmError> { Ok(()) }
}

// Registration
fn main() {
    let mut factory = ProviderFactory::new();
    factory.register("echo", |_config| {
        Ok(Box::new(EchoProvider::new()) as Box<dyn LlmProvider>)
    });
}
```

The `EchoProvider` is a useful testing tool — it echoes the user's last message back as the LLM response, character by character in streaming mode. This allows you to test the entire xaft pipeline (streaming, token display, cost tracking) without making actual LLM API calls. For integration testing, pair it with a mock agent that has a fixed set of tools to verify end-to-end behavior.
