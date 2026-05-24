# Multi-Agent Coordination

> How xauft orchestrates multiple agents using the `agtrs` framework primitives:
> `TeamMode`, `AgentMessageBus`, `SubagentTool<T>`, `SubagentPool`, and
> `HandoffOrchestrator`.

---

## 1. Overview

xauft is not a single monolithic LLM caller. It decomposes complex coding tasks
across a **team of specialised agents** that communicate through structured
message passing. The `agtrs` framework provides the coordination layer; xauft
defines the agent roles, tool sets, and routing policies on top.

```
┌─────────────────────────────────────────────────────────────┐
│                        xauft Session                        │
│                                                             │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌─────────┐ │
│  │ Planner  │   │  Coder   │   │Reviewer/QA│   │ Fixer   │ │
│  │ Agent    │   │  Agent   │   │  Agent    │   │ Agent   │ │
│  └────┬─────┘   └────┬─────┘   └────┬──────┘   └────┬────┘ │
│       │              │              │                │      │
│       └──────────────┴──────────────┴────────────────┘      │
│                         │                                    │
│                ┌────────▼────────┐                           │
│                │ AgentMessageBus │                           │
│                │  (async router) │                           │
│                └────────┬────────┘                           │
│                         │                                    │
│              ┌──────────▼──────────┐                         │
│              │ HandoffOrchestrator │                         │
│              │  + SubagentPool     │                         │
│              └─────────────────────┘                         │
└─────────────────────────────────────────────────────────────┘
```

---

## 2. TeamMode Enum

The `TeamMode` enum selects the coordination topology at session start.

```rust
/// Determines how multiple agents coordinate within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamMode {
    /// A central coordinator dispatches sub-tasks to worker agents.
    /// Workers do NOT communicate directly with each other.
    Coordinator,

    /// Agents contribute sequentially, each building on the prior agent's
    /// output. A synthesis step merges all contributions at the end.
    Collaborate,
}
```

### 2.1 TeamMode::Coordinator

In **Coordinator** mode a single *coordinator agent* acts as the dispatcher.
It receives the user prompt, decomposes it into sub-tasks, and assigns each
sub-task to a specialised worker agent. Workers return results to the
coordinator, which may issue follow-up tasks or synthesise a final answer.

```
                    User Prompt
                        │
                        ▼
              ┌─────────────────┐
              │   Coordinator   │  ← uses OneShotPlanner to
              │     Agent       │    decompose into PlanSteps
              └────────┬────────┘
                       │
          ┌────────────┼────────────┐
          │            │            │
          ▼            ▼            ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ Worker A │ │ Worker B │ │ Worker C │
    │ (Coder)  │ │ (QA)     │ │ (Fixer)  │
    └────┬─────┘ └────┬─────┘ └────┬─────┘
         │             │             │
         └─────────────┼─────────────┘
                       │
                       ▼
              ┌─────────────────┐
              │   Coordinator   │  ← merges results,
              │   (synthesis)   │    may re-dispatch
              └─────────────────┘
```

**Key properties:**

| Property              | Value                                   |
|-----------------------|-----------------------------------------|
| Communication pattern | Star (coordinator ↔ worker)             |
| Worker autonomy       | Low — workers only respond to tasks     |
| Context sharing       | Coordinator controls what each worker sees |
| Parallelism           | Workers execute in parallel via SubagentPool |
| Failure handling      | Coordinator retries or reassigns tasks  |
| Token cost            | Lower (workers see only relevant slice) |

**Rust sketch — CoordinatorExecutor integration:**

```rust
pub struct CoordinatorExecutor<P: LlmProvider> {
    provider: P,
    mode: TeamMode::Coordinator,
    pool: SubagentPool<P>,
    bus: AgentMessageBus,
    planner: Box<dyn Planner>,
    /// Maximum concurrent workers
    max_concurrency: usize,
    /// Semaphore to enforce concurrency limit
    semaphore: Arc<Semaphore>,
}

impl<P: LlmProvider> CoordinatorExecutor<P> {
    pub async fn execute(&self, task: Task) -> Result<TaskOutcome, ExecutorError> {
        // 1. Coordinator agent plans the decomposition
        let plan: Plan = self.planner.plan(&task).await?;

        // 2. Dispatch plan steps to workers
        let mut join_set = JoinSet::new();
        for step in plan.steps {
            let permit = self.semaphore.acquire().await?;
            let worker = self.pool.acquire_agent(step.required_role()).await?;
            let bus = self.bus.clone();
            join_set.spawn(async move {
                let _permit = permit;
                let result = worker.execute(step).await;
                bus.publish(AgentMessage::Completed {
                    agent_id: worker.id(),
                    step_id: step.id,
                    result: result.clone(),
                });
                result
            });
        }

        // 3. Collect worker results
        let mut results = Vec::new();
        while let Some(outcome) = join_set.join_next().await {
            results.push(outcome??);
        }

        // 4. Coordinator synthesises
        let synthesis = self.synthesise(&task, &results).await?;
        Ok(TaskOutcome::Completed(synthesis))
    }
}
```

