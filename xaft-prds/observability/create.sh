cat > ./01_telemetry.md << 'EOF'
# Telemetry & Observability

## Three-Layer Observability

| Layer | Mechanism | Consumers |
|---|---|---|
| Structured Logging | `tracing` crate + JSON subscriber | Log aggregators (Loki, Splunk) |
| Typed Events | `SignalBus` | TUI, metrics, audit log |
| Distributed Traces | `SpanTracer` + OTLP export | Jaeger, Grafana Tempo |

## Logging Configuration

```toml
[logging]
format = "json"      # "json" | "pretty" | "compact"
level = "info"       # "trace" | "debug" | "info" | "warn" | "error"
output = "stderr"    # "stderr" | "file"
file = ".xaft/logs/xaft.log"  # if output = "file"
```

```rust
fn init_tracing(config: &LoggingConfig) {
    match config.format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(EnvFilter::new(&config.level))
                .with_writer(std::io::stderr)
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new(&config.level))
                .with_target(true)
                .with_level(true)
                .init();
        }
    }
}
```

## Metrics (Prometheus format)

```rust
pub fn register_metrics() {
    // Counters
    describe_counter!("xaft_llm_calls_total", "Total LLM API calls");
    describe_counter!("xaft_tool_calls_total", "Total tool calls");
    describe_counter!("xaft_tokens_total", "Total tokens used");
    describe_counter!("xaft_sessions_total", "Total sessions started");

    // Histograms
    describe_histogram!("xaft_llm_duration_ms", "LLM call duration in milliseconds");
    describe_histogram!("xaft_tool_duration_ms", "Tool call duration in milliseconds");
    describe_histogram!("xaft_task_duration_secs", "Task duration in seconds");

    // Gauges
    describe_gauge!("xaft_active_sessions", "Currently active sessions");
    describe_gauge!("xaft_cost_usd_total", "Cumulative cost in USD");
}

// Exposed at /metrics endpoint (when running xaft serve)
```

## Span Tracer

```rust
// Wrap key operations in spans for distributed tracing

#[traced(name = "plan_step", level = "info")]
async fn execute_plan_step(step: &PlanStep, session: &XaftSession) -> Result<StepResult, XaftError> {
    // ...
}

// Or manually:
let tracer = session.span_tracer();
let span = tracer.start_span("code_agent_run", SpanKind::Agent)
    .with_inputs(serde_json::json!({"step": step.description}))
    .with_metadata(HashMap::from([
        ("task_id".to_string(), session.current_task_id().to_string()),
        ("session_id".to_string(), session.session_id.to_string()),
    ]));

let result = execute_agent(&agent, input, &mut ctx).await;

tracer.complete_span(span, serde_json::json!({"turns": result.turns})).await?;
```

## Cost Metrics Integration

```rust
bus.on::<ModelCallComplete>(|s| {
    counter!("xaft_llm_calls_total",
        "model" => s.model.clone(),
        "agent" => s.agent_name.clone()
    ).increment(1);

    histogram!("xaft_llm_duration_ms",
        "model" => s.model.clone()
    ).record(s.duration_ms);

    counter!("xaft_tokens_total",
        "type" => "input"
    ).increment(s.usage.input_tokens as u64);

    counter!("xaft_tokens_total",
        "type" => "output"
    ).increment(s.usage.output_tokens as u64);
});
```

## References

- agtrs: `agtrs-runtime/src/tracing.rs`, `agtrs-runtime/src/signals.rs`
EOF

cat > ./02_event_bus.md << 'EOF'
# Event Bus Architecture

## Signal Bus vs Message Bus

Two complementary event systems:

| | SignalBus | AgentMessageBus |
|---|---|---|
| Pattern | Publish/subscribe (broadcast) | P2P + broadcast |
| Direction | One-to-many | One-to-one or one-to-many |
| Purpose | Observability, TUI, metrics | Agent coordination |
| Type safety | Per-type subscriptions | Typed message envelopes |
| Persistence | None (in-memory only) | None (in-memory only) |
| Backpressure | Lagged receiver drops | Lagged receiver drops |

## Event Sourcing Readiness

While v1 does not implement event sourcing, all signals are designed to be serializable and replayable. The audit log (`xaft_audit.jsonl`) is effectively an event log.

```
Future EventLog (v2):
  SqliteEventLog implements EventLog
    ├── append(BoxedSignal) → sequence_number
    ├── replay_since(seq: u64) → Stream<BoxedSignal>
    └── snapshot_at(task_id) → SessionSnapshot

This enables:
  - Full session replay for debugging
  - Deterministic re-execution for testing
  - Cross-session analysis
  - Incident post-mortem
```

## TUI Event Pipeline Details

```
SignalBus (broadcast channels, one per signal type)
    │
    ├─ Subscription: ModelCallComplete
    │      │ map to UiEvent::ModelDone
    │      └─ mpsc::Sender<UiEvent> [capacity: 1024]
    │
    ├─ Subscription: ToolCallStarted
    │      │ map to UiEvent::ToolStart
    │      └─ same sender
    │
    ├─ Subscription: FileWritten
    │      │ map to UiEvent::FileChanged
    │      └─ same sender
    │
    └─ Subscription: ApprovalRequested
           │ map to UiEvent::ShowApproval
           └─ same sender

mpsc::Receiver<UiEvent>
    │
    ▼ (consumed on each TUI tick)
AppState mutation
    │
    ▼
terminal.draw(render_fn)
```

## Signal Delivery Guarantees

- Sync handlers (`bus.on::<T>(fn)`): Called synchronously, inline, in the emitter's task. Must complete quickly.
- Async subscribers (`bus.subscribe::<T>()`): Buffered (capacity 256). If buffer full: oldest event dropped. Subscriber receives `RecvError::Lagged(n)`.
- No ordering guarantee between different signal types.
- Same-type signals are ordered per emitter.

## References

- agtrs: `agtrs-runtime/src/signals.rs`
- agtrs: `agtrs-runtime/src/messaging.rs`
EOF

echo "Observability docs done"