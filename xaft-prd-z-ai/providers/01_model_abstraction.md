# Model Abstraction: LlmProvider Trait

> Deep dive into the `LlmProvider` trait: all methods, capability flags,
> provider implementations (OpenAI, Anthropic, Ollama, Gemini), context
> window tracking, model constants, feature detection, and provider-specific
> quirks.

---

## 1. Overview

The `LlmProvider` trait is xauft's abstraction over LLM APIs. Every model
interaction flows through this trait, enabling xauft to swap providers,
compose them (fallback, routing, consensus), and query their capabilities
before dispatching.

```
┌─────────────────────────────────────────────────────────────┐
│                    xauft Provider Stack                      │
│                                                             │
│  ┌──────────────┐                                           │
│  │ CostedProvider│─── route by cost predicate               │
│  └──────┬───────┘                                           │
│         │                                                   │
│  ┌──────▼───────┐                                           │
│  │FallbackProvider│── try primary, fallback on error        │
│  └──────┬───────┘                                           │
│         │                                                   │
│  ┌──────▼───────────────────────────────────────────────┐   │
│  │              LlmProvider Trait                       │   │
│  │                                                      │   │
│  │  ┌────────┐ ┌──────────┐ ┌───────┐ ┌────────┐      │   │
│  │  │OpenAI  │ │Anthropic │ │Ollama │ │ Gemini │      │   │
│  │  │Provider│ │Provider  │ │Provider│ │Provider│      │   │
│  │  └────────┘ └──────────┘ └───────┘ └────────┘      │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. LlmProvider Trait

### 2.1 Full Trait Definition

```rust
/// Core trait for LLM provider abstraction.
/// All model interactions in xauft flow through this trait.
#[async_trait]
pub trait LlmProvider: Send + Sync + 'static {
    /// Unique identifier for this provider instance.
    fn id(&self) -> &str;

    /// List available models from this provider.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Generate a completion for the given request.
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError>;

    /// Generate a streaming completion.
    async fn complete_stream(
        &self,
        request: LlmRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>;

    /// Count tokens in the given text using the provider's tokenizer.
    /// Returns None if the provider doesn't support token counting.
    async fn count_tokens(&self, text: &str, model: &str) -> Option<usize>;

    /// Query the capabilities of a specific model.
    fn model_capabilities(&self, model: &str) -> ModelCapabilities;

    /// Get the context window size for a model.
    fn context_window(&self, model: &str) -> usize;

    /// Check if this provider supports a specific feature.
    fn supports_feature(&self, model: &str, feature: ProviderFeature) -> bool;

    /// Estimate the cost of a request (before execution).
    fn estimate_cost(&self, request: &LlmRequest) -> CostEstimate;

    /// Get the provider type (for logging/routing).
    fn provider_type(&self) -> ProviderType;
}
```

### 2.2 Request and Response Types

```rust
/// A request to an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Model to use (e.g., "gpt-4o", "claude-3-5-sonnet-20241022").
    pub model: String,

    /// Conversation messages.
    pub messages: Vec<Message>,

    /// System prompt (separate from messages for providers that require it).
    pub system_prompt: Option<String>,

    /// Sampling temperature.
    pub temperature: Option<f64>,

    /// Maximum tokens to generate.
    pub max_tokens: Option<usize>,

    /// Top-p (nucleus sampling).
    pub top_p: Option<f64>,

    /// Stop sequences.
    pub stop: Vec<String>,

    /// Tool definitions (for tool calling).
    pub tools: Vec<ToolDefinition>,

    /// Whether to force a tool call.
    pub tool_choice: Option<ToolChoice>,

    /// Response format (for structured output).
    pub response_format: Option<ResponseFormat>,

    /// Seed for deterministic sampling.
    pub seed: Option<u64>,

    /// Provider-specific metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// A response from an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// The generated content.
    pub content: String,

    /// Tool calls made by the model.
    pub tool_calls: Vec<ToolCall>,

    /// Reason the model stopped generating.
    pub finish_reason: Option<FinishReason>,

    /// Token usage statistics.
    pub usage: TokenUsage,

    /// Model that actually generated this response (may differ from request
    /// if the provider auto-routed).
    pub model: String,

    /// Provider-specific metadata.
    pub metadata: HashMap<String, serde_json::Value>,

    /// Unique ID for this completion (for logging).
    pub completion_id: String,

    /// Timestamp.
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Default for TokenUsage {
    fn default() -> Self {
        Self { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Specific(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { schema: serde_json::Value },
}
```

---

## 3. Capability Flags

### 3.1 ModelCapabilities

```rust
/// Capabilities that a specific model supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Whether the model supports tool/function calling.
    pub tool_calling: bool,

    /// Whether the model supports streaming output.
    pub streaming: bool,

    /// Whether the model supports JSON structured output.
    pub structured_output: bool,

    /// Whether the model supports "thinking" / chain-of-thought.
    pub thinking: bool,

    /// Whether the model supports vision (image input).
    pub vision: bool,

    /// Whether the model supports audio input.
    pub audio_input: bool,

    /// Whether the model supports embeddings.
    pub embeddings: bool,

    /// Maximum context window in tokens.
    pub max_context_tokens: usize,

    /// Maximum output tokens.
    pub max_output_tokens: usize,

    /// Supported response formats.
    pub supported_formats: Vec<ResponseFormat>,

    /// Cost per million tokens (input).
    pub cost_per_million_input: f64,

    /// Cost per million tokens (output).
    pub cost_per_million_output: f64,

    /// Whether the model supports parallel tool calls.
    pub parallel_tool_calls: bool,

    /// Whether the model supports system messages.
    pub system_messages: bool,

    /// Maximum number of tools per request.
    pub max_tools: Option<usize>,

    /// Whether the model supports seed for deterministic output.
    pub deterministic: bool,
}
```

### 3.2 Feature Detection

xauft queries capabilities **before dispatching** to avoid sending
unsupported features:

```rust
impl LlmRequest {
    /// Validate this request against a model's capabilities.
    pub fn validate_against(&self, caps: &ModelCapabilities) -> Result<(), CapabilityError> {
        if !self.tools.is_empty() && !caps.tool_calling {
            return Err(CapabilityError::Unsupported {
                feature: "tool_calling".into(),
                model: self.model.clone(),
            });
        }

        if let Some(ResponseFormat::JsonObject) = &self.response_format {
            if !caps.structured_output {
                return Err(CapabilityError::Unsupported {
                    feature: "structured_output (JSON)".into(),
                    model: self.model.clone(),
                });
            }
        }

        if let Some(ResponseFormat::JsonSchema { .. }) = &self.response_format {
            if !caps.structured_output {
                return Err(CapabilityError::Unsupported {
                    feature: "structured_output (schema)".into(),
                    model: self.model.clone(),
                });
            }
        }

        if self.messages.iter().any(|m| m.has_images()) && !caps.vision {
            return Err(CapabilityError::Unsupported {
                feature: "vision".into(),
                model: self.model.clone(),
            });
        }

        if self.tools.len() > caps.max_tools.unwrap_or(usize::MAX) {
            return Err(CapabilityError::ToolLimitExceeded {
                count: self.tools.len(),
                max: caps.max_tools.unwrap(),
            });
        }

        // Estimate total tokens and check against context window
        let estimated_tokens = self.estimate_tokens();
        if estimated_tokens > caps.max_context_tokens {
            return Err(CapabilityError::ContextWindowExceeded {
                estimated: estimated_tokens,
                max: caps.max_context_tokens,
            });
        }

        Ok(())
    }
}
```

---

## 4. Provider Implementations

### 4.1 OpenAI Provider

```rust
pub struct OpenAIProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    organization: Option<String>,
    models: HashMap<String, ModelCapabilities>,
}

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        let models = Self::build_model_registry();
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".into(),
            organization: None,
            models,
        }
    }

    fn build_model_registry() -> HashMap<String, ModelCapabilities> {
        let mut models = HashMap::new();

        models.insert("gpt-4o".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: true,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 128_000,
            max_output_tokens: 16_384,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject, ResponseFormat::JsonSchema { schema: serde_json::Value::Null }],
            cost_per_million_input: 2.50,
            cost_per_million_output: 10.00,
            parallel_tool_calls: true,
            system_messages: true,
            max_tools: Some(128),
            deterministic: true,
        });

        models.insert("gpt-4o-mini".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: true,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 128_000,
            max_output_tokens: 16_384,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 0.15,
            cost_per_million_output: 0.60,
            parallel_tool_calls: true,
            system_messages: true,
            max_tools: Some(128),
            deterministic: false,
        });

        models.insert("o1".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: true,
            vision: true,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 200_000,
            max_output_tokens: 100_000,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 15.00,
            cost_per_million_output: 60.00,
            parallel_tool_calls: false,
            system_messages: true,
            max_tools: Some(128),
            deterministic: false,
        });

        models
    }
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
    fn id(&self) -> &str { "openai" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let caps = self.model_capabilities(&request.model);

        // Validate request against capabilities
        request.validate_against(&caps)?;

        // Build OpenAI API request body
        let mut body = serde_json::Map::new();
        body.insert("model".into(), serde_json::Value::String(request.model));
        body.insert("messages".into(), self.convert_messages(&request.messages, &request.system_prompt)?);

        if let Some(temp) = request.temperature {
            body.insert("temperature".into(), serde_json::Value::Number(
                Number::from_f64(temp).ok_or(ProviderError::InvalidParameter("temperature"))?
            ));
        }
        if let Some(max_tokens) = request.max_tokens {
            body.insert("max_tokens".into(), serde_json::Value::Number(max_tokens.into()));
        }
        if !request.tools.is_empty() {
            body.insert("tools".into(), self.convert_tools(&request.tools)?);
            if let Some(choice) = &request.tool_choice {
                body.insert("tool_choice".into(), self.convert_tool_choice(choice));
            }
        }
        if let Some(format) = &request.response_format {
            body.insert("response_format".into(), self.convert_response_format(format));
        }

        let response = self.client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError { status: status.as_u16(), body });
        }

        let completion: OpenAICompletion = response.json().await?;
        self.convert_response(completion)
    }

    fn model_capabilities(&self, model: &str) -> ModelCapabilities {
        // Check exact match first
        if let Some(caps) = self.models.get(model) {
            return caps.clone();
        }
        // Check prefix match (e.g., "gpt-4o-2024-05-13" → "gpt-4o")
        for (key, caps) in &self.models {
            if model.starts_with(key) {
                return caps.clone();
            }
        }
        // Default conservative capabilities
        ModelCapabilities::conservative()
    }

    fn context_window(&self, model: &str) -> usize {
        self.model_capabilities(model).max_context_tokens
    }

    fn provider_type(&self) -> ProviderType { ProviderType::OpenAI }
}
```

### 4.2 Anthropic Provider

**Quirk: System message placement.** Anthropic requires the system prompt as
a top-level field, not as a message in the messages array.

```rust
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    models: HashMap<String, ModelCapabilities>,
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str { "anthropic" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let caps = self.model_capabilities(&request.model);
        request.validate_against(&caps)?;

        let mut body = serde_json::Map::new();
        body.insert("model".into(), serde_json::Value::String(request.model));
        body.insert("max_tokens".into(), serde_json::Value::Number(
            (request.max_tokens.unwrap_or(4096)).into()
        ));

        // ⚠️ QUIRK: System prompt goes in top-level "system" field
        if let Some(system) = &request.system_prompt {
            body.insert("system".into(), serde_json::Value::String(system.clone()));
        } else {
            // Extract system messages from the messages array
            let (system_msgs, other_msgs): (Vec<_>, Vec<_>) = request.messages.iter()
                .partition(|m| m.role() == Role::System);

            if !system_msgs.is_empty() {
                let system_text = system_msgs.iter()
                    .map(|m| m.content())
                    .collect::<Vec<_>>()
                    .join("\n");
                body.insert("system".into(), serde_json::Value::String(system_text));
            }

            body.insert("messages".into(), self.convert_messages(&other_msgs)?);
        }

        if let Some(temp) = request.temperature {
            body.insert("temperature".into(), serde_json::Value::Number(
                Number::from_f64(temp).unwrap()
            ));
        }

        if !request.tools.is_empty() {
            body.insert("tools".into(), self.convert_tools_anthropic(&request.tools)?);
            if let Some(choice) = &request.tool_choice {
                body.insert("tool_choice".into(), self.convert_tool_choice_anthropic(choice));
            }
        }

        let response = self.client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::ApiError { status: status.as_u16(), body });
        }

        let completion: AnthropicMessage = response.json().await?;
        self.convert_response_anthropic(completion)
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Anthropic }
}