### 2.2 TeamMode::Collaborate

In **Collaborate** mode agents work sequentially. Each agent receives the
full conversation context plus the output of all prior agents. A final
*synthesis agent* merges contributions.

```
    ┌────────┐      ┌────────┐      ┌────────┐      ┌──────────┐
    │ Agent  │─────▶│ Agent  │─────▶│ Agent  │─────▶│Synthesis │
    │   A    │      │   B    │      │   C    │      │  Agent   │
    └────────┘      └────────┘      └────────┘      └──────────┘
         │               │               │
         ▼               ▼               ▼
    contribution_A  contribution_B  contribution_C
         │               │               │
         └───────────────┴───────────────┘
                         │
                         ▼
                   Final Merged Output
```

**Key properties:**

| Property              | Value                                       |
|-----------------------|---------------------------------------------|
| Communication pattern | Linear chain                                |
| Agent autonomy        | Medium — each agent can decide what to add  |
| Context sharing       | Full — every agent sees all prior output    |
| Parallelism           | None (sequential)                           |
| Failure handling      | Skip failed agent or abort chain            |
| Token cost            | Higher (each agent sees full history)       |

```rust
pub struct CollaborateExecutor<P: LlmProvider> {
    provider: P,
    agents: Vec<AgentConfig>,
    synthesiser: AgentConfig,
    bus: AgentMessageBus,
}

impl<P: LlmProvider> CollaborateExecutor<P> {
    pub async fn execute(&self, task: Task) -> Result<TaskOutcome, ExecutorError> {
        let mut conversation = Conversation::from_task(&task);
        let mut contributions: Vec<AgentContribution> = Vec::new();

        for agent_config in &self.agents {
            let agent = Agent::new(agent_config, &self.provider);
            let response = agent.chat(conversation.clone()).await?;

            // Record contribution
            let contribution = AgentContribution {
                agent_id: agent_config.id.clone(),
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
            };
            contributions.push(contribution.clone());

            // Append to conversation so next agent sees it
            conversation.push_message(Message::assistant(
                format!("[{}]: {}", agent_config.id, response.content)
            ));

            // Publish on bus for observability
            self.bus.publish(AgentMessage::Contributed {
                agent_id: agent_config.id.clone(),
                contribution,
            });
        }

        // Synthesis step
        let synthesiser = Agent::new(&self.synthesiser, &self.provider);
        let final_output = synthesiser.chat(conversation).await?;
        Ok(TaskOutcome::Completed(final_output.content))
    }
}
```

### 2.3 Mode Selection Heuristics

xauft selects `TeamMode` based on task characteristics:

```rust
impl TeamMode {
    pub fn select_for(task: &Task) -> TeamMode {
        match task.complexity() {
            Complexity::Simple => TeamMode::Coordinator, // single worker
            Complexity::Decomposable => TeamMode::Coordinator, // parallel workers
            Complexity::Sequential => TeamMode::Collaborate,   // chain
            Complexity::Exploratory => TeamMode::Collaborate,  // build on prior
        }
    }
}
```

| Signal                     | Coordinator | Collaborate |
|----------------------------|:-----------:|:-----------:|
| Independent sub-tasks      | ✅          |             |
| Dependencies between steps |             | ✅          |
| Parallel execution benefit | ✅          |             |
| Need iterative refinement  |             | ✅          |
| Large codebase search      | ✅          |             |
| Creative / design tasks    |             | ✅          |

---

## 3. AgentMessageBus

The `AgentMessageBus` is the central nervous system of multi-agent xauft. It
enables **direct agent-to-agent communication** without requiring the
coordinator as a relay.

### 3.1 Architecture

