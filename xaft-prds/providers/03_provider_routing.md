# Provider Routing

## Semantic Router

For automatic provider selection based on query characteristics:

```rust
use agtrs_runtime::routing::{SemanticRouter, Route};

let router = SemanticRouter::new(embed_fn, 0.7)
    .add_route(Route::new("simple", "Simple Q&A, status checks, summaries")
        .with_example("what files were modified?")
        .with_example("summarize the current diff"))
    .add_route(Route::new("complex", "Complex reasoning, code generation, multi-step")
        .with_example("implement JWT authentication")
        .with_example("fix the race condition in the pool"))
    .with_fallback("complex");

let matched = router.route(user_query).await?;
let provider = match matched.route_name.as_deref() {
    Some("simple") => cheap_llm.clone(),
    _              => capable_llm.clone(),
};
```

## Provider Fallback Chain

When the primary provider is unavailable:

```rust
pub struct FallbackProvider {
    providers: Vec<Arc<dyn LlmProvider>>,
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    async fn complete(&self, messages: &[Message], options: &LlmOptions) -> Result<LlmResponse, AgtrsError> {
        for provider in &self.providers {
            match provider.complete(messages, options).await {
                Ok(r) => return Ok(r),
                Err(AgtrsError::LlmCallFailed(e)) => {
                    tracing::warn!("provider {} failed: {e}, trying next", provider.provider_name());
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(AgtrsError::msg("all providers failed"))
    }
}
```

## References

- agtrs: `agtrs-runtime/src/routing.rs`
- agtrs example: `agtrs-examples/src/bin/04_provider_router.rs`