impl AnthropicProvider {
    fn build_model_registry() -> HashMap<String, ModelCapabilities> {
        let mut models = HashMap::new();

        models.insert("claude-3-5-sonnet-20241022".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: false, // Uses tool-calling workaround
            thinking: false,
            vision: true,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 200_000,
            max_output_tokens: 8_192,
            supported_formats: vec![ResponseFormat::Text],
            cost_per_million_input: 3.00,
            cost_per_million_output: 15.00,
            parallel_tool_calls: false, // ⚠️ Anthropic doesn't guarantee parallel
            system_messages: true,      // via top-level field only
            max_tools: Some(128),
            deterministic: false,
        });

        models.insert("claude-3-5-haiku-20241022".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: false,
            thinking: false,
            vision: true,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 200_000,
            max_output_tokens: 8_192,
            supported_formats: vec![ResponseFormat::Text],
            cost_per_million_input: 0.80,
            cost_per_million_output: 4.00,
            parallel_tool_calls: false,
            system_messages: true,
            max_tools: Some(128),
            deterministic: false,
        });

        models
    }
}
```

### 4.3 Ollama Provider

**Quirk: `tool_use_id` generation.** Ollama may not return consistent
`tool_use_id` values, requiring xauft to generate them.

```rust
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    models: HashMap<String, ModelCapabilities>,
    /// Whether Ollama is available (checked on startup).
    available: AtomicBool,
}