```
┌─────────────────────────────────────────────────────┐
│                  AgentMessageBus                     │
│                                                     │
│  ┌───────────────────────────────────────────────┐  │
│  │            Broadcast Channel (tokio)           │  │
│  └────────────────────┬──────────────────────────┘  │
│                       │                              │
│  ┌────────────┐  ┌────▼────┐  ┌────────────┐       │
│  │ Agent A    │  │ Agent B │  │ Agent C    │       │
│  │ rx handle  │  │ rx handle│  │ rx handle  │       │
│  └────────────┘  └─────────┘  └────────────┘       │
│                                                     │
│  ┌───────────────────────────────────────────────┐  │
│  │         Direct Message Routes                  │  │
│  │  agent_id → mpsc::Sender<AgentMessage>        │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

### 3.2 Message Types

```rust
/// Messages exchanged between agents on the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentMessage {
    /// Broadcast: agent started working on a task.
    Started {
        agent_id: AgentId,
        task_id: TaskId,
        timestamp: DateTime<Utc>,
    },

    /// Broadcast: agent completed a task.
    Completed {
        agent_id: AgentId,
        step_id: StepId,
        result: StepResult,
    },

    /// Broadcast: agent encountered an error.
    Failed {
        agent_id: AgentId,
        step_id: StepId,
        error: String,
    },

    /// Direct: request another agent to perform work.
    Request {
        /// Correlation ID for matching responses.
        correlation_id: CorrelationId,
        from: AgentId,
        to: AgentId,
        payload: RequestPayload,
        /// Deadline for the response.
        deadline: Option<Instant>,
    },

    /// Direct: response to a prior Request.
    Response {
        correlation_id: CorrelationId,
        from: AgentId,
        to: AgentId,
        payload: ResponsePayload,
        /// Time taken to produce this response.
        latency: Duration,
    },

    /// Broadcast: agent is handing off to another agent.
    Handoff {
        from: AgentId,
        to: AgentId,
        reason: HandoffReason,
        context: HandoffContext,
    },

    /// Broadcast: progress update.
    Progress {
        agent_id: AgentId,
        step_id: StepId,
        percentage: u8,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorrelationId(Uuid);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPayload {
    pub task_description: String,
    pub tool_hints: Vec<String>,
    pub priority: Priority,
    pub context_snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsePayload {
    pub result: StepResult,
    pub confidence: f64,
    pub follow_up: Option<Box<RequestPayload>>,
}
```

### 3.3 Request/Response Pattern with Correlated IDs

The bus implements a **correlated request/response** pattern so agents can
directly query each other without blocking the event loop:

```rust
pub struct AgentMessageBus {
    /// Broadcast channel for pub/sub messages.
    broadcast: broadcast::Sender<AgentMessage>,
    /// Direct message routes: agent_id → sender.
    direct_routes: DashMap<AgentId, mpsc::Sender<AgentMessage>>,
    /// Pending request correlations: correlation_id → oneshot channel.
    pending: DashMap<CorrelationId, oneshot::Sender<AgentMessage>>,
}

impl AgentMessageBus {
    /// Send a request to a specific agent and await the response.
    pub async fn request(
        &self,
        from: AgentId,
        to: AgentId,
        payload: RequestPayload,
        timeout: Duration,
    ) -> Result<AgentMessage, BusError> {
        let correlation_id = CorrelationId(Uuid::new_v4());
        let (tx, rx) = oneshot::channel();

        // Register pending correlation
        self.pending.insert(correlation_id, tx);

        // Send direct message
        let sender = self.direct_routes.get(&to)
            .ok_or(BusError::AgentNotFound(to.clone()))?;
        sender.send(AgentMessage::Request {
            correlation_id,
            from,
            to,
            payload,
            deadline: Some(Instant::now() + timeout),
        }).await.map_err(|_| BusError::SendFailed)?;

        // Await response with timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(BusError::ChannelClosed),
            Err(_) => {
                self.pending.remove(&correlation_id);
                Err(BusError::Timeout(timeout))
            }
        }
    }

    /// Handle an incoming response by correlating it to a pending request.
    pub fn handle_response(&self, msg: &AgentMessage) {
        if let AgentMessage::Response { correlation_id, .. } = msg {
            if let Some((_, sender)) = self.pending.remove(correlation_id) {
                let _ = sender.send(msg.clone());
            }
        }
    }

    /// Publish a broadcast message to all subscribers.
    pub fn publish(&self, msg: AgentMessage) {
        let _ = self.broadcast.send(msg);
    }

    /// Subscribe to broadcast messages.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentMessage> {
        self.broadcast.subscribe()
    }

    /// Register an agent with a direct message channel.
    pub fn register_agent(
        &self,
        agent_id: AgentId,
        buffer: usize,
    ) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(buffer);
        self.direct_routes.insert(agent_id, tx);
        rx
    }
}
```

### 3.4 Data Flow: Coordinator Dispatch

```
  ┌────────────┐         ┌──────────────┐         ┌────────────┐
  │ Coordinator│         │ MessageBus   │         │  Worker A  │
  └─────┬──────┘         └──────┬───────┘         └─────┬──────┘
        │                       │                       │
        │  Request(correlation_id=42,                   │
        │          to="worker-a",                       │
        │          payload=TaskPayload)                  │
        │──────────────────────▶│                       │
        │                       │  Deliver Request       │
        │                       │──────────────────────▶│
        │                       │                       │
        │                       │        ...processing...│
        │                       │                       │
        │                       │  Response(correlation_id=42,
        │                       │           from="worker-a",
        │                       │           payload=Result)│
        │                       │◀──────────────────────│
        │  Response(correlation_id=42)                   │
        │◀──────────────────────│                       │
        │                       │                       │
        │  Publish(Completed)   │                       │
        │──────────────────────▶│──────────────────────▶│
        │                       │                       │
