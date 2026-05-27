# Axum Integration

This document describes the planned integration between xaft and the Axum web framework. The integration exposes xaft's agent runtime as an HTTP API, enabling remote access, multi-user scenarios, and integration with web-based frontends. The design centers on the `RuntimeDispatch` trait, which provides a narrow, async interface for submitting tasks, checking status, and consuming events over HTTP.

---

## Motivation

The current xaft architecture is designed for single-user, terminal-based interaction. The TUI provides a rich interactive experience, but it is limited to a single user on a single machine. Several production scenarios require HTTP access to the agent runtime:

1. **Web-based UI**: A browser-based dashboard that displays agent progress, tool results, and cost tracking in real time. The web UI consumes the same event stream as the TUI but renders it using HTML/CSS/JavaScript.

2. **Multi-user access**: A shared xaft instance that serves multiple developers, each submitting tasks through an API. The API isolates sessions per user and provides authentication and authorization.

3. **CI/CD integration**: A headless xaft instance that accepts task submissions from CI pipelines, runs them asynchronously, and returns results via HTTP callbacks or polling.

4. **IDE integration**: A VS Code extension or JetBrains plugin that communicates with xaft through a local HTTP server, displaying agent output in an editor panel and routing approval requests to the IDE's notification system.

The Axum integration is designed to support all of these scenarios through a unified HTTP API that maps directly to the `RuntimeDispatch` trait's methods.

---

## RuntimeDispatch Trait

The `RuntimeDispatch` trait is the integration point between the xaft runtime and any external system. It defines a narrow async interface with five methods: `submit_message`, `cancel`, `status`, `subscribe`, and `shutdown`. This trait was designed for HTTP integration from the start — each method maps naturally to an HTTP endpoint.

```rust
#[async_trait]
pub trait RuntimeDispatch: Send + Sync {
    /// Submit a user message to the active agent.
    /// Returns a session ID that can be used to track the task.
    async fn submit_message(&self, message: String) -> Result<SessionId, RuntimeError>;

    /// Cancel the current agent operation.
    async fn cancel(&self) -> Result<(), RuntimeError>;

    /// Get the current runtime status.
    async fn status(&self) -> Result<RuntimeStatus, RuntimeError>;

    /// Subscribe to the event stream for a session.
    /// Returns a broadcast receiver that delivers StreamEvent instances.
    async fn subscribe(&self, session_id: &SessionId) -> Result<broadcast::Receiver<StreamEvent>, RuntimeError>;

    /// Shut down the runtime gracefully.
    async fn shutdown(&self) -> Result<(), RuntimeError>;
}
```

The `RuntimeDispatch` trait is implemented by `XaftRuntime`, which delegates each method to the appropriate internal component. The `submit_message` method creates a new session, constructs the agent pipeline, and starts the task on a background tokio task. The `cancel` method triggers the cancellation token. The `status` method returns a snapshot of the runtime's current state. The `subscribe` method returns a broadcast receiver for the specified session's event stream.

The key design decision is that `RuntimeDispatch` does not expose the runtime's internal types — agents, tools, providers, or the signal bus. It exposes only the operations that an external system needs, using serializable types (`SessionId`, `RuntimeStatus`, `StreamEvent`). This encapsulation ensures that the HTTP API remains stable even when the internal implementation changes.

---

## HTTP API Design

The HTTP API maps each `RuntimeDispatch` method to one or more REST endpoints. The API follows REST conventions: resources are identified by URLs, operations are expressed as HTTP methods, and responses use standard HTTP status codes.

### Endpoints

| Method | Path | RuntimeDispatch Method | Description |
|--------|------|----------------------|-------------|
| `POST` | `/api/v1/sessions` | `submit_message` | Create a new session and start a task |
| `POST` | `/api/v1/sessions/:id/cancel` | `cancel` | Cancel a running session |
| `GET` | `/api/v1/sessions/:id` | `status` | Get session status |
| `GET` | `/api/v1/sessions/:id/events` | `subscribe` | Stream events via SSE |
| `DELETE` | `/api/v1/runtime` | `shutdown` | Shut down the runtime |

### Create Session

```bash
POST /api/v1/sessions
Content-Type: application/json

{
    "prompt": "Refactor the authentication module to use JWT tokens",
    "model": "claude-sonnet-4-20250514",
    "preset": "full-workflow",
    "config_overrides": {
        "max_iterations": 30
    }
}
```

Response:

```json
{
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "status": "running",
    "created_at": "2024-01-15T10:30:00Z"
}
```

