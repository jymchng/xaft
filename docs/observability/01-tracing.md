# Tracing Conventions

This document describes xaft's tracing conventions: how spans are created and structured, when to use the `instrument` macro, how log files are managed, and the semantic conventions that make xaft's logs useful for debugging, performance analysis, and cost auditing. Tracing is xaft's primary observability mechanism — it is the first thing you reach for when something goes wrong, and it must be structured well enough to answer questions without adding temporary print statements.

---

## The `tracing` Crate

xaft uses the `tracing` crate for all structured logging and span creation. The `tracing` crate provides a superset of the `log` crate's functionality: it supports structured key-value pairs attached to log lines, hierarchical spans that track entry and exit, and subscriber-based routing that allows different consumers to handle different event types differently.

The `tracing` crate is initialized in `xaft-cli` during the dispatch phase. Two subscribers are available:

1. **Compact subscriber** — Used in headless mode. Formats log events as single-line messages with key-value pairs. Suitable for CI pipelines and file logging.

2. **No-op subscriber** — Used in TUI mode. The TUI handles its own display through the streaming pipeline; the tracing subscriber is disabled to avoid duplicate output and performance overhead. The TUI renders agent events, tool calls, and approval requests directly from the `StreamEvent` pipeline, not from log lines.

```rust
// xaft-cli/src/tracing_setup.rs
use tracing_subscriber::{fmt, EnvFilter};

pub fn init_headless() {
    fmt()
        .compact()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("xaft=info")),
        )
        .init();
}

pub fn init_tui() {
    // No-op: the TUI handles display through the streaming pipeline
    // Tracing is still available via RUST_LOG for debugging
    if std::env::var("RUST_LOG").is_ok() {
        fmt()
            .compact()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)  // Don't interfere with TUI on stdout
            .init();
    }
}
```

The `RUST_LOG` environment variable controls the log level. In headless mode, the default level is `info`, which produces a moderate amount of output: agent start/stop, tool calls, LLM call summaries, and errors. Setting `RUST_LOG=debug` enables more verbose output including full request/response payloads, tool execution details, and signal bus dispatch events. Setting `RUST_LOG=trace` enables the most verbose output, including per-token streaming events, which is useful for debugging streaming pipeline issues but produces very large logs.

---

## Span Creation

Spans represent units of work that have a beginning and an end. In xaft, spans are created for every significant operation: agent turns, LLM calls, tool executions, approval requests, and signal bus dispatches. Each span carries key-value pairs that identify the operation and provide context for log lines emitted within the span.

### Manual Span Creation

For operations that are not methods (like closure callbacks or inline async blocks), create spans manually using the `tracing::info_span!` or `tracing::debug_span!` macros:

```rust
let span = tracing::info_span!(
    "agent_turn",
    agent = %self.name,
    iteration = self.call_index.load(Ordering::Relaxed),
);
let _enter = span.enter();

// All log lines within this scope are associated with the span
tracing::info!("Starting agent turn");
// ... do work ...
tracing::info!("Agent turn complete");
```

The `!` format specifier (`%self.name`) uses the `Display` trait to format the value. This is the convention for string-like values (agent names, tool names, model IDs). For numeric values, omit the format specifier and let `tracing` use the default formatting.

The span is entered with `let _enter = span.enter()`, which sets the span as the current span for the duration of the `_enter` guard's lifetime. When the guard is dropped, the span is exited. This RAII pattern ensures that spans are always exited, even if the code panics or returns early.

### Span Naming Convention

Span names follow the pattern `<component>_<operation>`:

| Span Name | Component | Operation |
|-----------|-----------|-----------|
| `agent_turn` | Agent | Turn execution |
| `llm_call` | LLM Provider | API call |
| `tool_execute` | Tool | Tool execution |
| `approval_request` | Approval Gate | Request user decision |
| `signal_dispatch` | Signal Bus | Event dispatch |
| `session_persist` | Session Store | Write to database |
| `config_load` | Config | Load configuration |

This naming convention makes it easy to filter logs by component (e.g., `RUST_LOG=xaft_agent=debug` shows only agent-level spans) or by operation (e.g., searching for `llm_call` spans in a log file to analyze API latency).

### Required Span Fields

Every span must include the following fields when applicable:

| Field | Required In | Type | Example |
|-------|------------|------|---------|
| `agent` | Agent-related spans | String | `"coder"` |
| `tool` | Tool-related spans | String | `"read_file"` |
| `model` | LLM-related spans | String | `"claude-sonnet-4-20250514"` |
| `session_id` | All spans during a session | String | `"550e8400-e29b-41d4-a716-446655440000"` |
| `iteration` | Agent turn spans | usize | `3` |
| `call_index` | LLM call spans | usize | `7` |
| `duration_ms` | Spans that measure time | f64 | `1523.4` |

The `session_id` field is set once at the beginning of a session and is inherited by all child spans. This allows log analysis tools to correlate all events from a single session, even when the session spans multiple log files or multiple processes.

---

## The `instrument` Macro

The `#[tracing::instrument]` attribute macro automatically creates a span for a function or method and enters it for the duration of the function body. It captures the function's arguments as span fields, eliminating the need for manual span creation in many cases.

### Basic Usage

```rust
#[tracing::instrument(
    name = "tool_execute",
    skip(self, cancel),  // Don't log the tool instance or cancel token
    fields(tool = %self.name()),
)]
async fn execute(
    &self,
    input: serde_json::Value,
    cancel: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    // The span is automatically created and entered
    tracing::debug!(?input, "Executing tool");

    // ... tool logic ...

    tracing::info!("Tool completed successfully");
    Ok(result)
}
```