impl OllamaProvider {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            models: Self::build_model_registry(),
            available: AtomicBool::new(false),
        }
    }

    /// Check if Ollama is running and available.
    pub async fn check_availability(&self) -> bool {
        let result = self.client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await;
        let available = result.is_ok();
        self.available.store(available, Ordering::Relaxed);
        available
    }

    fn build_model_registry() -> HashMap<String, ModelCapabilities> {
        // Ollama models have variable capabilities depending on the model.
        // Conservative defaults are used; actual capabilities are probed.
        let mut models = HashMap::new();

        models.insert("llama3.1".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: false,
            audio_input: false,
            embeddings: true,
            max_context_tokens: 128_000,
            max_output_tokens: 4_096,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 0.0,    // local = free
            cost_per_million_output: 0.0,
            parallel_tool_calls: false,      // ⚠️ Ollama doesn't guarantee
            system_messages: true,
            max_tools: Some(64),
            deterministic: false,
        });

        models.insert("qwen2.5-coder".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: false,
            audio_input: false,
            embeddings: false,
            max_context_tokens: 128_000,
            max_output_tokens: 8_192,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 0.0,
            cost_per_million_output: 0.0,
            parallel_tool_calls: false,
            system_messages: true,
            max_tools: Some(64),
            deterministic: false,
        });

        models
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn id(&self) -> &str { "ollama" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        if !self.available.load(Ordering::Relaxed) {
            return Err(ProviderError::ProviderUnavailable("Ollama".into()));
        }

        let mut body = serde_json::Map::new();
        body.insert("model".into(), serde_json::Value::String(request.model));
        body.insert("stream".into(), serde_json::Value::Bool(false));

        let messages = self.convert_messages_ollama(&request.messages, &request.system_prompt)?;
        body.insert("messages".into(), messages);

        if let Some(format) = &request.response_format {
            body.insert("format".into(), self.convert_format_ollama(format));
        }

        if !request.tools.is_empty() {
            body.insert("tools".into(), self.convert_tools_ollama(&request.tools)?);
        }

        let response = self.client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::ApiError {
                status: response.status().as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let completion: OllamaChatResponse = response.json().await?;
        self.convert_response_ollama(completion)
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Ollama }
}

