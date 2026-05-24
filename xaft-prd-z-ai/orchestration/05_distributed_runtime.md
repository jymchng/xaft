# Distributed Runtime

> Future vision: distributed xauft execution. Extending the Axum SSE bridge to
> a full remote agent API, database-backed `TaskStore`, networked
> `AgentMessageBus`, actor model, multi-machine sessions, and gRPC/HTTP2
> transport.

---

## 1. Motivation

Today, xauft runs as a single-process CLI. For large-scale codebases and
teams, this has limitations:

| Limitation              | Impact                                              |
|-------------------------|-----------------------------------------------------|
| Single-machine compute  | Cannot parallelize across multiple GPUs/hosts       |
| Local-only storage      | Session state lost if machine crashes               |
| No shared sessions      | Multiple developers can't collaborate on same task  |
| Single provider region  | Higher latency for remote API calls                 |
| Limited observability   | Debugging distributed agent flows is hard           |

The distributed runtime addresses these by making xauft's existing
primitives network-aware.

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Distributed xauft Cluster                       │
│                                                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │
│  │   Node 1     │  │   Node 2     │  │   Node 3     │              │
│  │              │  │              │  │              │              │
│  │ ┌──────────┐ │  │ ┌──────────┐ │  │ ┌──────────┐ │              │
│  │ │ xauft    │ │  │ │ xauft    │ │  │ │ xauft    │ │              │
│  │ │ Agent    │ │  │ │ Agent    │ │  │ │ Agent    │ │              │
│  │ │ Runtime  │ │  │ │ Runtime  │ │  │ │ Runtime  │ │              │
│  │ └────┬─────┘ │  │ └────┬─────┘ │  │ └────┬─────┘ │              │
│  │      │       │  │      │       │  │      │       │              │
│  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │  │ ┌────▼─────┐ │              │
│  │ │ Network  │ │  │ │ Network  │ │  │ │ Network  │ │              │
│  │ │ Adapter  │ │  │ │ Adapter  │ │  │ │ Adapter  │ │              │
│  │ └────┬─────┘ │  │ └────┬─────┘ │  │ └────┬─────┘ │              │
│  └──────┼───────┘  └──────┼───────┘  └──────┼───────┘              │
│         │                 │                 │                       │
│         └─────────────────┼─────────────────┘                       │
│                           │                                         │
│                    ┌──────▼──────┐                                  │
│                    │   Message   │     gRPC / HTTP2                  │
│                    │    Bus      │     (network transport)           │
│                    └──────┬──────┘                                  │
│                           │                                         │
│                    ┌──────▼──────┐                                  │
│                    │  TaskStore  │     PostgreSQL / CockroachDB      │
│                    │  (database) │                                   │
│                    └─────────────┘                                  │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. Axum SSE Bridge → Full Remote Agent API

### 3.1 Current State: SSE Bridge

xauft currently uses an Axum-based HTTP server for Server-Sent Events (SSE)
to stream agent events to a frontend:

```rust
// Current: SSE bridge for event streaming
pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = state.event_bus.subscribe().map(|event| {
        Ok(Event::default().data(serde_json::to_string(&event).unwrap()))
    });
    Sse::new(stream)
}
```

### 3.2 Future: Full Remote Agent API

The SSE bridge extends to a comprehensive REST + gRPC API:

```
┌─────────────────────────────────────────────────────────────────┐
│                      xauft API Server (Axum)                    │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                    REST API (HTTP/1.1)                     │  │
│  │                                                           │  │
│  │  POST   /v1/sessions              Create session          │  │
│  │  GET    /v1/sessions/:id          Get session status      │  │
│  │  DELETE /v1/sessions/:id          Cancel session          │  │
│  │  POST   /v1/sessions/:id/tasks    Submit task             │  │
│  │  GET    /v1/sessions/:id/tasks    List tasks              │  │
│  │  GET    /v1/tasks/:id             Get task status         │  │
│  │  POST   /v1/tasks/:id/approve     Approve step            │  │
│  │  POST   /v1/tasks/:id/reject      Reject step             │  │
│  │  GET    /v1/tasks/:id/events      Event stream (SSE)      │  │
│  │  GET    /v1/tasks/:id/checkpoints List checkpoints        │  │
│  │  POST   /v1/tasks/:id/resume      Resume from checkpoint  │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                    gRPC API (HTTP/2)                       │  │
│  │                                                           │  │
│  │  AgentService                                              │  │
│  │    rpc CreateSession(CreateSessionReq) returns (Session)   │  │
│  │    rpc SubmitTask(SubmitTaskReq) returns (Task)            │  │
│  │    rpc StreamEvents(TaskId) returns (stream AgentEvent)    │  │
│  │    rpc ApproveStep(ApproveReq) returns (ApproveResp)       │  │
│  │    rpc ResumeTask(ResumeReq) returns (Task)                │  │
│  │                                                           │  │
│  │  ClusterService (internal)                                 │  │
│  │    rpc RegisterNode(RegisterReq) returns (NodeInfo)        │  │
│  │    rpc Heartbeat(HeartbeatReq) returns (HeartbeatResp)     │  │
│  │    rpc AcquireAgent(AcquireReq) returns (AgentHandle)      │  │
│  │    rpc TransferContext(TransferReq) returns (TransferResp)  │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### 3.3 Axum Server Implementation Sketch

```rust
pub struct DistributedApiServer<P: LlmProvider> {
    state: Arc<ApiState<P>>,
    listener: TcpListener,
}

struct ApiState<P: LlmProvider> {
    runner: Arc<TaskRunner<P>>,
    store: Arc<dyn TaskStore>,
    bus: Arc<AgentMessageBus>,
    session_manager: Arc<SessionManager>,
    node_registry: Arc<NodeRegistry>,
}

impl<P: LlmProvider + Clone + 'static> DistributedApiServer<P> {
    pub async fn serve(self) -> Result<(), ServerError> {
        let app = Router::new()
            // Session endpoints
            .route("/v1/sessions", post(Self::create_session))
            .route("/v1/sessions/:id", get(Self::get_session))
            .route("/v1/sessions/:id", delete(Self::cancel_session))

            // Task endpoints
            .route("/v1/sessions/:id/tasks", post(Self::submit_task))
            .route("/v1/sessions/:id/tasks", get(Self::list_tasks))
            .route("/v1/tasks/:id", get(Self::get_task))
            .route("/v1/tasks/:id/approve", post(Self::approve_step))
            .route("/v1/tasks/:id/reject", post(Self::reject_step))
            .route("/v1/tasks/:id/resume", post(Self::resume_task))

            // Event streaming
            .route("/v1/tasks/:id/events", get(Self::stream_events))

            // Cluster management
            .route("/v1/cluster/nodes", post(Self::register_node))
            .route("/v1/cluster/heartbeat", post(Self::heartbeat))

            .with_state(self.state.clone());

        axum::serve(self.listener, app).await?;
        Ok(())
    }

    async fn submit_task(
        State(state): State<Arc<ApiState<P>>>,
        Path(session_id): Path<SessionId>,
        Json(body): Json<SubmitTaskRequest>,
    ) -> impl IntoResponse {
        let task = Task::new(body.description, session_id);
        let task_id = task.id;
        state.runner.submit(task).await
            .map(|_| (StatusCode::ACCEPTED, Json(SubmitTaskResponse { task_id })))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    }

    async fn stream_events(
        State(state): State<Arc<ApiState<P>>>,
        Path(task_id): Path<TaskId>,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        let stream = state.bus.subscribe()
            .filter_map(move |msg| {
                let task_id = task_id;
                async move {
                    match msg {
                        AgentMessage::Completed { step_id, result, .. } => {
                            Some(Ok(Event::default()
                                .event("completed")
                                .data(serde_json::to_string(&result).unwrap())))
                        }
                        AgentMessage::Progress { message, percentage, .. } => {
                            Some(Ok(Event::default()
                                .event("progress")
                                .data(format!("{}%: {}", percentage, message))))
                        }
                        _ => None,
                    }
                }
            });
        Sse::new(stream)
    }
}
```

---

## 4. Database-Backed TaskStore

### 4.1 Schema Design

```sql
-- Tasks table
CREATE TABLE tasks (
    id              UUID PRIMARY KEY,
    session_id      UUID NOT NULL REFERENCES sessions(id),
    description     TEXT NOT NULL,
    state           VARCHAR(32) NOT NULL DEFAULT 'received',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ,
    total_tokens    BIGINT NOT NULL DEFAULT 0,
    total_duration_ms INTEGER,
    metadata        JSONB DEFAULT '{}'
);

CREATE INDEX idx_tasks_session ON tasks(session_id);
CREATE INDEX idx_tasks_state ON tasks(state);