The `POST /api/v1/sessions` endpoint creates a new session and starts the task asynchronously. The response returns immediately with the session ID — the task runs in the background, and the client monitors progress through the event stream. This async-start pattern is essential for long-running tasks that may take minutes to complete. A synchronous API that blocks until the task completes would time out HTTP connections and waste server resources.

### Stream Events

```bash
GET /api/v1/sessions/550e8400-e29b-41d4-a716-446655440000/events
Accept: text/event-stream
```

Response (Server-Sent Events):

```
event: token
data: {"text": "I'll"}

event: token
data: {"text": " refactor"}

event: tool_call
data: {"name": "read_file", "input": {"path": "src/auth.rs"}, "call_id": "call_001"}

event: tool_result
data: {"name": "read_file", "output": "...", "call_id": "call_001", "duration_ms": 45}

event: model_call_complete
data: {"model": "claude-sonnet-4-20250514", "input_tokens": 1200, "output_tokens": 340, "cost_usd": 0.0087}

event: done
data: {"summary": "Refactored authentication module to use JWT tokens"}
```

The event stream uses Server-Sent Events (SSE) rather than WebSockets. SSE is simpler to implement, works with standard HTTP infrastructure (load balancers, proxies, CDN caching), and supports automatic reconnection with the `Last-Event-ID` header. WebSockets provide bidirectional communication, but xaft's event stream is fundamentally unidirectional (server pushes events to client), so SSE is the natural fit.

Each SSE event has a type (`token`, `tool_call`, `tool_result`, `approval_request`, `model_call_complete`, `done`) and a JSON data payload. The event types correspond directly to the `StreamEvent` enum variants, providing a one-to-one mapping between the internal event system and the HTTP API.

### Approval Requests

Approval requests require bidirectional communication: the server sends the approval request, and the client sends the decision. This is handled through a separate endpoint:

```bash
GET /api/v1/sessions/:id/events  # SSE stream includes approval_request events

event: approval_request
data: {"tool_name": "write_file", "input": {"path": "src/auth.rs"}, "request_id": "apr_001"}
```

```bash
POST /api/v1/sessions/:id/approvals/apr_001
Content-Type: application/json

{
    "decision": "approve"
}
```

The approval flow splits the bidirectional communication into two unidirectional channels: the SSE stream delivers the approval request to the client, and a separate HTTP POST delivers the client's decision to the server. This avoids the complexity of bidirectional WebSocket communication while still supporting the approval workflow. The `request_id` in the approval event correlates the request with the response, allowing the server to route the decision to the correct waiting task.

---

## Axum Router Configuration

The Axum integration is implemented as a separate crate (`xaft-server`) that depends on `xaft-runtime` and `axum`. It constructs an Axum router with the API endpoints and a shared `Arc<XaftRuntime>` state:

```rust
use axum::{
    Router,
    routing::{get, post, delete},
    extract::State,
};
use std::sync::Arc;

pub fn create_router(runtime: Arc<XaftRuntime>) -> Router {
    Router::new()
        .route("/api/v1/sessions", post(create_session))
        .route("/api/v1/sessions/:id", get(get_session_status))
        .route("/api/v1/sessions/:id/cancel", post(cancel_session))
        .route("/api/v1/sessions/:id/events", get(stream_events))
        .route("/api/v1/sessions/:id/approvals/:request_id", post(submit_approval))
        .route("/api/v1/runtime", delete(shutdown_runtime))
        .with_state(runtime)
}
```

Each handler extracts the `Arc<XaftRuntime>` from Axum's state and calls the corresponding `RuntimeDispatch` method. The handlers are thin wrappers that translate between HTTP types and the runtime's types:

```rust
async fn create_session(
    State(runtime): State<Arc<XaftRuntime>>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    let session_id = runtime.submit_message(body.prompt).await
        .map_err(ApiError::from)?;

    Ok(Json(CreateSessionResponse {
        session_id,
        status: "running".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
    }))
}
```

The `ApiError` type maps `RuntimeError` variants to appropriate HTTP status codes: `ConfigError` becomes 400 (Bad Request), `LlmError::ConnectionFailed` becomes 502 (Bad Gateway), `AgentError::Cancelled` becomes 204 (No Content), and internal errors become 500 (Internal Server Error). This mapping provides meaningful HTTP semantics while preserving the error details in the response body.

### SSE Implementation

The SSE handler uses Axum's streaming response support to deliver events as they arrive:

```rust
use axum::response::sse::{Event, Sse};
use futures::stream::Stream;

async fn stream_events(
    State(runtime): State<Arc<XaftRuntime>>,
    Path(session_id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = runtime.subscribe(&session_id).await
        .map_err(|_| {
            // Return a stream that immediately closes with an error
        })
        .unwrap();

    let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
        .filter_map(|result| match result {
            Ok(event) => {
                let event_type = event_type_name(&event);
                let data = serde_json::to_string(&event).ok()?;
                Some(Ok(Event::default()
                    .event(event_type)
                    .data(data)))
            }
            Err(_) => None, // Skip lagged events
        });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
```

The SSE stream wraps a `broadcast::Receiver<StreamEvent>` in a `BroadcastStream`, which converts the receiver into an async stream. Each event is serialized to JSON and wrapped in an SSE `Event` with the appropriate event type. The `keep_alive` configuration sends a keep-alive comment every 15 seconds, which prevents intermediate proxies and load balancers from closing idle connections.

---

## Session Management

The HTTP API supports concurrent sessions. Each `POST /api/v1/sessions` request creates a new session with its own agent, tool registry, and event stream. Sessions run independently — one session's agents do not interfere with another's. This is the key difference from the TUI mode, which supports only one session at a time.

Concurrent sessions are managed by the `SessionManager`, which maintains a map of active sessions and their associated runtime state:

```rust
pub struct SessionManager {
    sessions: Arc<DashMap<SessionId, ActiveSession>>,
    runtime_config: Arc<XaftConfig>,
}

pub struct ActiveSession {
    pub cancel_token: CancellationToken,
    pub event_tx: broadcast::Sender<StreamEvent>,
    pub status: Arc<Mutex<SessionStatus>>,
}
```

The `SessionManager` creates a new `ActiveSession` for each incoming request, spawns the agent task on the tokio runtime, and returns the session ID to the client. The session's event stream is available immediately — the client can start listening before the agent begins its first turn. The `DashMap` provides lock-free concurrent access to the session map, which is important when multiple clients are creating and monitoring sessions simultaneously.

Session cleanup happens automatically when the agent task completes. The task updates the session status to `Completed` or `Failed`, and a background reaper task removes sessions that have been inactive for longer than the configured retention period (default: 24 hours). The reaper also cancels any sessions that have been running for longer than the maximum session duration (default: 1 hour), preventing runaway agents from consuming resources indefinitely.

---

## Authentication and Authorization

The HTTP API supports two authentication modes:

1. **API key**: A static API key passed in the `Authorization: Bearer <key>` header. Suitable for internal tools and CI/CD pipelines where the client is trusted.

2. **JWT**: A JSON Web Token that encodes the user's identity and permissions. Suitable for multi-user deployments where different users have different access levels.

Authorization is enforced at the session level. Each session is owned by the user who created it, and only the owner can cancel the session, submit approvals, or access the event stream. The `SessionManager` stores the owner's identity (extracted from the authentication token) in the `ActiveSession` metadata, and each handler verifies that the requesting user matches the session's owner before performing the operation.

```rust
async fn cancel_session(
    State(manager): State<Arc<SessionManager>>,
    Path(session_id): Path<String>,
    user: AuthenticatedUser,  // Extracted from the Authorization header
) -> Result<Json<CancelResponse>, ApiError> {
    let session = manager.get(&session_id)
        .ok_or(ApiError::NotFound)?;

    // Verify ownership
    if session.owner != user.id {
        return Err(ApiError::Forbidden);
    }

    session.cancel_token.cancel();
    Ok(Json(CancelResponse { status: "cancelled".to_string() }))
}
```

This session-level authorization is sufficient for the current design. More fine-grained authorization (e.g., restricting which tools an agent can use, or which files it can modify) is handled by the agent's configuration, not by the HTTP API layer.

---

## Future Directions

The Axum integration is planned but not yet implemented. The current priority is stabilizing the core runtime and the TUI experience. When the integration is implemented, the following features are planned:

1. **WebSocket support**: An alternative to SSE that supports bidirectional communication, simplifying the approval flow and enabling real-time interaction patterns (e.g., the client sends a follow-up message while the agent is running).

2. **Session persistence**: Storing session state in the database so that sessions survive server restarts. A client can reconnect to a running session after a network interruption.

3. **Rate limiting**: Per-user rate limits on session creation and API calls, preventing a single user from monopolizing the LLM provider's quota.

4. **Metrics endpoint**: A `/metrics` endpoint that exposes Prometheus-compatible metrics (session count, token usage, cost, latency percentiles) for monitoring and alerting.

5. **GraphQL API**: An alternative to REST that allows clients to query exactly the data they need, reducing over-fetching for clients that only need a subset of the session's data.