impl OllamaProvider {
    /// Convert Ollama response, handling the tool_use_id quirk.
    fn convert_response_ollama(
        &self,
        completion: OllamaChatResponse,
    ) -> Result<LlmResponse, ProviderError> {
        let mut tool_calls = Vec::new();
        if let Some(calls) = completion.message.tool_calls {
            for (i, call) in calls.into_iter().enumerate() {
                // ⚠️ QUIRK: Ollama may return empty or null tool_use_id
                let id = if call.id.is_empty() || call.id == "null" {
                    format!("ollama_tool_{}", i)  // Generate deterministic ID
                } else {
                    call.id
                };
                tool_calls.push(ToolCall {
                    id,
                    name: call.function.name,
                    input: call.function.arguments,
                });
            }
        }

        Ok(LlmResponse {
            content: completion.message.content,
            tool_calls,
            finish_reason: match completion.done_reason.as_deref() {
                Some("stop") => Some(FinishReason::Stop),
                Some("tool_calls") => Some(FinishReason::ToolCall),
                Some("length") => Some(FinishReason::Length),
                _ => None,
            },
            usage: TokenUsage {
                prompt_tokens: completion.prompt_eval_count.unwrap_or(0) as u64,
                completion_tokens: completion.eval_count.unwrap_or(0) as u64,
                total_tokens: (completion.prompt_eval_count.unwrap_or(0)
                    + completion.eval_count.unwrap_or(0)) as u64,
            },
            model: completion.model,
            metadata: HashMap::new(),
            completion_id: Uuid::new_v4().to_string(),
            created_at: Utc::now(),
        })
    }
}
```

### 4.4 Gemini Provider

**Quirk: Grounding** — Gemini supports search grounding which can be
leveraged for code search. Also, Gemini uses a different function calling
schema format.

```rust
pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    models: HashMap<String, ModelCapabilities>,
}