```

---

## 4. SubagentTool\<T\>

`SubagentTool<T>` is the primary mechanism for **typed delegation** — one
agent invoking another with a strongly-typed input/output contract.

### 4.1 Design Rationale

Without `SubagentTool<T>`, delegation would be unstructured string exchange.
Typed delegation ensures:

1. **Compile-time safety**: input/output types are checked.
2. **Schema enforcement**: the sub-agent receives a well-defined JSON schema.
3. **Observability**: structured logging of delegation events.
4. **Testing**: sub-agents can be mocked with typed stubs.

### 4.2 Trait Sketch

```rust
/// A tool that delegates work to a sub-agent with typed input/output.
pub struct SubagentTool<TInput, TOutput> {
    name: String,
    description: String,
    agent_config: AgentConfig,
    provider: Arc<dyn LlmProvider>,
    bus: AgentMessageBus,
    _phantom: PhantomData<(TInput, TOutput)>,
}

impl<TInput, TOutput> SubagentTool<TInput, TOutput>
where
    TInput: JsonSchema + DeserializeOwned + Send + Sync + 'static,
    TOutput: JsonSchema + Serialize + Send + Sync + 'static,
{
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        agent_config: AgentConfig,
        provider: Arc<dyn LlmProvider>,
        bus: AgentMessageBus,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agent_config,
            provider,
            bus,
            _phantom: PhantomData,
        }
    }

    /// Execute the sub-agent with typed input, return typed output.
    pub async fn execute(&self, input: TInput) -> Result<TOutput, SubagentError> {
        // 1. Serialize input to JSON
        let input_json = serde_json::to_value(&input)
            .map_err(SubagentError::Serialization)?;

        // 2. Build sub-agent prompt with schema instructions
        let output_schema = schemars::schema_for!(TOutput);
        let system_prompt = format!(
            "You are a specialised sub-agent. You will receive JSON input \
             conforming to the input schema. You MUST produce output as \
             valid JSON conforming to the output schema.\n\n\
             Output Schema:\n```json\n{}\n```",
            serde_json::to_string_pretty(&output_schema)?
        );

        // 3. Create and run sub-agent
        let agent = Agent::with_config(&self.agent_config, self.provider.clone())
            .system_prompt(&system_prompt)
            .structured_output::<TOutput>()
            .build();

        let result = agent.chat(input_json).await?;

        // 4. Deserialize output
        let output: TOutput = serde_json::from_value(result)
            .map_err(SubagentError::Deserialization)?;

        // 5. Publish delegation event
        self.bus.publish(AgentMessage::Completed {
            agent_id: self.agent_config.id.clone(),
            step_id: StepId::new(),
            result: StepResult::Typed(serde_json::to_value(&output)?),
        });

        Ok(output)
    }
}

