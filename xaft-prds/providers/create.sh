cat > ./01_model_provider_abstraction.md << 'EOF'
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
EOF

cat > ./02_cost_routing.md << 'EOF'
# Cost Routing & Budget Enforcement

## Cost Architecture

```
CostTracker (Arc, shared across session)
    ├── session_total_usd: AtomicF64
    ├── task_total_usd:    AtomicF64
    ├── per_agent_cost:    HashMap<String, AtomicF64>
    └── per_model_cost:    HashMap<String, AtomicF64>

PricingTable
    ├── anthropic_defaults()
    ├── gemini_defaults()
    └── custom(HashMap<model, TokenPrice>)

Budget enforcement:
    AgentConfig::max_cost_usd  → per-agent run limit
    XaftConfig::session_budget → per-session limit
    XaftConfig::task_budget    → per-task limit
```

## Real-Time Cost Tracking

```rust
// Plugged in via SignalBus sync handler
bus.on::<ModelCallComplete>(move |s| {
    cost_tracker.add_model_call(
        &s.model,
        &s.agent_name,
        s.cost_usd,
    );

    // Emit CostUpdate for TUI
    ui_tx.try_send(UiEvent::CostUpdate {
        session_total: cost_tracker.session_total(),
        task_total: cost_tracker.task_total(),
        last_call: s.cost_usd,
    }).ok();
});
```

## Budget Enforcement Levels

```
Level 1: Per-model-call (executor checks after each LLM call)
    if total_cost > config.session_budget:
        return Err(AgtrsError::CostBudgetExceeded)

Level 2: Per-agent-run (AgentConfig::max_cost_usd)
    checked by AgentExecutor after each turn

Level 3: Per-task (XaftConfig::task_budget)
    checked by PlanExecutor after each step

Level 4: Per-session (XaftConfig::session_budget)
    checked by SessionManager
```

## Cost-Aware Model Routing

Route to cheaper models when the task doesn't require maximum capability:

```rust
pub fn select_model_for_task(task_type: &str, cost_remaining: f64) -> &'static str {
    if cost_remaining < 0.10 {
        // Very low budget — use cheapest
        return "gemini-2.0-flash";
    }

    match task_type {
        "plan" | "summarize" | "classify" => "gemini-2.0-flash",
        "review" | "explain"              => "claude-3-haiku",
        "code" | "fix" | "complex"        => "claude-3-5-sonnet-20241022",
        _                                 => "claude-3-5-sonnet-20241022",
    }
}
```

## Cost Reports

```bash
$ xaft cost ses-abc123

Session: ses-abc123
Duration: 4m 32s
Intent: "migrate auth module to JWT"

Model Usage:
  claude-3-5-sonnet  |  12,450 in  |  4,230 out  |  $0.089
  gemini-2.0-flash   |   3,200 in  |    890 out  |  $0.003

Tool Calls:
  read_file      ×12   (0 cost)
  write_file     ×5    (0 cost)
  run_cargo      ×4    (0 cost)
  search_code    ×8    (0 cost)

Total: $0.092 / $2.00 budget (4.6%)
```

## References

- agtrs: `agtrs-runtime/src/cost.rs`, `agtrs-runtime/src/signals.rs`
- agtrs example: `agtrs-examples/src/bin/03_user_budget_caps.rs`
EOF

cat > ./03_provider_routing.md << 'EOF'
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
EOF

echo "Provider docs done"