impl GeminiProvider {
    fn build_model_registry() -> HashMap<String, ModelCapabilities> {
        let mut models = HashMap::new();

        models.insert("gemini-2.0-flash".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: true,
            audio_input: true,
            embeddings: true,
            max_context_tokens: 1_048_576,    // 1M tokens
            max_output_tokens: 8_192,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 0.10,      // Flash pricing
            cost_per_million_output: 0.40,
            parallel_tool_calls: true,
            system_messages: true,
            max_tools: Some(64),
            deterministic: false,
        });

        models.insert("gemini-1.5-pro".into(), ModelCapabilities {
            tool_calling: true,
            streaming: true,
            structured_output: true,
            thinking: false,
            vision: true,
            audio_input: true,
            embeddings: true,
            max_context_tokens: 2_097_152,    // 2M tokens
            max_output_tokens: 8_192,
            supported_formats: vec![ResponseFormat::Text, ResponseFormat::JsonObject],
            cost_per_million_input: 1.25,
            cost_per_million_output: 5.00,
            parallel_tool_calls: true,
            system_messages: true,
            max_tools: Some(64),
            deterministic: false,
        });

        models
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn id(&self) -> &str { "gemini" }

    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let caps = self.model_capabilities(&request.model);
        request.validate_against(&caps)?;

        // Gemini uses a different API structure
        let mut body = serde_json::Map::new();

        // Convert messages to Gemini format
        let contents = self.convert_messages_gemini(&request.messages)?;
        body.insert("contents".into(), contents);

        // System instruction (separate field, like Anthropic)
        if let Some(system) = &request.system_prompt {
            let mut system_config = serde_json::Map::new();
            system_config.insert("parts".into(), serde_json::json!([
                { "text": system }
            ]));
            body.insert("systemInstruction".into(), system_config.into());
        }

        // Generation config
        let mut gen_config = serde_json::Map::new();
        if let Some(temp) = request.temperature {
            gen_config.insert("temperature".into(), serde_json::Value::Number(
                Number::from_f64(temp).unwrap()
            ));
        }
        if let Some(max_tokens) = request.max_tokens {
            gen_config.insert("maxOutputTokens".into(), serde_json::Value::Number(max_tokens.into()));
        }
        if !request.stop.is_empty() {
            gen_config.insert("stopSequences".into(),
                serde_json::Value::Array(request.stop.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect()));
        }
        if let Some(format) = &request.response_format {
            gen_config.insert("responseMimeType".into(),
                self.convert_mime_type_gemini(format));
        }
        body.insert("generationConfig".into(), gen_config.into());

        // Tool definitions (Gemini function calling)
        if !request.tools.is_empty() {
            let function_declarations = self.convert_tools_gemini(&request.tools)?;
            body.insert("tools".into(), serde_json::json!([
                { "functionDeclarations": function_declarations }
            ]));
        }