/// Implement the Tool trait so SubagentTool can be registered with agents.
#[async_trait]
impl<TInput, TOutput> Tool for SubagentTool<TInput, TOutput>
where
    TInput: JsonSchema + DeserializeOwned + Send + Sync + 'static,
    TOutput: JsonSchema + Serialize + Send + Sync + 'static,
{
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn input_schema(&self) -> serde_json::Value {
        schemars::schema_for!(TInput).into()
    }

    async fn call(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let typed_input: TInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let output = self.execute(typed_input).await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let output_json = serde_json::to_value(&output)
            .map_err(|e| ToolError::Serialization(e.to_string()))?;
        Ok(ToolOutput::Json(output_json))
    }
}
```

### 4.3 Usage Example: Code Review Delegation

```rust
#[derive(Debug, JsonSchema, Deserialize)]
struct CodeReviewInput {
    file_path: String,
    diff: String,
    language: String,
}

#[derive(Debug, JsonSchema, Serialize)]
struct CodeReviewOutput {
    issues: Vec<CodeIssue>,
    summary: String,
    approved: bool,
}

#[derive(Debug, JsonSchema, Serialize)]
struct CodeIssue {
    line: u32,
    severity: Severity,
    message: String,
    suggestion: Option<String>,
}

// Register SubagentTool with the coordinator agent
let review_tool = SubagentTool::<CodeReviewInput, CodeReviewOutput>::new(
    "code_review",
    "Delegate code review to a specialised QA sub-agent",
    qa_agent_config,
    provider.clone(),
    bus.clone(),
);

coordinator_agent.register_tool(review_tool);
```

---

## 5. SubagentPool

`SubagentPool` manages a pool of reusable sub-agent instances for **parallel
execution** of independent tasks.

### 5.1 Architecture

```
                    ┌──────────────────┐
                    │  SubagentPool    │
                    │                  │
                    │  available:      │
                    │  ┌──┐┌──┐┌──┐   │
                    │  │A1││A2││A3│   │  ← idle agents
                    │  └──┘└──┘└──┘   │
                    │                  │
                    │  in_flight:      │
                    │  ┌──────────┐   │
                    │  │ A4 → T1  │   │  ← busy agents
                    │  │ A5 → T2  │   │
                    │  └──────────┘   │
                    │                  │
                    │  semaphore: 5   │  ← max concurrency
                    └──────────────────┘
                              │
              ┌───────────────┼───────────────┐
              │               │               │
              ▼               ▼               ▼
         ┌─────────┐   ┌─────────┐   ┌─────────┐
         │ Task T1 │   │ Task T2 │   │ Task T3 │
         │ (Coder) │   │  (QA)   │   │ (Fixer) │
         └─────────┘   └─────────┘   └─────────┘
```

### 5.2 Implementation

```rust
pub struct SubagentPool<P: LlmProvider> {
    provider: Arc<P>,
    /// Role → Vec of pre-configured agent handles
    agents: DashMap<AgentRole, Vec<AgentHandle<P>>>,
    /// Role → available agent indices
    available: DashMap<AgentRole, VecDeque<usize>>,
    /// Concurrency limiter
    semaphore: Arc<Semaphore>,
    /// Message bus for inter-agent communication
    bus: AgentMessageBus,
    /// Configuration for creating new agents on demand
    config: PoolConfig,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum concurrent sub-agents
    pub max_concurrency: usize,
    /// Pre-warm agents on pool creation
    pub prewarm: bool,
    /// Idle timeout before recycling an agent
    pub idle_timeout: Duration,
    /// Maximum tasks per agent before recycling
    pub max_tasks_per_agent: usize,
    /// Provider configuration for sub-agents
    pub subagent_provider: SubagentProviderConfig,
}

struct AgentHandle<P: LlmProvider> {
    agent: Agent<P>,
    tasks_completed: AtomicUsize,
    created_at: Instant,
    state: AtomicAgentState,
}

#[repr(u8)]
enum AgentState {
    Idle = 0,
    Busy = 1,
    Draining = 2,
}

impl<P: LlmProvider + Clone> SubagentPool<P> {
    /// Acquire an agent for a specific role, waiting if necessary.
    pub async fn acquire_agent(
        &self,
        role: AgentRole,
    ) -> Result<PooledAgent<'_, P>, PoolError> {
        let _permit = self.semaphore.acquire().await
            .map_err(|_| PoolError::ConcurrencyLimit)?;

        // Try to get an available agent
        if let Some(mut queue) = self.available.get_mut(&role) {
            if let Some(idx) = queue.pop_front() {
                let agents = self.agents.get(&role).unwrap();
                let handle = &agents[idx];
                handle.state.store(AgentState::Busy);
                return Ok(PooledAgent {
                    handle: &agents[idx],
                    pool: self,
                    role,
                    idx,
                    _permit,
                });
            }
        }