-- Plans table
CREATE TABLE plans (
    id              UUID PRIMARY KEY,
    task_id         UUID NOT NULL REFERENCES tasks(id),
    revision        INTEGER NOT NULL DEFAULT 0,
    planner_type    VARCHAR(32) NOT NULL,
    steps           JSONB NOT NULL,    -- serialized Vec<PlannedStep>
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(task_id, revision)
);

-- Steps table (denormalized for fast queries)
CREATE TABLE steps (
    id              UUID PRIMARY KEY,
    plan_id         UUID NOT NULL REFERENCES plans(id),
    task_id         UUID NOT NULL REFERENCES tasks(id),
    step_index      INTEGER NOT NULL,
    description     TEXT NOT NULL,
    assigned_role   VARCHAR(32) NOT NULL,
    state           VARCHAR(32) NOT NULL DEFAULT 'planned',
    risk_level      VARCHAR(16) NOT NULL DEFAULT 'low',
    depends_on      UUID[] DEFAULT '{}',
    retry_count     INTEGER NOT NULL DEFAULT 0,
    result          JSONB,
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_steps_task ON steps(task_id);
CREATE INDEX idx_steps_state ON steps(state);

-- Checkpoints table
CREATE TABLE checkpoints (
    id              UUID PRIMARY KEY,
    task_id         UUID NOT NULL REFERENCES tasks(id),
    step_id         UUID NOT NULL REFERENCES steps(id),
    seq             INTEGER NOT NULL,
    state           VARCHAR(32) NOT NULL,
    conversation    JSONB NOT NULL,     -- serialized Vec<Message>
    tool_results    JSONB NOT NULL DEFAULT '{}',
    modified_files  JSONB NOT NULL DEFAULT '[]',
    token_usage     JSONB NOT NULL DEFAULT '{}',
    saved_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(step_id, seq)
);

CREATE INDEX idx_checkpoints_task ON checkpoints(task_id);

-- Events table (append-only)
CREATE TABLE events (
    id              BIGSERIAL PRIMARY KEY,
    task_id         UUID NOT NULL REFERENCES tasks(id),
    event_type      VARCHAR(64) NOT NULL,
    payload         JSONB NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_events_task ON events(task_id);
CREATE INDEX idx_events_type ON events(event_type);
```

### 4.2 SqliteTaskStore Implementation

```rust
pub struct SqliteTaskStore {
    pool: SqlitePool,
}

impl SqliteTaskStore {
    pub async fn new(database_url: &str) -> Result<Self, StoreError> {
        let pool = SqlitePool::connect(database_url).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl TaskStore for SqliteTaskStore {
    async fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO tasks (id, session_id, description, state, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5)"
        )
        .bind(task.id.to_string())
        .bind(task.session_id.to_string())
        .bind(&task.description)
        .bind(serde_json::to_string(&task.state)?)
        .bind(serde_json::to_value(&task.metadata)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn load_task(&self, task_id: TaskId) -> Result<Task, StoreError> {
        let row = sqlx::query_as!(
            TaskRow,
            "SELECT * FROM tasks WHERE id = ?1",
            task_id.to_string()
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(Task::from_row(row)?)
    }

    async fn update_task_state(
        &self,
        task_id: TaskId,
        state: TaskState,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE tasks SET state = ?1, updated_at = NOW() WHERE id = ?2"
        )
        .bind(serde_json::to_string(&state)?)
        .bind(task_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_plan(&self, plan: &Plan) -> Result<(), StoreError> {
        let steps_json = serde_json::to_value(&plan.steps)?;
        sqlx::query(
            "INSERT INTO plans (id, task_id, revision, planner_type, steps) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (task_id, revision) DO UPDATE SET steps = ?5"
        )
        .bind(plan.id.to_string())
        .bind(plan.task_id.to_string())
        .bind(plan.revision as i32)
        .bind(serde_json::to_string(&plan.planner)?)
        .bind(steps_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_checkpoint(
        &self,
        task_id: TaskId,
        step_id: StepId,
        checkpoint: &Checkpoint,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO checkpoints (id, task_id, step_id, seq, state, \
             conversation, tool_results, modified_files, token_usage) \
             VALUES (?1, ?2, ?3, \
             (SELECT COALESCE(MAX(seq), 0) + 1 FROM checkpoints WHERE step_id = ?3), \
             ?4, ?5, ?6, ?7, ?8)"
        )
        .bind(Uuid::new_v4().to_string())
        .bind(task_id.to_string())
        .bind(step_id.to_string())
        .bind(serde_json::to_string(&checkpoint.state)?)
        .bind(serde_json::to_value(&checkpoint.conversation)?)
        .bind(serde_json::to_value(&checkpoint.tool_results)?)
        .bind(serde_json::to_value(&checkpoint.modified_files)?)
        .bind(serde_json::to_value(&checkpoint.token_usage)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn append_event(&self, event: TaskEvent) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO events (task_id, event_type, payload) VALUES (?1, ?2, ?3)"
        )
        .bind(event.task_id().to_string())
        .bind(event.event_type())
        .bind(serde_json::to_value(&event)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

### 4.3 PostgreSQL TaskStore (for production clusters)

```rust
pub struct PostgresTaskStore {
    pool: PgPool,
}

impl PostgresTaskStore {
    pub async fn new(database_url: &str) -> Result<Self, StoreError> {
        let pool = PgPool::connect(database_url).await?;
        sqlx::migrate!("./migrations/pg").run(&pool).await?;
        Ok(Self { pool })
    }

    /// LISTEN/NOTIFY for real-time cross-node coordination.
    pub async fn listen_for_task_events(
        &self,
    ) -> Result<impl Stream<Item = TaskEvent>, StoreError> {
        let mut conn = self.pool.acquire().await?;
        conn.execute("LISTEN task_events").await?;

        // Use PostgreSQL LISTEN/NOTIFY for cross-node event propagation
        let stream = async_stream::stream! {
            loop {
                let notification = conn.notifications().recv().await;
                if let Ok(msg) = notification {
                    if let Ok(event) = serde_json::from_str::<TaskEvent>(&msg.payload()) {
                        yield event;
                    }
                }
            }
        };
        Ok(stream)
    }
}
```

---

## 5. Networked AgentMessageBus

### 5.1 Current: In-Process Bus

```rust
// Current: tokio broadcast + mpsc channels (in-process only)
pub struct AgentMessageBus {
    broadcast: broadcast::Sender<AgentMessage>,
    direct_routes: DashMap<AgentId, mpsc::Sender<AgentMessage>>,
    pending: DashMap<CorrelationId, oneshot::Sender<AgentMessage>>,
}
```

### 5.2 Future: Network Transport

The bus extends to support network communication between nodes:

```rust
pub enum BusTransport {
    /// In-process (current implementation).
    Local(LocalBus),
    /// Network via gRPC.
    Grpc(GrpcBus),
    /// Network via NATS messaging.
    Nats(NatsBus),
    /// Hybrid: local for same-node, network for cross-node.
    Hybrid(HybridBus),
}

pub struct HybridBus {
    /// Local broadcast for same-node messages.
    local: LocalBus,
    /// Network transport for cross-node messages.
    network: Arc<dyn NetworkTransport>,
    /// Node identity.
    node_id: NodeId,
    /// Topic routing: which messages go to which nodes.
    router: MessageRouter,
}

impl HybridBus {
    pub async fn publish(&self, msg: AgentMessage) {
        // Always publish locally first
        self.local.publish(msg.clone());

        // Route to remote nodes if needed
        let targets = self.router.route(&msg);
        for target_node in targets {
            if target_node != self.node_id {
                self.network.send(target_node, msg.clone()).await.ok();
            }
        }
    }

    pub async fn request(
        &self,
        from: AgentId,
        to: AgentId,
        payload: RequestPayload,
        timeout: Duration,
    ) -> Result<AgentMessage, BusError> {
        // Check if target agent is local
        if self.local.has_agent(&to) {
            self.local.request(from, to, payload, timeout).await
        } else {
            // Route to remote node
            let target_node = self.router.agent_location(&to)
                .ok_or(BusError::AgentNotFound(to.clone()))?;
            self.network.request(target_node, from, to, payload, timeout).await
        }
    }
}
```

### 5.3 gRPC Transport

```protobuf
// agent_bus.proto

syntax = "proto3";
package xaft.bus;

service AgentBusService {
    // Publish a message to the bus.
    rpc Publish(BusMessage) returns (PublishAck);

    // Send a direct request and await response.
    rpc Request(BusRequest) returns (BusResponse);

    // Subscribe to broadcast messages.
    rpc Subscribe(SubscribeRequest) returns (stream BusMessage);

    // Register an agent on this node.
    rpc RegisterAgent(RegisterAgentRequest) returns (RegisterAgentResponse);
}

message BusMessage {
    string message_id = 1;
    string from_agent = 2;
    optional string to_agent = 3;
    string message_type = 4;     // "started", "completed", "request", "response", "handoff"
    bytes payload = 5;           // serialized AgentMessage
    int64 timestamp_ms = 6;
    optional string correlation_id = 7;
}

message BusRequest {
    string correlation_id = 1;
    string from_agent = 2;
    string to_agent = 3;
    bytes payload = 4;
    int32 timeout_ms = 5;
}

message BusResponse {
    string correlation_id = 1;
    string from_agent = 2;
    bytes payload = 3;
    int64 latency_ms = 4;
}

message PublishAck {
    bool ok = 1;
}

message SubscribeRequest {
    string node_id = 1;
    repeated string message_types = 2;   // filter
}

message RegisterAgentRequest {
    string agent_id = 1;
    string role = 2;
    string node_id = 3;
}

message RegisterAgentResponse {
    bool ok = 1;
}
```

### 5.4 gRPC Client/Server Implementation

```rust
pub struct GrpcBus {
    node_id: NodeId,
    /// gRPC clients for remote nodes.
    clients: DashMap<NodeId, agent_bus_service_client::AgentBusServiceClient<Channel>>,
    /// Local receiver for messages from remote nodes.
    inbound_rx: mpsc::Receiver<AgentMessage>,
    inbound_tx: mpsc::Sender<AgentMessage>,
}

impl GrpcBus {
    pub async fn connect_to_node(
        &self,
        node_id: NodeId,
        address: String,
    ) -> Result<(), BusError> {
        let client = agent_bus_service_client::AgentBusServiceClient::connect(address).await
            .map_err(|e| BusError::ConnectionFailed(e.to_string()))?;
        self.clients.insert(node_id, client);
        Ok(())
    }

    pub async fn send(
        &self,
        target_node: NodeId,
        msg: AgentMessage,
    ) -> Result<(), BusError> {
        let mut client = self.clients.get(&target_node)
            .ok_or(BusError::NodeNotFound(target_node))?
            .clone();

        let grpc_msg = BusMessage::from_agent_message(&msg, &self.node_id);
        client.publish(grpc_msg).await
            .map_err(|e| BusError::SendFailed(e.to_string()))?;
        Ok(())
    }

    pub async fn request(
        &self,
        target_node: NodeId,
        from: AgentId,
        to: AgentId,
        payload: RequestPayload,
        timeout: Duration,
    ) -> Result<AgentMessage, BusError> {
        let mut client = self.clients.get(&target_node)
            .ok_or(BusError::NodeNotFound(target_node))?
            .clone();

        let request = BusRequest {
            correlation_id: Uuid::new_v4().to_string(),
            from_agent: from.to_string(),
            to_agent: to.to_string(),
            payload: serde_json::to_vec(&payload).unwrap(),
            timeout_ms: timeout.as_millis() as i32,
        };

        let response = tokio::time::timeout(
            timeout + Duration::from_secs(5), // extra buffer
            client.request(request),
        ).await
            .map_err(|_| BusError::Timeout(timeout))?
            .map_err(|e| BusError::RemoteError(e.to_string()))?;

        let msg = AgentMessage::from_grpc_response(&response)?;
        Ok(msg)
    }
}
```

---

## 6. Actor Model on Existing Primitives

### 6.1 Actor Abstraction

xauft's agents naturally map to the **actor model**: each agent is an isolated
unit of computation with a mailbox (inbound message channel), state, and the
ability to send messages to other actors.

```
┌────────────────────────────────────────────────────────┐
│                     Actor Model                         │
│                                                        │
│  ┌──────────┐     ┌──────────┐     ┌──────────┐      │
│  │  Agent   │     │  Agent   │     │  Agent   │      │
│  │  Actor   │     │  Actor   │     │  Actor   │      │
│  │          │     │          │     │          │      │
│  │ mailbox: │     │ mailbox: │     │ mailbox: │      │
│  │ [msg,    │     │ [msg]    │     │ [msg,    │      │
│  │  msg]    │     │          │     │  msg,    │      │
│  │          │     │          │     │  msg]    │      │
│  │ state:   │     │ state:   │     │ state:   │      │
│  │ context  │     │ context  │     │ context  │      │
│  └────┬─────┘     └────┬─────┘     └────┬─────┘      │
│       │                │                │             │
│       └────────────────┼────────────────┘             │
│                        │                              │
│                 ┌──────▼──────┐                       │
│                 │   Router    │                       │
│                 │  (MessageBus│                       │
│                 │   + network)│                       │
│                 └─────────────┘                       │
└────────────────────────────────────────────────────────┘
```

### 6.2 Actor Trait

```rust
/// Core actor trait that all xauft agents implement.
#[async_trait]
pub trait Actor: Send + Sync + 'static {
    type Message: Send + 'static;

    /// Handle an incoming message.
    async fn handle(&mut self, msg: Self::Message, ctx: &mut ActorContext);

    /// Called when the actor starts.
    async fn on_start(&mut self, _ctx: &mut ActorContext) {}

    /// Called when the actor stops.
    async fn on_stop(&mut self, _ctx: &mut ActorContext) {}
}

pub struct ActorContext {
    /// This actor's ID.
    actor_id: AgentId,
    /// Reference to the message bus.
    bus: AgentMessageBus,
    /// Ability to spawn child actors.
    spawner: ActorSpawner,
    /// Scheduled message support.
    scheduler: MessageScheduler,
    /// Persistence handle.
    persistence: Arc<dyn ActorPersistence>,
}

pub struct ActorRef<M: Send + 'static> {
    sender: mpsc::Sender<M>,
    actor_id: AgentId,
}

impl<M: Send + 'static> ActorRef<M> {
    /// Send a message to this actor.
    pub async fn tell(&self, msg: M) {
        self.sender.send(msg).await.ok();
    }

    /// Send a message and await a response.
    pub async fn ask<T: Send + 'static>(
        &self,
        msg: M,
        timeout: Duration,
    ) -> Result<T, AskError> {
        let (tx, rx) = oneshot::channel();
        // Wrap message with response channel
        // ... implementation depends on message type
        tokio::time::timeout(timeout, rx).await?
            .map_err(|_| AskError::ActorStopped)
    }
}
```

### 6.3 Actor System

```rust
pub struct ActorSystem {
    /// Registry of running actors.
    actors: DashMap<AgentId, ActorHandle>,
    /// Message bus for inter-actor communication.
    bus: AgentMessageBus,
    /// Configuration.
    config: ActorSystemConfig,
}

struct ActorHandle {
    /// Channel to send messages to the actor.
    mailbox_tx: Box<dyn Any + Send>,
    /// Handle to the actor's task.
    task: JoinHandle<()>,
}

impl ActorSystem {
    /// Spawn a new actor.
    pub async fn spawn<A: Actor>(&self, actor: A, id: AgentId) -> ActorRef<A::Message> {
        let (tx, rx) = mpsc::channel::<A::Message>(256);
        let bus = self.bus.clone();
        let mut ctx = ActorContext {
            actor_id: id.clone(),
            bus,
            spawner: ActorSpawner::new(self),
            scheduler: MessageScheduler::new(),
            persistence: Arc::new(NoopPersistence),
        };

        let task = tokio::spawn(async move {
            let mut actor = actor;
            actor.on_start(&mut ctx).await;

            while let Some(msg) = rx.recv().await {
                actor.handle(msg, &mut ctx).await;
            }

            actor.on_stop(&mut ctx).await;
        });

        self.actors.insert(id.clone(), ActorHandle {
            mailbox_tx: Box::new(tx.clone()),
            task,
        });

        ActorRef {
            sender: tx,
            actor_id: id,
        }
    }

    /// Stop an actor.
    pub async fn stop(&self, id: &AgentId) -> Result<(), ActorError> {
        if let Some((_, handle)) = self.actors.remove(id) {
            handle.task.abort();
        }
        Ok(())
    }
}
```

### 6.4 Agent as Actor

```rust
/// Agent actor implementation.
pub struct AgentActor<P: LlmProvider> {
    agent: Agent<P>,
    context: AgentExecutionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentMessage {
    ExecuteStep { step: PlannedStep },
    Handoff { from: AgentId, context: HandoffTransferContext },
    ApprovalResponse { decision: ApprovalDecision },
    Cancel,
}

#[async_trait]
impl<P: LlmProvider + Clone + 'static> Actor for AgentActor<P> {
    type Message = AgentMessage;

    async fn handle(&mut self, msg: Self::Message, ctx: &mut ActorContext) {
        match msg {
            AgentMessage::ExecuteStep { step } => {
                let result = self.agent.execute_step(&step).await;
                ctx.bus.publish(AgentMessage::Completed {
                    agent_id: ctx.actor_id.clone(),
                    step_id: step.id,
                    result: result.unwrap_or_else(|e| StepResult::Error(e.to_string())),
                });
            }
            AgentMessage::Handoff { from, context } => {
                self.agent.receive_context(&context).await.ok();
                // Agent continues from where the previous agent left off
            }
            AgentMessage::ApprovalResponse { decision } => {
                // Handle approval/rejection
            }
            AgentMessage::Cancel => {
                // Clean up and stop
            }
        }
    }
}
```

---

## 7. Multi-Machine Sessions

### 7.1 Session Spanning

A xauft session can span multiple machines when:

1. **Task requires more compute** than a single machine provides.
2. **Different models are on different machines** (e.g., local Ollama + remote API).
3. **Fault tolerance** — if one machine fails, another takes over.
4. **Geographic distribution** — reduce latency to different LLM APIs.

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Machine A     │     │   Machine B     │     │   Machine C     │
│   (US-East)     │     │   (EU-West)     │     │   (Local)       │
│                 │     │                 │     │                 │
│  ┌───────────┐  │     │  ┌───────────┐  │     │  ┌───────────┐  │
│  │ Session   │  │     │  │ Session   │  │     │  │ Session   │  │
│  │ Manager   │  │     │  │ Manager   │  │     │  │ Manager   │  │
│  └─────┬─────┘  │     │  └─────┬─────┘  │     │  └─────┬─────┘  │
│        │        │     │        │        │     │        │        │
│  ┌─────▼─────┐  │     │  ┌─────▼─────┐  │     │  ┌─────▼─────┐  │
│  │ Agents    │  │     │  │ Agents    │  │     │  │ Agents    │  │
│  │ [Coder]  │  │     │  │ [QA]      │  │     │  │ [Ollama]  │  │
│  │ [Planner]│  │     │  │ [Fixer]   │  │     │  │ [Local]   │  │
│  └───────────┘  │     │  └───────────┘  │     │  └───────────┘  │
│        │        │     │        │        │     │        │        │
│  ┌─────▼─────┐  │     │  ┌─────▼─────┐  │     │  ┌─────▼─────┐  │
│  │ OpenAI    │  │     │  │ Anthropic │  │     │  │ Ollama    │  │
│  │ Provider  │  │     │  │ Provider  │  │     │  │ Provider  │  │
│  └───────────┘  │     │  └───────────┘  │     │  └───────────┘  │
└────────┬────────┘     └────────┬────────┘     └────────┬────────┘
         │                       │                       │
         └───────────────────────┼───────────────────────┘
                                 │
                          ┌──────▼──────┐
                          │  Shared     │
                          │  TaskStore  │
                          │  (Postgres) │
                          └─────────────┘
```

### 7.2 Session Coordination Protocol

```rust
pub struct DistributedSessionManager {
    node_id: NodeId,
    store: Arc<dyn TaskStore>,
    bus: Arc<HybridBus>,
    node_registry: Arc<NodeRegistry>,
}

pub struct NodeRegistry {
    nodes: DashMap<NodeId, NodeInfo>,
    heartbeat_interval: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    id: NodeId,
    address: String,
    region: String,
    capabilities: NodeCapabilities,
    last_heartbeat: DateTime<Utc>,
    load: NodeLoad,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    available_roles: Vec<AgentRole>,
    available_providers: Vec<String>,
    max_concurrent_agents: usize,
    gpu_available: bool,
    memory_gb: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLoad {
    active_agents: usize,
    pending_tasks: usize,
    cpu_percent: f64,
    memory_percent: f64,
}

impl DistributedSessionManager {
    /// Assign a step to the best available node.
    pub async fn assign_step(&self, step: &PlannedStep) -> Result<NodeId, SessionError> {
        let candidates: Vec<NodeInfo> = self.node_registry.nodes.iter()
            .filter(|entry| {
                let info = entry.value();
                info.capabilities.available_roles.contains(&step.assigned_role)
                    && info.load.active_agents < info.capabilities.max_concurrent_agents
            })
            .map(|entry| entry.value().clone())
            .collect();

        if candidates.is_empty() {
            return Err(SessionError::NoAvailableNode(step.assigned_role));
        }

        // Select least-loaded node
        let best = candidates.into_iter()
            .min_by_key(|n| n.load.active_agents)
            .unwrap();

        Ok(best.id)
    }

    /// Run heartbeat loop.
    pub async fn heartbeat_loop(&self) {
        let mut interval = tokio::time::interval(self.node_registry.heartbeat_interval);
        loop {
            interval.tick().await;
            // Send heartbeat to all known nodes
            for entry in self.node_registry.nodes.iter() {
                let node = entry.value();
                if let Ok(mut client) = agent_bus_service_client::AgentBusServiceClient::connect(
                    format!("http://{}", node.address)
                ).await {
                    let _ = client.heartbeat(HeartbeatReq {
                        node_id: self.node_id.to_string(),
                        load: Some(self.current_load().into()),
                    }).await;
                }
            }
        }
    }
}
```

---

## 8. Security Considerations

### 8.1 Authentication and Authorization

```rust
pub struct DistributedAuth {
    /// JWT-based authentication for API access.
    jwt_validator: JwtValidator,
    /// API key for inter-node communication.
    node_api_key: String,
    /// Role-based access control.
    rbac: RbacPolicy,
}

#[derive(Debug, Clone)]
pub struct RbacPolicy {
    rules: Vec<RbacRule>,
}

#[derive(Debug, Clone)]
struct RbacRule {
    role: UserRole,
    resource: ResourcePattern,
    action: Action,
    effect: Effect,
}

#[derive(Debug, Clone, Copy)]
enum UserRole { Admin, Developer, Viewer }
#[derive(Debug, Clone, Copy)]
enum Effect { Allow, Deny }
```

### 8.2 Transport Security

- All inter-node communication uses **TLS** (mTLS for node-to-node).
- API endpoints enforce **HTTPS** with valid certificates.
- gRPC channels use **TLS encryption**.
- Database connections use **SSL**.

---

## 9. Deployment Topology

### 9.1 Single-Region Deployment

```
  ┌───────────────────────────────────────────┐
  │              Kubernetes Cluster            │
  │                                           │
  │  ┌─────────┐  ┌─────────┐  ┌─────────┐  │
  │  │ xauft   │  │ xauft   │  │ xauft   │  │
  │  │ Node 1  │  │ Node 2  │  │ Node 3  │  │
  │  └────┬────┘  └────┬────┘  └────┬────┘  │
  │       │             │             │       │
  │  ┌────▼─────────────▼─────────────▼────┐  │
  │  │          PostgreSQL (HA)             │  │
  │  └─────────────────────────────────────┘  │
  │                                           │
  │  ┌─────────────────────────────────────┐  │
  │  │          NATS / gRPC Bus            │  │
  │  └─────────────────────────────────────┘  │
  └───────────────────────────────────────────┘
```

### 9.2 Multi-Region Deployment

```
  US-East                    EU-West
  ┌──────────────┐          ┌──────────────┐
  │ xauft Nodes  │          │ xauft Nodes  │
  │ (OpenAI)     │          │ (Anthropic)  │
  └──────┬───────┘          └──────┬───────┘
         │                         │
         │    ┌──────────────┐     │
         └───▶│  CockroachDB │◀────┘
              │  (multi-region)│
              └──────────────┘
```

---

## 10. Configuration Reference

```toml
[xaft.distributed]
enabled = false
node_id = "node-1"
cluster_name = "xaft-prod"

[xaft.distributed.api]
bind_address = "0.0.0.0"
port = 8080
tls_cert = "/etc/xaft/tls/cert.pem"
tls_key = "/etc/xaft/tls/key.pem"

[xaft.distributed.grpc]
enabled = true
bind_address = "0.0.0.0"
port = 9090
max_message_size_bytes = 16777216    # 16 MB

[xaft.distributed.database]
url = "postgresql://xaft:password@localhost:5432/xaft"
max_connections = 20
min_connections = 5

[xaft.distributed.bus]
transport = "hybrid"          # "local" | "grpc" | "nats" | "hybrid"
nats_url = "nats://localhost:4222"

[xaft.distributed.heartbeat]
interval_secs = 5
timeout_secs = 15
dead_node_threshold_secs = 30

[xaft.distributed.auth]
jwt_secret = "${JWT_SECRET}"
node_api_key = "${NODE_API_KEY}"
```
