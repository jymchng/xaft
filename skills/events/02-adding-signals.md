# Adding New Signals

## Purpose

As xaft evolves, new observability requirements emerge. You might need to track a new type of event—model fallback, rate limit encounters, cache hits, custom tool metrics, or workflow state transitions. This document provides a step-by-step guide for adding a new signal type to the xaft system, from defining the struct to emitting it at the right point, subscribing in the EventBridge, and surfacing it in the TUI. Following this process ensures new signals integrate cleanly with the existing bus architecture and don't introduce subtle type-routing bugs.

Adding a signal is a cross-cutting change: it touches the signal definition, the emission site, the event bridge, and the TUI. Each step has specific requirements that, if missed, result in a signal that is emitted but never displayed, or displayed with incorrect data.

## Mental Model

Think of adding a signal as **installing a new sensor in a factory**. You need to: (1) design the sensor (define the struct), (2) place it at the right point in the production line (add emission), (3) wire it to the monitoring dashboard (subscribe in EventBridge), and (4) add a gauge on the dashboard for it (add TUI handling). Skip any step and the sensor exists but provides no value.

```
Step 1: Define Struct           Step 2: Add Emission
┌───────────────────┐          ┌───────────────────┐
│ struct MySignal { │          │ bus.try_emit(     │
│   field1: Type,   │──────────▶│   MySignal {      │
│   field2: Type,   │          │     field1: ...,   │
│ }                 │          │     field2: ...,   │
└───────────────────┘          │   }                │
                               │ ).await            │
                               └────────┬──────────┘
                                        │
Step 3: EventBridge              Step 4: TUI Handling
┌───────────────────┐          ┌───────────────────┐
│ bridge.on::<MySig>│          │ TuiEvent::MySignal │
│   (|sig| {        │──────────▶│ AppState::handle() │
│     forward(sig)  │          │ render in widget    │
│   })              │          │                     │
└───────────────────┘          └───────────────────┘
```

## Extension Patterns

### Step 1: Define the Signal Struct

Every signal must implement `Debug`, `Clone`, `Serialize`, and `Deserialize`. It should be a plain data struct with no behavior—no methods, no trait objects, no `Arc` references. The struct name should follow the `Xaft` prefix convention for xaft-level signals, or use descriptive names for agtrs-level signals.

```rust
use serde::{Serialize, Deserialize};

/// Emitted when the LLM provider falls back from a primary model to a secondary model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftModelFallback {
    /// The model that was originally requested
    pub primary_model: String,
    /// The model that was actually used
    pub fallback_model: String,
    /// Why the fallback occurred (e.g., "rate_limit", "timeout", "context_length_exceeded")
    pub reason: String,
    /// The conversation key for the agent that triggered the fallback
    pub conversation_key: String,
}
```

Design considerations for signal structs:

- **Use owned types only.** `String` not `&str`, `Vec<String>` not `&[String]`. Signals outlive their emission site.
- **Include enough context for consumers.** A signal with just `reason: String` is hard to act on. Include `conversation_key`, `agent_name`, or `tool_name` so consumers can filter or route.
- **Keep it small.** Signals are cloned for every consumer. Avoid embedding large payloads (full file contents, complete API responses). Use summaries or references instead.
- **Use semantically meaningful types.** `duration_ms: u64` is better than `duration: f64` because it's unambiguous about units.

### Step 2: Add Emission Points

Find the code location where the event occurs and add a `try_emit_signal` call. The emission should happen at the point of truth—the function that actually performs the action, not a wrapper or caller.

```rust
// In llm_client.rs, after detecting a fallback
async fn call_with_fallback(&self, prompt: &str, ctx: &AgentContext) -> Result<Response> {
    match self.primary_client.chat(prompt).await {
        Ok(response) => Ok(response),
        Err(e) if e.is_retryable() => {
            tracing::warn!("Primary model failed: {}, falling back", e);

            // Emit the fallback signal
            ctx.signal_bus.try_emit_signal(XaftModelFallback {
                primary_model: self.primary_client.model_name().to_string(),
                fallback_model: self.fallback_client.model_name().to_string(),
                reason: e.category().to_string(),
                conversation_key: ctx.conversation_key.clone(),
            }).await;

            self.fallback_client.chat(prompt).await
        }
        Err(e) => Err(e),
    }
}
```

Emission guidelines:

- **Emit after the event occurs, not before.** For "completed" signals, emit after the operation succeeds, not before. This ensures the signal data is accurate.
- **Emit at the lowest relevant layer.** A model fallback signal belongs in the LLM client, not in the agent executor. This avoids duplicate emissions and keeps the signal source canonical.
- **Use `try_emit_signal`, not `emit_signal`.** The `try_` variant is non-blocking and handles missing consumers gracefully. The non-`try_` variant is reserved for critical signals that must be delivered.
- **Don't emit in hot loops.** If a function runs thousands of times per second, emitting a signal each time will overwhelm the bus. Aggregate or throttle instead.

### Step 3: Subscribe in EventBridge

The `EventBridge` forwards signals from the internal bus to external consumers (typically the TUI via a cross-thread channel). Add a subscription for your new signal type:

```rust
impl EventBridge {
    pub async fn start(&self, bus: &SignalBus, tx: &mpsc::Sender<TuiEvent>) {
        // ... existing subscriptions ...

        // Forward XaftModelFallback to TUI
        bus.on::<XaftModelFallback>(|signal| {
            let _ = tx.try_send(TuiEvent::ModelFallback(signal));
        }).await;
    }
}
```