        // ⚠️ QUIRK: Gemini grounding
        // Enable Google Search grounding if configured
        if request.metadata.get("grounding").and_then(|v| v.as_bool()).unwrap_or(false) {
            body.insert("tools".into(), serde_json::json!([
                { "googleSearchRetrieval": {} }
            ]));
        }

        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url, request.model, self.api_key
        );

        let response = self.client
            .post(&url)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::ApiError {
                status: response.status().as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let completion: GeminiResponse = response.json().await?;
        self.convert_response_gemini(completion)
    }

    fn provider_type(&self) -> ProviderType { ProviderType::Gemini }
}
```

---

## 5. Context Window Tracking

### 5.1 Token Counting

```rust
pub enum TokenCounter {
    /// Exact counting via the provider's API.
    Provider(Arc<dyn LlmProvider>),
    /// Heuristic estimation (4 chars ≈ 1 token for English, varies for code).
    Heuristic(TokenEstimator),
}

impl TokenCounter {
    pub async fn count(&self, text: &str, model: &str) -> usize {
        match self {
            Self::Provider(provider) => {
                provider.count_tokens(text, model).await
                    .unwrap_or_else(|| self.heuristic_count(text))
            }
            Self::Heuristic(estimator) => estimator.estimate(text),
        }
    }

    fn heuristic_count(&self, text: &str) -> usize {
        let char_count = text.len() as f64;
        // Rough heuristic: 4 chars per token for English,
        // 3 chars per token for code
        (char_count / 3.5).ceil() as usize
    }
}
```

### 5.2 Context Window Monitor

```rust
pub struct ContextWindowMonitor {
    /// Token counter.
    counter: TokenCounter,
    /// Model being used.
    model: String,
    /// Maximum context tokens for this model.
    max_tokens: usize,
    /// Reserved tokens for output.
    output_reserve: usize,
    /// Current token usage.
    current_usage: AtomicUsize,
}

impl ContextWindowMonitor {
    /// Check if a message can be added without exceeding the context window.
    pub async fn can_add_message(&self, message: &Message) -> bool {
        let msg_tokens = self.counter.count(&message.content(), &self.model).await;
        let current = self.current_usage.load(Ordering::Relaxed);
        let available = self.max_tokens.saturating_sub(self.output_reserve);
        current + msg_tokens <= available
    }

    /// Add a message's tokens to the tracker.
    pub async fn add_message(&self, message: &Message) -> usize {
        let tokens = self.counter.count(&message.content(), &self.model).await;
        self.current_usage.fetch_add(tokens, Ordering::Relaxed);
        tokens
    }

    /// Get remaining tokens in the context window.
    pub fn remaining(&self) -> usize {
        let current = self.current_usage.load(Ordering::Relaxed);
        self.max_tokens.saturating_sub(current).saturating_sub(self.output_reserve)
    }

