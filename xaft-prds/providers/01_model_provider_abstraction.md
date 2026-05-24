# Model Provider Abstraction

## LlmProvider Trait

All model interactions go through the `LlmProvider` trait from `agtrs-runtime`:

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, messages: &[Message], options: &LlmOptions) -> Result<LlmResponse, AgtrsError>;
    async fn stream(&self, messages: &[Message], options: &LlmOptions)
        -> Result<Pin<Box<dyn Stream<Item = Result<CompletionChunk, AgtrsError>> + Send>>, AgtrsError>;
    async fn embed(&self, texts: &[String], options: &EmbedOptions) -> Result<Vec<Embedding>, AgtrsError>;

    fn model(&self) -> &str;
    fn provider_name(&self) -> &str;
    fn supports_tool_calling(&self) -> bool;
    fn supports_thinking(&self) -> bool;
    fn supports_streaming(&self) -> bool;
    fn supports_embeddings(&self) -> bool;
    fn context_window_tokens(&self) -> Option<usize>;
    fn max_output_tokens(&self) -> Option<u32>;
}
```

## Provider Registry

```rust
// DI container registration
Container::builder()
    // Primary capable model (complex reasoning, coding)
    .register("primary", DynProvider::new(|| async {
        AnthropicProvider::builder()
            .api_key(env!("ANTHROPIC_API_KEY"))
            .model("claude-3-5-sonnet-20241022")
            .build()
    }))
    // Cheap model (planning, summarization, classification)
    .register("cheap", DynProvider::new(|| async {
        GeminiProvider::builder()
            .api_key(env!("GEMINI_API_KEY"))
            .model("gemini-2.0-flash")
            .build()
    }))
    // Embeddings
    .register("embeddings", DynProvider::new(|| async {
        VoyageProvider::builder()
            .api_key(env!("VOYAGE_API_KEY"))
            .model("voyage-code-2")
            .build()
    }))
    // Optional: local model
    .register("local", DynProvider::new(|| async {
        OllamaProvider::builder()
            .base_url("http://localhost:11434")
            .model("codellama:13b")
            .build()
    }))
    .build().await?
```

## Provider Capabilities Matrix

| Provider | Tool Calling | Streaming | Thinking | Embeddings | Context |
|---|---|---|---|---|---|
| Anthropic Claude 3.5 Sonnet | ✓ | ✓ | ✓ | ✗ | 200K |
| Anthropic Claude 3 Haiku | ✓ | ✓ | ✗ | ✗ | 200K |
| Google Gemini 2.0 Flash | ✓ | ✓ | ✗ | ✗ | 1M |
| OpenAI GPT-4o | ✓ | ✓ | ✗ | ✗ | 128K |
| Voyage AI | ✗ | ✗ | ✗ | ✓ | N/A |
| Ollama/codellama | ✓ | ✓ | ✗ | ✓ | 4K–32K |

## Agent-to-Provider Assignment

```toml
# .xaft/config.toml
[agents]
planner.provider = "cheap"      # Gemini Flash for planning
code.provider = "primary"       # Claude 3.5 Sonnet for coding
fixer.provider = "primary"
reviewer.provider = "cheap"     # Gemini Flash sufficient for review
summarizer.provider = "cheap"
indexer.provider = "embeddings" # Voyage for semantic search
```

## References

- agtrs: `agtrs-runtime/src/llm.rs`
- agtrs: `agtrs-anthropic/src/lib.rs`, `agtrs-gemini/src/lib.rs`
