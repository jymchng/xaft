# Provider Chain Construction

## Purpose

LLM providers in xaft are not used directly—they are composed into a chain that adds retry logic, cost tracking, and streaming behavior on top of the base API client. This chain architecture separates concerns: the base provider handles API communication, the `FallbackProvider` handles transient failures, and the `CostedProvider` handles financial governance. Without this separation, every provider would need its own retry logic, cost tracking, and streaming implementation, leading to massive code duplication and inconsistent behavior. The chain also ensures that cost tracking is always active (you can't accidentally use a provider without cost tracking) and that retries are always applied (you can't accidentally use a provider without fallback).

## Mental Model

Think of the provider chain as a Russian nesting doll. The innermost doll is the base provider (`AnthropicProvider` or `OpenAiProvider`), which makes the actual HTTP call to the LLM API. The middle doll is `FallbackProvider`, which wraps the base provider and retries on transient errors (rate limits, server errors) using exponential backoff. It also controls streaming behavior via `StreamingMode::BufferAndCommit`, which buffers the stream in memory and commits the full response at once—this is necessary for cost tracking, which needs the complete token count. The outermost doll is `CostedProvider`, which wraps the fallback provider and records input/output tokens and cost after each call. If the cost limit is exceeded, `CostedProvider` rejects the call before it reaches the base provider. The chain is constructed bottom-up but invoked top-down: the outer layer calls the inner layer, which calls the innermost layer.

## Extension Patterns

When adding a new base provider (e.g., `GeminiProvider`), implement the `LlmProvider` trait and ensure it is the first layer in the chain. When adding a new middleware layer (e.g., a caching provider), insert it between `FallbackProvider` and `CostedProvider` so that retries happen before caching (avoiding stale cache entries on transient errors) and cost tracking happens after caching (counting only actual API calls). When modifying retry behavior, update `FallbackProvider`'s configuration rather than adding retry logic to the base provider. When modifying streaming behavior, update `StreamingMode` rather than changing each base provider's streaming implementation.

## Common Pitfalls

- **Wrapping in the wrong order**: If `CostedProvider` is inside `FallbackProvider`, then a retried call is counted twice—once for the failed attempt and once for the successful retry. `CostedProvider` must be the outermost layer so it only counts the final result.
- **Using `StreamingMode::StreamDirectly` with cost tracking**: Direct streaming means tokens arrive one at a time, and the total count isn't known until the stream ends. If cost tracking checks the limit mid-stream, it can't enforce it accurately. Use `StreamingMode::BufferAndCommit` so the full usage is available before cost is recorded.
- **Not configuring the fallback provider**: If `FallbackProvider` is constructed with zero retries, it provides no fallback—the chain is equivalent to the base provider alone. Always configure `max_retries` and `retry_on` appropriately.
- **API key resolution failures**: If the API key is not found through any resolution path, the provider chain will fail at the first HTTP call with a cryptic authentication error. Always validate that the API key exists before constructing the chain.
- **Forgetting to propagate the model info**: Each layer in the chain must delegate `model_info()` to the inner layer. If `CostedProvider::model_info()` returns a default instead of delegating, downstream code gets incorrect model metadata.

## Invariants

1. The provider chain must always be: `CostedProvider → FallbackProvider → BaseProvider`. No other ordering is valid.
2. `FallbackProvider` must retry on rate limit (HTTP 429) and server errors (HTTP 5xx). It must not retry on client errors (HTTP 4xx) other than 429.
3. `StreamingMode::BufferAndCommit` must be the default streaming mode. Direct streaming may only be used when cost tracking is not required.
4. `CostedProvider` must check the cost limit before each call. If the limit is exceeded, the call must be rejected before reaching the inner provider.
5. API key resolution must follow this order: `api_key_env` (environment variable name) → `api_key` (literal value) → `XAFT_<PROVIDER>_API_KEY` → provider-specific vars (e.g., `ANTHROPIC_API_KEY`) → universal fallback `XAFT_API_KEY`.
6. Each layer must delegate `model_info()` to the inner layer. No layer may return a default model info without checking the inner provider.

## Examples