    /// Get usage as a percentage of the context window.
    pub fn usage_percent(&self) -> f64 {
        let current = self.current_usage.load(Ordering::Relaxed) as f64;
        (current / self.max_tokens as f64) * 100.0
    }
}
```

---

## 6. Model Constants

### 6.1 Model Reference Table

| Provider  | Model                    | Context  | Output  | Input $/M | Output $/M | Tools | Vision | Streaming |
|-----------|--------------------------|---------:|--------:|----------:|-----------:|:-----:|:------:|:---------:|
| OpenAI    | gpt-4o                   | 128K     | 16K     | $2.50     | $10.00     | ✅    | ✅     | ✅        |
| OpenAI    | gpt-4o-mini              | 128K     | 16K     | $0.15     | $0.60      | ✅    | ✅     | ✅        |
| OpenAI    | o1                       | 200K     | 100K    | $15.00    | $60.00     | ✅    | ✅     | ✅        |
| OpenAI    | o1-mini                  | 128K     | 65K     | $3.00     | $12.00     | ✅    | ❌     | ✅        |
| Anthropic | claude-3.5-sonnet        | 200K     | 8K      | $3.00     | $15.00     | ✅    | ✅     | ✅        |
| Anthropic | claude-3.5-haiku         | 200K     | 8K      | $0.80     | $4.00      | ✅    | ✅     | ✅        |
| Anthropic | claude-3-opus            | 200K     | 4K      | $15.00    | $75.00     | ✅    | ✅     | ✅        |
| Ollama    | llama3.1                 | 128K     | 4K      | $0.00     | $0.00      | ✅    | ❌     | ✅        |
| Ollama    | qwen2.5-coder            | 128K     | 8K      | $0.00     | $0.00      | ✅    | ❌     | ✅        |
| Ollama    | codellama                | 16K      | 4K      | $0.00     | $0.00      | ❌    | ❌     | ✅        |
| Gemini    | gemini-2.0-flash         | 1M       | 8K      | $0.10     | $0.40      | ✅    | ✅     | ✅        |
| Gemini    | gemini-1.5-pro           | 2M       | 8K      | $1.25     | $5.00      | ✅    | ✅     | ✅        |

### 6.2 Cost Constants

```rust
pub mod model_costs {
    pub const GPT4O_INPUT_PER_MILLION: f64 = 2.50;
    pub const GPT4O_OUTPUT_PER_MILLION: f64 = 10.00;
    pub const GPT4O_MINI_INPUT_PER_MILLION: f64 = 0.15;
    pub const GPT4O_MINI_OUTPUT_PER_MILLION: f64 = 0.60;
    pub const CLAUDE35_SONNET_INPUT_PER_MILLION: f64 = 3.00;
    pub const CLAUDE35_SONNET_OUTPUT_PER_MILLION: f64 = 15.00;
    pub const CLAUDE35_HAIKU_INPUT_PER_MILLION: f64 = 0.80;
    pub const CLAUDE35_HAIKU_OUTPUT_PER_MILLION: f64 = 4.00;
    pub const OLLAMA_INPUT_PER_MILLION: f64 = 0.00;
    pub const OLLAMA_OUTPUT_PER_MILLION: f64 = 0.00;
    pub const GEMINI_FLASH_INPUT_PER_MILLION: f64 = 0.10;
    pub const GEMINI_FLASH_OUTPUT_PER_MILLION: f64 = 0.40;
    pub const GEMINI_PRO_INPUT_PER_MILLION: f64 = 1.25;
    pub const GEMINI_PRO_OUTPUT_PER_MILLION: f64 = 5.00;
}
```

---

## 7. Provider-Specific Quirk Summary

| Quirk                            | Provider  | Mitigation                                    |
|----------------------------------|-----------|-----------------------------------------------|
| System message in top-level field | Anthropic | Extract system messages from array → top-level |
| No structured output natively    | Anthropic | Use tool-calling workaround                   |
| Inconsistent tool_use_id         | Ollama    | Generate deterministic IDs if missing         |
| No parallel tool calls           | Anthropic | Serialize tool calls within a single response |
| Different function calling schema| Gemini    | Convert ToolDefinition → functionDeclarations |
| Grounding tool                   | Gemini    | Special googleSearchRetrieval tool config     |
| Rate limits vary by tier         | OpenAI    | Exponential backoff with tier-aware limits    |
| Context overflow behavior        | All       | FallbackProvider + context truncation         |
| Streaming chunk format differs   | All       | Normalize to StreamChunk in provider impl     |
| Cost calculation differs         | All       | Provider-specific estimate_cost()             |

---

## 8. Configuration Reference

```toml
[xaft.providers]

[xaft.providers.openai]
api_key = "${OPENAI_API_KEY}"
base_url = "https://api.openai.com/v1"
organization = ""
default_model = "gpt-4o"
max_retries = 3
timeout_secs = 120

[xaft.providers.anthropic]
api_key = "${ANTHROPIC_API_KEY}"
base_url = "https://api.anthropic.com"
default_model = "claude-3-5-sonnet-20241022"
max_retries = 3
timeout_secs = 120

[xaft.providers.ollama]
base_url = "http://localhost:11434"
default_model = "qwen2.5-coder"
auto_detect_models = true
timeout_secs = 300              # local inference can be slow

[xaft.providers.gemini]
api_key = "${GOOGLE_API_KEY}"
base_url = "https://generativelanguage.googleapis.com"
default_model = "gemini-2.0-flash"
grounding_enabled = false
timeout_secs = 120
```
