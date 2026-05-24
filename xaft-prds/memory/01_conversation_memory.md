# Conversation & Memory Model

## Three Memory Layers

| Layer | Scope | Duration | Implementation |
|---|---|---|---|
| Short-term | Per agent run | Single turn sequence | `ShortTermMemory` |
| Conversation | Per session | Session lifetime | `ConversationStore` |
| Long-term user | Per user | Persistent | `UserMemoryStore` |

## Short-Term Memory (Sliding Window)

`ShortTermMemory` maintains a rolling token-bounded message history.

```rust
// CodeAgent::on_start
async fn on_start(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
    let mut mem = ShortTermMemory::new(40_000);

    // Load prior conversation if resuming
    if let Some(conv_id) = ctx.context_state().get("conversation_id") {
        let conv_id = conv_id.as_str().unwrap_or("");
        let history = self.conversation_store.load(conv_id).await?;
        for msg in history { mem.add_message(msg); }
    }

    ctx.set_memory(Box::new(mem));
    Ok(())
}
```

## Summarization

When context usage exceeds `summarize_at` threshold (default 80%):

```rust
// Triggered automatically by AgentExecutor
async fn summarize_if_needed(ctx: &mut AgentContext, llm: &dyn LlmProvider) {
    let mem = ctx.short_term_memory();
    let usage = mem.token_estimate() as f64 / ctx.config().memory_window_tokens as f64;

    if usage > ctx.config().summarize_at.unwrap_or(0.8) {
        let to_summarize = mem.messages_to_summarize(20_000);

        let summary_agent = SummaryAgent::new(llm);
        let summary = summary_agent.summarize(&to_summarize).await?;

        mem.trim_oldest(to_summarize.len());
        mem.set_summary(summary);
    }
}
```

## Conversation Persistence

```rust
// CodeAgent::on_complete
async fn on_complete(&self, ctx: &mut AgentContext, _: &AgentResponse) -> Result<(), AgtrsError> {
    if let Some(conv_id) = ctx.context_state().get("conversation_id") {
        let conv_id = conv_id.as_str().unwrap_or("default");
        self.conversation_store.save(conv_id, ctx.messages()).await?;
    }
    Ok(())
}
```

Backends:
- **Development**: `InMemoryConversationStore`
- **Production**: `SqliteConversationStore` (wraps `agtrs-store/sqlite`)

## Project Memory (Persistent)

`xaft` maintains project-level memory across sessions:

```rust
pub struct ProjectMemory {
    store: Arc<SqliteStore>,
}

impl ProjectMemory {
    /// Remember a fact about the project
    pub async fn remember(&self, fact: &str, category: &str) -> Result<(), XaftError>;

    /// Recall relevant facts for current task
    pub async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryResult>, XaftError>;

    /// Prune stale memories (older than N days, low confidence)
    pub async fn prune(&self, max_age_days: u64) -> Result<usize, XaftError>;
}
```

Example auto-stored facts:
- "Auth module is in `src/auth/`. Last modified by xaft on 2026-01-10."
- "Cargo workspace uses edition 2024, MSRV 1.86."
- "Tests run with `just test`, not `cargo test` directly."

## References

- agtrs: `agtrs-runtime/src/memory.rs`
- agtrs guide: `guides/10-memory.md`
