# AGENTS.md

# Agent Runtime Rules

This repository enforces strict workflow-state mutation semantics.

These rules are foundational architecture constraints and MUST NEVER be violated.

---

# Core Principle

Agents DO NOT own workflow state.

Tools own workflow state mutation.

Agents are decision-makers and orchestrators only.

---

# Absolute Rule

## ALL workflow state mutations MUST occur through tools.

Agents MAY:

* inspect state through tools
* request mutations through tools
* reason about state
* plan around state
* coordinate tools
* coordinate other agents

Agents MUST NOT:

* mutate workflow state directly
* mutate shared memory directly
* mutate task state directly
* mutate orchestration state directly
* mutate graph state directly
* mutate runtime state directly
* emit structured outputs intended to mutate state
* bypass tool execution
* synthesize state transitions internally

---

# Structured Outputs Are BANNED

Structured outputs are explicitly forbidden for workflow state mutation.

This includes:

* JSON outputs
* Pydantic outputs
* typed response objects
* schema-generated mutations
* direct state-return contracts
* response-driven orchestration
* LLM-generated workflow deltas

The runtime MUST NEVER interpret structured LLM responses as authoritative state mutations.

State mutation authority belongs exclusively to tools.

---

# Why Structured Outputs Are Banned

Structured outputs create severe architectural problems:

## 1. Hidden State Mutation

LLMs silently mutate runtime state without observable execution boundaries.

This breaks:

* auditability
* tracing
* determinism
* replayability
* observability

---

## 2. No Execution Guarantees

Structured outputs cannot guarantee:

* validation
* authorization
* concurrency safety
* transactional semantics
* rollback support
* idempotency
* durability

Tools can.

---

## 3. Breaks Event Sourcing

State mutation must emit runtime events.

Tool execution naturally supports:

* tracing spans
* lifecycle hooks
* SignalBus events
* telemetry
* metrics
* persistence logs

Structured outputs bypass the runtime.

---

## 4. Breaks Human Approval

Approval systems require explicit executable operations.

Tools provide:

* approval interception
* mutation previews
* rollback boundaries
* permission checks

Structured outputs bypass approval infrastructure.

---

# Correct Architecture

## GOOD

```text
Agent
  → decides action
  → invokes tool
  → tool validates mutation
  → tool mutates workflow state
  → runtime emits events
  → observers react
```

---

## BAD

```text
Agent
  → returns structured output
  → runtime mutates state directly
```

This architecture is forbidden.

---

# State Access Rules

Agents may ONLY interact with workflow state through tools.

Examples:

## Allowed

```text
ReadStateTool
UpdateTaskStatusTool
AppendMessageTool
StoreMemoryTool
TransitionWorkflowTool
CreateArtifactTool
CommitCheckpointTool
```

## Forbidden

```python
ctx.workflow.state.status = "done"
ctx.memory.entries.append(...)
ctx.task.phase = "running"
```

Direct state mutation is prohibited.

---

# Tool Requirements

All state-mutating tools MUST:

* be idempotent where possible
* emit structured events
* support tracing
* support cancellation
* support retries
* validate input
* validate permissions
* support rollback semantics where applicable
* provide deterministic mutation boundaries

---

# Event Requirements

Every mutation MUST emit runtime events.

Examples:

```text
StateTransitionStarted
StateTransitionCompleted
WorkflowCheckpointCreated
TaskStatusUpdated
MemoryEntryStored
ArtifactCreated
```

No silent mutations.

---

# Concurrency Rules

Workflow state MUST be concurrency-safe.

Tools handling mutation MUST support:

* optimistic concurrency
* locking semantics where needed
* atomic operations
* transactional boundaries
* race-condition prevention

Agents themselves MUST remain stateless orchestrators.

---

# Replayability

A workflow run must be reproducible from:

* tool invocations
* emitted events
* persisted checkpoints

This is impossible if state mutates via structured outputs.

---

# Approval Model

Human approval systems operate at the tool layer.

Only tools may:

* request approval
* preview mutations
* apply mutations
* rollback mutations

Agents cannot bypass approval systems.

---

# Workflow Philosophy

The runtime is an event-driven execution engine.

Agents are planners.

Tools are executors.

State mutation is an executable side effect — never a language-model side effect.

---

# Architectural Benefits

This architecture provides:

* deterministic execution
* runtime observability
* event sourcing compatibility
* replayability
* audit logs
* rollback support
* safer autonomous execution
* distributed-runtime compatibility
* transactional semantics
* human-in-the-loop enforcement

---

# Enforcement

The runtime SHOULD enforce these rules via:

* banned structured-output mutation APIs
* immutable workflow snapshots
* tool-only mutation interfaces
* runtime validation
* event-sourced execution boundaries
* approval interceptors
* mutation capability guards

---

# Final Rule

If workflow state changes:

A TOOL must have executed.

If no tool executed:

No state mutation is allowed.