The `name` parameter overrides the default span name (which would be `execute` — the function name). Using a descriptive span name like `tool_execute` is more useful for log analysis because it identifies the operation, not just the method name.

The `skip` parameter excludes arguments from the span fields. Always skip `self` (it would produce a verbose and unhelpful debug representation of the entire struct), `CancellationToken` (it has no useful display representation), and any arguments that contain sensitive data (API keys, file contents). The `fields` parameter adds additional key-value pairs that are not derived from the function arguments — in this case, the tool name from `self.name()`.

### Instrumenting Async Functions

The `#[tracing::instrument]` macro works correctly with async functions. It creates the span before the function body begins and exits the span after the future completes. This is important because async functions may yield and resume multiple times — the span correctly tracks the total time from entry to completion, including time spent waiting for I/O or other tasks.

```rust
#[tracing::instrument(
    name = "llm_stream",
    skip(self, request),
    fields(model = %request.model(), call_index = call_index),
)]
async fn stream_with_tracing(
    &self,
    request: ChatRequest,
    call_index: usize,
) -> Result<BoxStream<'static, Result<StreamChunk, LlmError>>, LlmError> {
    tracing::info!("Starting LLM stream");
    let stream = self.provider.stream(request).await?;
    tracing::info!("Stream established");
    Ok(stream)
}
```

When instrumenting async functions, be aware that the span covers the entire future, including `.await` points. This means that if the LLM provider takes 30 seconds to respond, the span duration will be 30 seconds, not the CPU time spent processing the request. This is the correct behavior for observability — you want to measure wall-clock latency, not CPU time.

### Instrument Skip Guidelines

Follow these guidelines when deciding what to skip in `#[instrument]`:

1. **Always skip `self`** — The debug representation of a struct is rarely useful and often verbose. Use `fields` to capture specific attributes instead.

2. **Always skip `CancellationToken`** — It has no meaningful display representation and clutters the span.

3. **Skip large values** — Tool input `serde_json::Value` can be very large (thousands of characters for file contents). Log it separately with `tracing::debug!(?input, ...)` at the debug level, where it can be filtered out in production.

4. **Never skip identifiers** — Agent names, model IDs, tool names, and session IDs should always be captured in the span fields. These are the primary search keys for log analysis.

5. **Skip sensitive data** — API keys, authentication tokens, and file contents that may contain secrets should never appear in span fields or log lines. If you must log them for debugging, use the `trace` level and ensure the logs are not persisted.

---

## Log File Management

### File Logging in Headless Mode

In headless mode (CI/CD, batch processing), xaft writes structured logs to a file in addition to stderr. The log file is created in the session directory (typically `.xaft/logs/`) and is named with the session ID and timestamp. The file uses the JSON format, which enables machine parsing for automated analysis.

```rust
pub fn init_file_logging(session_id: &str, log_dir: &Path) -> Result<(), std::io::Error> {
    let log_path = log_dir.join(format!("{}-{}.jsonl", session_id, chrono::Utc::now().format("%Y%m%d_%H%M%S")));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let file_subscriber = fmt()
        .json()  // JSON format for machine parsing
        .with_writer(Arc::new(file))
        .with_env_filter(EnvFilter::new("info"))
        .finish();

    // Compose with the stderr subscriber
    tracing_subscriber::registry()
        .with(fmt::Layer::default().with_writer(std::io::stderr))
        .with(fmt::Layer::default().json().with_writer(Arc::new(file)))
        .init();

    Ok(())
}
```

The JSON log format includes the timestamp, level, span name, span fields, and message. This structured format can be ingested by log aggregation systems (ELK, Datadog, CloudWatch Logs) and queried with tools like `jq`. The `jsonl` extension indicates that each line is a complete JSON object, making the file easy to process incrementally.

### Log Rotation

Log files are not rotated during a session. Each session creates a new log file, and the file grows until the session completes. For long-running sessions, this can produce large log files (hundreds of megabytes). To manage disk usage, xaft provides a `xaft sessions prune` command that removes log files for sessions older than a configurable age:

```bash
# Remove logs for sessions older than 30 days
xaft sessions prune --older-than 30d --include-logs
```

The prune command respects a retention policy configured in `xaft.toml`:

```toml
[session]
log_retention_days = 30
max_log_file_size_mb = 500
```

When `max_log_file_size_mb` is exceeded during a session, xaft logs a warning but does not truncate the file. This ensures that no log data is lost, but it alerts the operator that the log file is growing unexpectedly large, which may indicate a runaway agent or excessive debug logging.

### Structured Log Fields for Searching

Every log line emitted during a session includes the `session_id` field, which allows all logs from a single session to be collected and analyzed together. Additional structured fields that aid in log analysis include:

| Field | Description | Search Example |
|-------|-------------|---------------|
| `agent` | The agent that produced the log | `agent=coder` |
| `tool` | The tool being executed | `tool=write_file` |
| `model` | The LLM model being called | `model=claude-sonnet-4-20250514` |
| `call_index` | The sequential call number | `call_index>=5` |
| `duration_ms` | The operation's duration | `duration_ms>10000` |
| `error` | The error message (if any) | `error=~"rate.limit"` |

These fields are included automatically by the span instrumentation and the `instrument` macro. You do not need to add them manually to every log line — the span's fields are inherited by all events within the span. This inheritance is one of the primary benefits of `tracing` over the simpler `log` crate.
