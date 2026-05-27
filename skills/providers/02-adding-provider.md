# Adding a New Provider

## Purpose

Adding a new LLM provider to xaft is a structured process that touches four layers: the provider trait implementation, the provider type enum, the factory construction logic, and the configuration system. Each layer has specific requirements and validation points. This document provides a step-by-step guide so that new providers integrate correctly with the provider chain (FallbackProvider + CostedProvider), appear in the configuration system, and can be tested without real API calls. Following this guide ensures that a new provider has feature parity with existing providers and that the runtime's safety guarantees (cost tracking, retry logic, approval gates) apply to it automatically.

## Mental Model

Think of adding a provider as plugging a new appliance into a standardized outlet. The outlet is the `LlmProvider` trait—it defines the shape of the plug (complete, complete_stream, model_info). The appliance is your new provider—it implements the plug shape for a specific API. The circuit breaker is the `FallbackProvider`—it sits between the outlet and the appliance, protecting against surges (rate limits, server errors). The meter is the `CostedProvider`—it sits outside the circuit breaker, measuring total consumption. The switchboard is the `ProviderFactory`—it routes the user's configuration to the correct outlet. The configuration panel is the `ProviderConfig`—it lets the user specify which appliance to use and how to configure it. You must wire all five components for a new provider to work.

## Extension Patterns

When adding a provider that supports a new streaming protocol (e.g., server-sent events with a different format), implement `complete_stream()` to return a `Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>`. The `FallbackProvider` will wrap this stream in `BufferAndCommit` mode by default, so you don't need to handle buffering yourself. When adding a provider with non-standard authentication (e.g., OAuth2), override the `resolve_api_key()` function in the factory and add the relevant configuration fields to `ProviderConfig`. When adding a provider with model-specific parameters (e.g., Gemini's `thinking_config`), extend the `Request` struct with an optional `provider_specific` field that the provider can deserialize.

## Common Pitfalls

- **Implementing retry logic in the base provider**: The `FallbackProvider` handles all retries. If you add retry logic in your base provider, it will conflict with the fallback layer and may cause double retries or incorrect error classification.
- **Forgetting to implement `model_info()`**: The `model_info()` method returns metadata (context window size, max output tokens, supported features) that the runtime uses for prompt length validation and model selection. Returning a default `ModelInfo` with incorrect values will cause silent truncation or failed requests.
- **Not testing with `for_testing()`**: Every provider must have a `for_testing()` constructor that returns a provider with canned responses. Without this, integration tests for agents and workflows must make real API calls, which is slow, expensive, and flaky.
- **Adding the provider variant but not the factory case**: If you add `ProviderType::Gemini` to the enum but forget to add the corresponding case in `ProviderFactory::build()`, the runtime will panic with an "unimplemented provider type" error when the user tries to use Gemini.
- **Hardcoding API URLs**: Each provider should read its base URL from configuration (with a sensible default). Hardcoding URLs makes it impossible to use proxy servers or alternative endpoints.

## Invariants

1. Every provider must implement the `LlmProvider` trait with all three methods: `complete`, `complete_stream`, and `model_info`.
2. Every provider must have a variant in the `ProviderType` enum and a corresponding case in `ProviderFactory::build()`.
3. Every provider must have a configuration section in `ProviderConfig` with at minimum: `model`, `api_key` (or `api_key_env`), and provider-specific settings.
4. Every provider must provide a `for_testing()` constructor that returns a provider with canned responses for integration testing.
5. Retry logic must not be implemented in the base provider. It must be delegated to `FallbackProvider`.
6. `model_info()` must return accurate metadata for the configured model. Never return a generic default.
7. API base URLs must be configurable (with sensible defaults). Never hardcode API endpoints.

## Examples

```rust
// Step 1: Implement the LlmProvider trait
pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        }
    }

    /// Test constructor with canned responses
    pub fn for_testing() -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: "test-key".to_string(),
            model: "gemini-2.0-flash".to_string(),
            base_url: "http://localhost:0".to_string(), // unreachable
        }
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    async fn complete(&self, request: Request) -> Result<Response, ProviderError> {
        let url = format!("{}/models/{}:generateContent?key={}", self.base_url, self.model, self.api_key);
        let body = self.convert_request(&request);
        let http_response = self.client.post(&url).json(&body).send().await?;
        let response = self.handle_response(http_response).await?;
        Ok(response)
    }

    async fn complete_stream(&self, request: Request) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, ProviderError>> + Send>>, ProviderError> {
        let url = format!("{}/models/{}:streamGenerateContent?key={}&alt=sse", self.base_url, self.model, self.api_key);
        let body = self.convert_request(&request);
        let http_response = self.client.post(&url).json(&body).send().await?;
        Ok(Box::pin(self.parse_sse_stream(http_response)))
    }

    fn model_info(&self) -> ModelInfo {
        match self.model.as_str() {
            "gemini-2.0-flash" => ModelInfo {
                context_window: 1_048_576,
                max_output_tokens: 8192,
                supports_streaming: true,
                supports_tools: true,
            },
            "gemini-2.0-pro" => ModelInfo {
                context_window: 2_097_152,
                max_output_tokens: 8192,
                supports_streaming: true,
                supports_tools: true,
            },
            _ => ModelInfo::default(),
        }
    }
}

// Step 2: Add variant to ProviderType enum
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Anthropic,
    OpenAi,
    Gemini, // NEW
}

impl ProviderType {
    pub fn name(&self) -> &str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
        }
    }
}

// Step 3: Handle in ProviderFactory::build()
impl ProviderFactory {
    pub fn build(config: &ProviderConfig, cost_tracker: Arc<CostTracker>) -> Result<Box<dyn LlmProvider>, ProviderError> {
        let api_key = resolve_api_key(config)?;
        let base: Box<dyn LlmProvider> = match config.provider_type {
            ProviderType::Anthropic => Box::new(AnthropicProvider::new(api_key, config.model.clone())),
            ProviderType::OpenAi => Box::new(OpenAiProvider::new(api_key, config.model.clone())),
            ProviderType::Gemini => Box::new(GeminiProvider::new(api_key, config.model.clone())), // NEW
        };
        let fallback = Box::new(FallbackProvider::new(base).max_retries(3).streaming_mode(StreamingMode::BufferAndCommit));
        let costed = Box::new(CostedProvider::new(fallback, cost_tracker));
        Ok(costed)
    }
}

// Step 4: Add config section
#[derive(Debug, Deserialize)]
pub struct ProviderConfig {
    pub provider_type: ProviderType,
    pub model: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub base_url: Option<String>,       // Optional override
    // Gemini-specific
    pub thinking_config: Option<GeminiThinkingConfig>,
}

#[derive(Debug, Deserialize)]
pub struct GeminiThinkingConfig {
    pub thinking_budget: Option<u32>,
}
```