The `try_send` is non-blocking—if the TUI channel is full, the event is dropped. This is intentional: the TUI should never block the agent runtime, and a dropped TUI event is acceptable for most signals.

### Step 4: Add TuiEvent Variant and Handle in AppState

Add a new variant to the `TuiEvent` enum:

```rust
pub enum TuiEvent {
    // ... existing variants ...
    ModelFallback(XaftModelFallback),
}
```

Then handle it in `AppState::handle_event()`:

```rust
impl AppState {
    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            // ... existing handlers ...
            TuiEvent::ModelFallback(signal) => {
                self.model_fallbacks.push(ModelFallbackEntry {
                    primary: signal.primary_model,
                    fallback: signal.fallback_model,
                    reason: signal.reason,
                    timestamp: std::time::SystemTime::now(),
                });
                self.status_bar.set_message(format!(
                    "Fell back from {} to {} ({})",
                    signal.primary_model, signal.fallback_model, signal.reason
                ));
            }
        }
    }
}
```

Finally, render the data in the appropriate widget. If the fallback information should be displayed in the status bar, update the status bar widget. If it needs a new panel, add a `PaneType` variant and a new widget (see the TUI widget development guide).

## Common Pitfalls

1. **Forgetting to add the EventBridge subscription.** The signal is emitted and consumed by internal handlers, but never reaches the TUI. This is the most common omission because the EventBridge is a separate module from the emission site.

2. **Adding the TuiEvent variant but not handling it in AppState.** The compiler won't catch this if you use a `_ => {}` catch-all in the match. Always use an exhaustive match or explicitly list the unhandled variant with a `todo!()`.

3. **Emitting before the data is available.** If you emit a signal with placeholder data and then update it later, consumers will see stale data. Emit once, after all fields are populated.

4. **Making signal fields optional when they shouldn't be.** A field like `commit_hash: Option<String>` suggests that sometimes there's no commit hash. If the signal only makes sense with a commit hash, make it required: `commit_hash: String`. Optional fields add branching complexity to every consumer.

5. **Using `serde_json::Value` as a field type.** This defeats the type safety of the signal system. If you need flexible data, define a proper enum or struct. Consumers should be able to destructure signals without parsing JSON.

6. **Not adding `Clone` derive.** Every signal must be `Clone` because the bus sends copies to each subscriber. Missing this derive causes a compile error, but it's easy to forget when adding a new field that contains a non-Clone type.

7. **Signal struct growing too large.** If you find yourself adding more and more fields to a signal, consider splitting it into multiple signals. A `XaftAgentActivity` signal with 20 optional fields is a code smell; it should probably be 3-4 focused signals.

## Invariants

- **Every signal struct implements `Debug + Clone + Serialize + Deserialize`.** No exceptions. The bus requires `Clone`; serialization is needed for persistence and network forwarding.
- **Signal structs are plain data.** No methods, no trait objects, no interior mutability, no `Arc<Mutex<...>>`.
- **Emission uses `try_emit_signal()`** for fire-and-forget semantics. The `emit_signal()` variant (which blocks until handlers complete) is reserved for critical signals and should be used sparingly.
- **EventBridge subscriptions are registered before the agent starts.** If you subscribe after the agent is already running, you'll miss early signals.
- **TuiEvent variants map 1:1 to signal types.** Don't merge multiple signal types into a single TuiEvent variant; it makes matching ambiguous.
- **`AppState::handle_event()` handles every `TuiEvent` variant.** Use exhaustive matches or `todo!()` for unimplemented variants—never `_ => {}`.

## Examples

### Complete Example: Adding a Cache Hit Signal

**1. Define the struct:**

```rust
/// Emitted when the LLM response cache returns a hit, avoiding an API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftCacheHit {
    pub conversation_key: String,
    pub cache_key: String,
    pub saved_tokens: usize,
}
```

**2. Add emission in the caching layer:**

```rust
// In cached_llm_client.rs
async fn chat(&self, prompt: &str, ctx: &AgentContext) -> Result<Response> {
    let cache_key = self.compute_cache_key(prompt);

    if let Some(cached) = self.cache.get(&cache_key).await {
        ctx.signal_bus.try_emit_signal(XaftCacheHit {
            conversation_key: ctx.conversation_key.clone(),
            cache_key: cache_key.clone(),
            saved_tokens: cached.token_count,
        }).await;
        return Ok(cached.response);
    }

    let response = self.inner.chat(prompt, ctx).await?;
    self.cache.insert(cache_key, &response).await;
    Ok(response)
}
```

**3. Subscribe in EventBridge:**

```rust
bus.on::<XaftCacheHit>(|signal| {
    let _ = tx.try_send(TuiEvent::CacheHit(signal));
}).await;
```

**4. Add TuiEvent variant and handle:**

```rust
pub enum TuiEvent {
    // ... existing variants ...
    CacheHit(XaftCacheHit),
}

impl AppState {
    pub fn handle_event(&mut self, event: TuiEvent) {
        match event {
            // ... existing handlers ...
            TuiEvent::CacheHit(signal) => {
                self.cache_stats.record_hit(signal.saved_tokens);
                self.status_bar.set_message(format!(
                    "Cache hit! Saved {} tokens", signal.saved_tokens
                ));
            }
        }
    }
}
```

### Signal Without TUI Integration

Not every signal needs to reach the TUI. For signals used only by internal subsystems (metrics, logging, audit), skip steps 3 and 4:

```rust
// Just emit and subscribe internally
bus.on::<XaftCacheHit>(|signal| {
    COUNTER_CACHE_HITS.increment(1);
    HISTOGRAM_TOKENS_SAVED.record(signal.saved_tokens as f64);
}).await;
```