```rust
/// Provider chain construction
pub fn build_provider_chain(config: &ProviderConfig, cost_tracker: Arc<CostTracker>) -> Result<Box<dyn LlmProvider>, ProviderError> {
    // Step 1: Resolve API key
    let api_key = resolve_api_key(config)?;

    // Step 2: Construct base provider
    let base: Box<dyn LlmProvider> = match config.provider_type {
        ProviderType::Anthropic => Box::new(AnthropicProvider::new(api_key, config.model.clone())),
        ProviderType::OpenAi => Box::new(OpenAiProvider::new(api_key, config.model.clone())),
    };

    // Step 3: Wrap in FallbackProvider (retry + streaming mode)
    let fallback = Box::new(FallbackProvider::new(base)
        .max_retries(3)
        .retry_on(vec![RetryCondition::RateLimit, RetryCondition::ServerError])
        .streaming_mode(StreamingMode::BufferAndCommit));

    // Step 4: Wrap in CostedProvider (cost tracking)
    let costed = Box::new(CostedProvider::new(fallback, cost_tracker));

    Ok(costed)
}

/// API key resolution with priority order
fn resolve_api_key(config: &ProviderConfig) -> Result<String, ProviderError> {
    // Priority 1: api_key_env (environment variable name)
    if let Some(env_var) = &config.api_key_env {
        if let Ok(key) = std::env::var(env_var) {
            return Ok(key);
        }
    }

    // Priority 2: api_key (literal value)
    if let Some(key) = &config.api_key {
        return Ok(key.clone());
    }

    // Priority 3: XAFT_<PROVIDER>_API_KEY
    let provider_key = format!("XAFT_{}_API_KEY", config.provider_type.name().to_uppercase());
    if let Ok(key) = std::env::var(&provider_key) {
        return Ok(key);
    }

    // Priority 4: Provider-specific vars
    let specific_key = match config.provider_type {
        ProviderType::Anthropic => "ANTHROPIC_API_KEY",
        ProviderType::OpenAi => "OPENAI_API_KEY",
    };
    if let Ok(key) = std::env::var(specific_key) {
        return Ok(key);
    }

    // Priority 5: Universal fallback
    if let Ok(key) = std::env::var("XAFT_API_KEY") {
        return Ok(key);
    }

    Err(ProviderError::ApiKeyNotFound {
        provider: config.provider_type.name().to_string(),
        tried_vars: vec![
            config.api_key_env.clone().unwrap_or_default(),
            provider_key,
            specific_key.to_string(),
            "XAFT_API_KEY".to_string(),
        ],
    })
}

/// FallbackProvider with retry and streaming
pub struct FallbackProvider {
    inner: Box<dyn LlmProvider>,
    max_retries: u32,
    retry_conditions: Vec<RetryCondition>,
    streaming_mode: StreamingMode,
}

impl FallbackProvider {
    pub fn new(inner: Box<dyn LlmProvider>) -> Self {
        Self {
            inner,
            max_retries: 3,
            retry_conditions: vec![RetryCondition::RateLimit, RetryCondition::ServerError],
            streaming_mode: StreamingMode::BufferAndCommit,
        }
    }

    pub fn max_retries(mut self, retries: u32) -> Self { self.max_retries = retries; self }
    pub fn retry_on(mut self, conditions: Vec<RetryCondition>) -> Self { self.retry_conditions = conditions; self }
    pub fn streaming_mode(mut self, mode: StreamingMode) -> Self { self.streaming_mode = mode; self }
}

/// CostedProvider with pre-call limit check
pub struct CostedProvider {
    inner: Box<dyn LlmProvider>,
    cost_tracker: Arc<CostTracker>,
}

impl CostedProvider {
    pub fn new(inner: Box<dyn LlmProvider>, cost_tracker: Arc<CostTracker>) -> Self {
        cost_tracker.subscribe(); // Subscribe BEFORE any calls
        Self { inner, cost_tracker }
    }
}

#[async_trait]
impl LlmProvider for CostedProvider {
    async fn complete(&self, request: Request) -> Result<Response, ProviderError> {
        // Pre-call limit check
        if self.cost_tracker.is_over_limit() {
            return Err(ProviderError::CostLimitExceeded {
                current: self.cost_tracker.total_cost().await,
                limit: self.cost_tracker.limit(),
            });
        }
        let response = self.inner.complete(request).await?;
        // Post-call cost recording
        self.cost_tracker.record(response.usage.clone()).await;
        Ok(response)
    }

    fn model_info(&self) -> ModelInfo {
        self.inner.model_info() // Delegate to inner
    }
}
```