        // Create a new agent if pool not at capacity
        let agent = self.create_agent_for_role(&role).await?;
        // ... register and return

        unimplemented!()
    }

    /// Execute multiple tasks in parallel, returning results in order.
    pub async fn execute_parallel(
        &self,
        tasks: Vec<(AgentRole, Task)>,
    ) -> Vec<Result<StepResult, AgentError>> {
        let mut join_set = JoinSet::new();

        for (role, task) in tasks {
            let pool = self.clone();
            join_set.spawn(async move {
                let agent = pool.acquire_agent(role).await?;
                agent.execute(task).await
            });
        }

        let mut results = Vec::with_capacity(tasks.len());
        while let Some(res) = join_set.join_next().await {
            results.push(res.unwrap());
        }
        results
    }

    async fn create_agent_for_role(&self, role: &AgentRole) -> Result<Agent<P>, PoolError> {
        let config = self.config.subagent_provider.config_for_role(role);
        let agent = Agent::new(&config, self.provider.clone());
        Ok(agent)
    }
}

/// RAII guard that returns the agent to the pool on drop.
pub struct PooledAgent<'a, P: LlmProvider> {
    handle: &'a AgentHandle<P>,
    pool: &'a SubagentPool<P>,
    role: AgentRole,
    idx: usize,
    _permit: SemaphorePermit<'a>,
}

impl<'a, P: LlmProvider> Drop for PooledAgent<'a, P> {
    fn drop(&mut self) {
        self.handle.state.store(AgentState::Idle);
        self.handle.tasks_completed.fetch_add(1, Ordering::Relaxed);
        if let Some(mut queue) = self.pool.available.get_mut(&self.role) {
            queue.push_back(self.idx);
        }
    }
}
```

---

## 6. HandoffOrchestrator: QA→Fixer Loops

The `HandoffOrchestrator` manages **control transfer between agents**,
implementing the critical QA→Fixer loop pattern.

### 6.1 Loop Diagram

```
  ┌────────┐         ┌────────┐         ┌────────┐
  │ Coder  │────────▶│   QA   │────────▶│ Fixer  │
  │ Agent  │  done   │ Agent  │ issues  │ Agent  │
  └────────┘         └────┬───┘         └────┬───┘
                          │                   │
                          │   ┌───────────────┘
                          │   │  fixed code
                          ▼   ▼
                     ┌────────┐
                     │   QA   │  ← re-review fixed code
                     │ Agent  │
                     └────┬───┘
                          │
                          │ approved
                          ▼
                     ┌────────┐
                     │  Done  │
                     └────────┘
```

### 6.2 Implementation

```rust
pub struct HandoffOrchestrator<P: LlmProvider> {
    provider: Arc<P>,
    bus: AgentMessageBus,
    agent_store: HandoffAgentStore,
    /// Maximum QA→Fixer iterations before escalating
    max_iterations: usize,
    /// Agent configurations
    configs: HashMap<AgentRole, AgentConfig>,
}

impl<P: LlmProvider> HandoffOrchestrator<P> {
    /// Execute a task with the QA→Fixer loop.
    pub async fn execute_with_qa_loop(
        &self,
        initial_task: Task,
    ) -> Result<TaskOutcome, OrchestratorError> {
        // 1. Coder produces initial output
        let coder = self.create_agent(AgentRole::Coder).await?;
        let coder_output = coder.execute(&initial_task).await?;

        // 2. QA reviews
        let mut current_output = coder_output;
        let mut iteration = 0;

        loop {
            let qa = self.create_agent(AgentRole::QA).await?;
            let review: CodeReviewOutput = qa.review(&current_output).await?;

            if review.approved {
                self.bus.publish(AgentMessage::Completed {
                    agent_id: qa.id(),
                    step_id: StepId::new(),
                    result: StepResult::Approved(current_output.clone()),
                });
                return Ok(TaskOutcome::Completed(current_output));
            }

            iteration += 1;
            if iteration > self.max_iterations {
                return Err(OrchestratorError::MaxIterationsExceeded {
                    iterations: iteration,
                    remaining_issues: review.issues,
                });
            }

            // 3. Fixer addresses issues
            self.bus.publish(AgentMessage::Handoff {
                from: qa.id(),
                to: AgentId::from_role(AgentRole::Fixer),
                reason: HandoffReason::IssuesFound {
                    issue_count: review.issues.len(),
                },
                context: HandoffContext {
                    issues: review.issues.iter().map(|i| i.message.clone()).collect(),
                    iteration,
                },
            });

            let fixer = self.create_agent(AgentRole::Fixer).await?;
            current_output = fixer.fix(&current_output, &review.issues).await?;

            // 4. Loop back to QA (step 2)
        }
    }

