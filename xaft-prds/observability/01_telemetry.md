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
