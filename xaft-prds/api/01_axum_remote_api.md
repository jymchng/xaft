# Axum Remote Agent API

## Overview

`xaft serve` starts an Axum HTTP server that exposes `xaft` sessions over HTTP. This enables:
- Remote IDE/editor integration (LSP-like agent calls)
- CI/CD pipeline integration
- Web UI dashboards
- Multi-user shared agent sessions (future)

## API Surface

```
POST   /sessions              — Create a new session
GET    /sessions              — List active sessions
GET    /sessions/{id}         — Get session status
DELETE /sessions/{id}         — Cancel and cleanup session

POST   /sessions/{id}/run     — Start executing a goal (returns SSE stream)
POST   /sessions/{id}/suspend — Suspend active task
POST   /sessions/{id}/resume  — Resume suspended task
POST   /sessions/{id}/approve — Respond to approval request
POST   /sessions/{id}/message — Send a follow-up message (interactive mode)
GET    /sessions/{id}/events  — SSE stream of all session events
GET    /sessions/{id}/diff    — Get current staged diff

GET    /health                — Liveness check
GET    /version               — Version info
```

## Axum Router

```rust
// xaft-server/src/server.rs
pub fn build_router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/sessions", post(create_session).get(list_sessions))
        .route("/sessions/:id", get(get_session).delete(cancel_session))
        .route("/sessions/:id/run", post(run_session))
        .route("/sessions/:id/suspend", post(suspend_session))
        .route("/sessions/:id/resume", post(resume_session))
        .route("/sessions/:id/approve", post(approve_tool))
        .route("/sessions/:id/events", get(session_events_sse))
        .route("/sessions/:id/diff", get(get_diff))
        .route("/health", get(health))
        .route("/version", get(version))
        .layer(middleware::from_fn_with_state(Arc::clone(&state), auth_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

## SSE Streaming Run Endpoint

```rust
pub async fn run_session(
    State(state): State<Arc<ServerState>>,
    Path(session_id): Path<Uuid>,
    Json(body): Json<RunRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, axum::Error>>>, AppError> {
    let session = state.get_session(session_id).await
        .ok_or(AppError::NotFound("session not found".into()))?;

    // Verify not already running
    if session.is_running().await {
        return Err(AppError::Conflict("session already executing".into()));
    }

    let intent = parse_intent_from_body(&body);

    let sse_stream = async_stream::stream! {
        // Yield initial acknowledgment
        yield Ok(Event::default()
            .event("session_started")
            .data(serde_json::json!({"session_id": session_id}).to_string()));

        // Subscribe to all session events
        let mut events_rx = session.subscribe_events().await;

        // Spawn session execution
        let session_clone = Arc::clone(&session);
        let run_handle = tokio::spawn(async move {
            session_clone.run(intent).await
        });

        // Stream events to client
        loop {
            tokio::select! {
                Some(event) = events_rx.recv() => {
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    yield Ok(Event::default().data(json));

                    if matches!(event, SessionEvent::Complete { .. } | SessionEvent::Failed { .. }) {
                        break;
                    }
                }
                result = &mut run_handle => {
                    match result {
                        Ok(Ok(())) => yield Ok(Event::default().event("run_complete").data("{}")),
                        Ok(Err(e)) => yield Ok(Event::default().event("run_error").data(
                            serde_json::json!({"error": e.to_string()}).to_string()
                        )),
                        Err(_) => yield Ok(Event::default().event("run_error").data(
                            r#"{"error":"internal error"}"#
                        )),
                    }
                    break;
                }
            }
        }
    };

    Ok(Sse::new(sse_stream).keep_alive(Default::default()))
}
```

## Request/Response Types

```rust
#[derive(Deserialize)]
pub struct RunRequest {
    pub goal: String,
    pub constraints: Option<Vec<String>>,
    pub budget_usd: Option<f64>,
    pub timeout_secs: Option<u64>,
    pub auto_approve: Option<String>,  // "none" | "low" | "medium" | "high"
}

#[derive(Serialize)]
pub struct SessionStatus {
    pub session_id: Uuid,
    pub state: String,
    pub intent: Option<String>,
    pub current_step: Option<usize>,
    pub total_steps: Option<usize>,
    pub cost_usd: f64,
    pub started_at: Option<DateTime<Utc>>,
    pub elapsed_secs: Option<f64>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    TextDelta { agent: String, delta: String },
    ToolCall { agent: String, tool: String, input: serde_json::Value },
    ToolResult { agent: String, tool: String, success: bool, output: String },
    PlanStepStarted { step: usize, total: usize, description: String },
    PlanStepCompleted { step: usize, duration_ms: f64 },
    ApprovalRequired { tool: String, input: serde_json::Value, risk: String },
    CostUpdate { session_total: f64, last_call: f64 },
    Complete { summary: String, cost_usd: f64, turns: usize },
    Failed { error: String },
    Cancelled,
}
```

## Authentication

```rust
async fn auth_middleware(
    State(state): State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if state.config.server.require_auth {
        let token = req.headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;

        if !state.config.server.api_keys.contains(token) {
            return Err(AppError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}
```

## Server Configuration

```toml
[server]
host = "127.0.0.1"  # localhost only by default
port = 7080
require_auth = true
api_keys = ["${XAFT_API_KEY}"]  # from env
max_concurrent_sessions = 4
session_ttl_secs = 3600
```

## References

- agtrs: `agtrs-runtime` (axum feature)
- agtrs guide: `guides/04-running-agents.md`
- Next: [Distributed Runtime →](02_distributed_runtime.md)