    fn create_agent(&self, role: AgentRole) -> BoxFuture<'_, Agent<P>> {
        let config = self.configs.get(&role).unwrap().clone();
        let provider = self.provider.clone();
        async move { Agent::new(&config, provider) }.boxed()
    }
}
```

### 6.3 HandoffAgentStore

Tracks the currently active agent and handoff history:

```rust
pub struct HandoffAgentStore {
    /// Currently active agent
    active: RwLock<Option<AgentId>>,
    /// History of handoffs for the current task
    history: RwLock<Vec<HandoffRecord>>,
    /// Agent → accumulated context
    contexts: DashMap<AgentId, AgentContext>,
}

#[derive(Debug, Clone)]
pub struct HandoffRecord {
    from: AgentId,
    to: AgentId,
    reason: HandoffReason,
    timestamp: DateTime<Utc>,
    context_transferred: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct AgentContext {
    messages: Vec<Message>,
    tool_results: HashMap<ToolCallId, ToolOutput>,
    artifacts: Vec<Artifact>,
}

impl HandoffAgentStore {
    /// Perform a handoff: update active agent and transfer context.
    pub async fn handoff(
        &self,
        from: AgentId,
        to: AgentId,
        reason: HandoffReason,
        context_messages: Vec<Message>,
    ) {
        // Record history
        let record = HandoffRecord {
            from: from.clone(),
            to: to.clone(),
            reason,
            timestamp: Utc::now(),
            context_transferred: context_messages.clone(),
        };
        self.history.write().await.push(record);

        // Update active agent
        *self.active.write().await = Some(to.clone());

        // Transfer context to new agent
        let mut ctx = self.contexts.entry(to).or_insert(AgentContext {
            messages: Vec::new(),
            tool_results: HashMap::new(),
            artifacts: Vec::new(),
        });
        ctx.messages.extend(context_messages);
    }
}
```

---

## 7. End-to-End Data Flow: Full Coordination Sequence

The following diagram shows a complete coordination sequence for a typical
code generation + review task:

```
  User           Coordinator         Coder           QA            Fixer
   │                 │                 │              │              │
   │  "Fix bug #42"  │                 │              │              │
   │────────────────▶│                 │              │              │
   │                 │                 │              │              │
   │                 │  Plan(steps=[   │              │              │
   │                 │    Analyze,     │              │              │
   │                 │    Implement,   │              │              │
   │                 │    Review])     │              │              │
   │                 │                 │              │              │
   │                 │  Request→Coder  │              │              │
   │                 │────────────────▶│              │              │
   │                 │                 │  write code  │              │
   │                 │                 │──────┐       │              │
   │                 │                 │      │       │              │
   │                 │                 │◀─────┘       │              │
   │                 │  Response(code) │              │              │
   │                 │◀────────────────│              │              │
   │                 │                 │              │              │
   │                 │  Handoff→QA     │              │              │
   │                 │──────────────────────────────▶│              │
   │                 │                 │              │  review      │
   │                 │                 │              │──────┐       │
   │                 │                 │              │      │       │
   │                 │                 │              │◀─────┘       │
   │                 │                 │              │              │
   │                 │  Handoff→Fixer  │              │              │
   │                 │                 │              │─────────────▶│
   │                 │                 │              │              │ fix
   │                 │                 │              │              │──┐
   │                 │                 │              │              │◀─┘
   │                 │                 │              │              │
   │                 │  Handoff→QA     │              │              │
   │                 │──────────────────────────────▶│              │
   │                 │                 │              │  re-review   │
   │                 │                 │              │──────┐       │
   │                 │                 │              │      │       │
   │                 │                 │              │◀─────┘       │
   │                 │                 │              │              │
   │                 │  Approved ✓     │              │              │
   │                 │◀──────────────────────────────│              │
   │                 │                 │              │              │
   │  Result         │                 │              │              │
   │◀────────────────│                 │              │              │
   │                 │                 │              │              │
```

---

## 8. Configuration Reference

### 8.1 Session-Level Configuration

```toml
[xauft.coordination]
team_mode = "coordinator"          # "coordinator" | "collaborate"
max_concurrency = 5                # max parallel sub-agents
qa_fixer_loop = true               # enable QA→Fixer loop
max_qa_iterations = 3              # max QA→Fixer rounds
handoff_timeout_secs = 120         # timeout for agent handoff

[xauft.coordination.coordinator]
planner = "one_shot"               # "one_shot" | "iterative" | "tree_of_thought"
synthesis_model = "gpt-4o"         # model for result synthesis
step_timeout_secs = 300            # per-step timeout

[xauft.coordination.collaborate]
agent_chain = ["planner", "coder", "reviewer"]
synthesis_model = "gpt-4o"
```

### 8.2 Agent Role Definitions

| Role       | System Prompt Focus               | Key Tools                    | Typical Model     |
|------------|-----------------------------------|------------------------------|-------------------|
| Planner    | Decomposition, step ordering      | ReplanTool                   | gpt-4o / claude-3.5 |
| Coder      | Implementation, file editing      | FileEdit, Shell, Search      | gpt-4o / claude-3.5 |
| QA         | Review, correctness, security     | CodeReview, TestRunner       | gpt-4o / claude-3.5 |
| Fixer      | Address review issues             | FileEdit, Shell              | gpt-4o / claude-3.5 |
| Researcher | Information gathering             | WebSearch, FileReader, Grep  | gpt-4o-mini       |
| Synthesiser | Merging contributions            | (none — LLM-only)           | gpt-4o            |

---

## 9. Error Handling and Resilience

### 9.1 Failure Modes

```rust
#[derive(Debug, thiserror::Error)]
pub enum CoordinationError {
    #[error("Agent {agent_id} timed out after {timeout:?}")]
    AgentTimeout { agent_id: AgentId, timeout: Duration },

    #[error("Sub-agent pool exhausted for role {role:?}")]
    PoolExhausted { role: AgentRole },

    #[error("Handoff failed: agent {to} not available")]
    HandoffFailed { from: AgentId, to: AgentId },

    #[error("QA→Fixer loop exceeded {max} iterations")]
    QaLoopExhausted { max: usize, remaining_issues: usize },

    #[error("Message bus error: {0}")]
    BusError(#[from] BusError),

    #[error("Provider error during delegation: {0}")]
    ProviderError(String),
}
```

### 9.2 Recovery Strategies

| Failure              | Strategy                                        |
|----------------------|-------------------------------------------------|
| Agent timeout        | Retry with same agent (1x), then fallback agent |
| Pool exhausted       | Queue task, wait for agent to become available  |
| Handoff failed       | Abort sub-task, report error to coordinator     |
| QA loop exhausted    | Return best-effort result with warning          |
| Provider error       | FallbackProvider tries next in chain            |
| Bus error            | Reconnect, replay from last checkpoint          |

### 9.3 Observability

All coordination events are emitted as structured events:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum CoordinationEvent {
    #[serde(rename = "coordination.agent_started")]
    AgentStarted { agent_id: AgentId, role: AgentRole, task_id: TaskId },

    #[serde(rename = "coordination.agent_completed")]
    AgentCompleted { agent_id: AgentId, duration: Duration, tokens: TokenUsage },

    #[serde(rename = "coordination.handoff")]
    Handoff { from: AgentId, to: AgentId, reason: HandoffReason },

    #[serde(rename = "coordination.delegation")]
    Delegation { parent: AgentId, child: AgentId, tool: String },

    #[serde(rename = "coordination.pool_acquire")]
    PoolAcquire { role: AgentRole, wait_time: Duration },

    #[serde(rename = "coordination.pool_release")]
    PoolRelease { role: AgentRole, tasks_completed: usize },
}
```

These events are consumed by xauft's telemetry subsystem and surfaced via
the SSE bridge for real-time dashboard display.

---

## 10. Future Extensions

1. **Dynamic team composition**: Agents join/leave the team mid-session based
   on task requirements discovered during execution.

2. **Hierarchical coordination**: Sub-coordinators manage sub-teams, enabling
   tree-shaped coordination for very large tasks.

3. **Agent capability negotiation**: Agents advertise capabilities on the bus;
   coordinators select agents based on capability matching rather than static
   role assignment.

4. **Cross-session agent persistence**: Agent context is persisted across
   xauft sessions so the same agent can resume work on recurring tasks.
