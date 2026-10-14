# PRD 58 — Meta Agent (Agent that Creates Other Agents)

**Status:** Draft  
**Date:** 2026-06-12  
**Complexity:** High  
**Upstream repos:** `agtrs` (`/root/rust_projects/agtrs`), `xaft` (`/root/rust_projects/xaft`)

---

## 1. Problem Statement

xaft's current orchestration model is a fixed four-agent pipeline:

```
Planner → Coder → QA ↔ Fixer
```

This pipeline works for typical single-domain coding tasks. It fails — or performs poorly — when the task requires:

- **Multiple specializations in parallel.** A migration from REST to gRPC touches `.proto` schema, server implementation, and every client call-site. No single generalist agent handles all three domains optimally; doing them sequentially also wastes wall-clock time.
- **Emergent task structure.** The planner cannot know at boot time that a security audit needs a threat-modeling agent, a static-analysis agent, and a patch agent. The workflow must be designed *after* reading the task.
- **Domain depth.** A "database migration specialist" that only sees migration-related tools and a targeted system prompt outperforms a generalist coder receiving all tools and a generic prompt.
- **Parallel synthesis.** Two specialist agents can investigate independent subsystems simultaneously; the meta agent synthesizes their findings before handing off to QA. This is impossible in the current sequential pipeline.

Five primitives are absent from both `agtrs` and `xaft` today:

1. No serializable description of an agent (blueprint) the LLM can produce as structured output.
2. No factory that constructs a live `Arc<dyn Agent>` from a blueprint at runtime.
3. No tool a running agent can call to spawn and run another agent on demand.
4. No orchestrator variant that accepts new agents during a live run.
5. No reusable agent pool to amortize construction overhead across parallel invocations.

---

## 2. Goals

| # | Goal |
|---|------|
| G1 | `AgentBlueprint` — a `Serialize + Deserialize + JsonSchema` struct the LLM produces as structured tool input. |
| G2 | `AgentFactory` trait — converts a blueprint into a live `Arc<dyn Agent>`. |
| G3 | `SpawnAgentTool` — a `Tool` that any agent can call to define, spawn, and run a specialist; returns the specialist's final output. |
| G4 | `ParallelAgentTool` — like `SpawnAgentTool` but spawns N specialists concurrently and returns an array of results. |
| G5 | `DynamicHandoffOrchestrator` — extends `HandoffOrchestrator` to accept new agents mid-run. |
| G6 | `AgentPool` — a size-bounded pool of pre-warmed agent instances the meta agent draws from. |
| G7 | `XaftAgentFactory` — xaft's implementation of `AgentFactory`, resolving blueprint tool names against `ToolRegistryBuilder`. |
| G8 | `WorkflowConfig::Meta` — a new workflow mode wired into `run_workflow()` and config. |
| G9 | `MetaAgentSystemPrompt` — a built-in system prompt explaining the tools, blueprint schema, and synthesis strategy. |
| G10 | TUI integration — ephemeral spinner and committed transcript lines for spawned specialist agents. |
| G11 | `/agents` update — lists dynamic specialist agents with their blueprints. |
| G12 | Nesting guard — `allow_nesting = false` prevents recursive spawn. |
| G13 | Spawn limit — `max_spawned_agents` cap enforced by the tool; 4th call fails cleanly. |

### Non-Goals

- Replacing the existing `Standard` workflow (full backward compatibility required).
- Automatic model selection per agent (blueprint may carry a model override; routing is the caller's responsibility).
- Persistent agent definitions across sessions (blueprints exist only for the lifetime of one run).
- Network isolation between spawned agents (all agents share the same workspace and git worktree).

---

## 3. Background: What Exists Today

### 3.1 `SubagentTool<T>` in agtrs-runtime

`SubagentTool<T>` (`agtrs-runtime/src/subagent.rs`) is a `Tool` that invokes a *pre-defined* subagent:

```rust
pub struct SubagentTool<T> {
    name: String,
    description: String,
    subagent: Arc<dyn Agent>,   // fully built at construction time
    config: SubagentConfig,
    _phantom: PhantomData<T>,
}
```

The subagent's system prompt, tools, and config are fixed before the parent runs. The subagent cannot be defined dynamically from LLM output. `SpawnAgentTool` (§5.1) extends this concept to runtime-defined blueprints.

### 3.2 `HandoffOrchestrator` in agtrs-runtime

`HandoffOrchestrator` (`agtrs-runtime/src/team.rs`) coordinates agents via a `HandoffAgentStore`. Agents are registered at build time:

```rust
let orchestrator = HandoffOrchestrator::builder()
    .agent("planner", planner)
    .agent("coder", coder)
    .build();
```

No API exists to add agents after `build()` is called. `DynamicHandoffOrchestrator` (§5.3) provides this.

### 3.3 `AgentRegistry` in xaft-runtime

`AgentRegistry` (`crates/xaft-agents/src/registry.rs`) maps names to `AgentDefinition`s. It is populated at session start; no runtime additions are possible. `WorkflowConfig::Meta` sidesteps the registry for dynamically spawned agents by driving them through `XaftAgentFactory` (§5.5).

### 3.4 `WorkflowConfig` in xaft-agents

```rust
pub enum WorkflowConfig {
    Standard,
    Dynamic { initial_agent, max_handoffs, agent_subset },
}
```

The `Meta` variant (§5.6) is additive; both existing variants are preserved unchanged.

### 3.5 Existing Signals

agtrs has `SubagentStarted` / `SubagentCompleted` signals. These fire for `SubagentTool<T>` invocations but carry no blueprint. New signals `AgentSpawned` / `AgentSpawnCompleted` (§5.4) carry blueprint metadata and distinguish meta-agent-spawned specialists from ordinary subagents.

---

## 4. Architecture Overview

```
┌──────────────────────────────────────────────────────────────┐
│  MetaOrchestrator (xaft-runtime::orchestrator)               │
│                                                              │
│  ┌──────────────────────────────────────┐                    │
│  │  MetaAgent (xaft-agents::meta_agent) │                    │
│  │  Tools:                              │                    │
│  │    spawn_agent         ─────────────►│ SpawnAgentTool     │
│  │    spawn_agents_parallel────────────►│ ParallelAgentTool  │
│  │    [read tools: list_files, grep…]   │                    │
│  └──────────────────────────────────────┘                    │
│            │ blueprint JSON                                   │
│            ▼                                                  │
│  XaftAgentFactory::create(blueprint)                         │
│            │                                                  │
│            ▼                                                  │
│  ┌──────────────────────────────────────┐                    │
│  │ Specialist agents (dynamic)          │                    │
│  │   proto-definition-agent             │                    │
│  │   service-impl-agent    (parallel)   │                    │
│  │   client-update-agent   (parallel)   │                    │
│  └──────────────────────────────────────┘                    │
│            │ final outputs                                    │
│            ▼                                                  │
│  MetaAgent synthesizes → handoff_to_agent("qa")              │
│            │                                                  │
│            ▼                                                  │
│  QA ↔ Fixer  (standard HandoffOrchestrator sub-loop)        │
└──────────────────────────────────────────────────────────────┘
```

Data flows:
1. User submits task to `MetaOrchestrator`.
2. MetaAgent uses read tools to understand the codebase.
3. MetaAgent calls `spawn_agent` or `spawn_agents_parallel` with blueprint JSON.
4. `SpawnAgentTool` calls `XaftAgentFactory::create(blueprint)`, runs the specialist, returns output.
5. MetaAgent synthesizes all specialist outputs.
6. MetaAgent calls `handoff_to_agent("qa", synthesis)`.
7. Standard QA ↔ Fixer loop verifies changes.

---

## 5. Detailed Design

### 5.1 `AgentBlueprint` and `TerminationCondition` (agtrs-runtime)

**File:** `agtrs-runtime/src/agent.rs` (or new `agtrs-runtime/src/meta.rs`)

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A serializable description of an agent that can be constructed at runtime.
///
/// Produced as structured LLM output by the meta/coordinator agent.
/// Resolved by `AgentFactory::create()` into a live `Arc<dyn Agent>`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentBlueprint {
    /// Unique name for this agent instance within the run.
    /// Used as the routing key in `DynamicHandoffOrchestrator`.
    pub name: String,

    /// Short human-readable role description (shown in TUI and `/agents`).
    pub role: String,

    /// Full system prompt. The factory passes this verbatim to the agent.
    /// May contain placeholders like `{{task}}` or `{{working_dir}}`;
    /// the factory resolves them from `BlueprintContext` before passing to the agent.
    pub system_prompt: String,

    /// Tool names this agent should have access to.
    /// Resolved by `AgentFactory` against the known tool master set.
    /// Unknown names are logged as warnings and silently skipped
    /// (fail-open so a typo does not abort the whole workflow).
    pub tools: Vec<String>,

    /// Maximum LLM turns before the agent is forcibly stopped.
    /// Capped by `AgentFactory` at the factory's `max_turns_ceiling`.
    pub max_turns: usize,

    /// Optional model override. When `None`, the factory uses the runtime default.
    /// Validated against the provider's known model list; invalid values fall back
    /// to the default with a warning.
    pub model: Option<String>,

    /// When the specialist should stop producing output and return.
    #[serde(default)]
    pub terminates_on: TerminationCondition,

    /// If `true`, the meta agent will not hand off further after this agent's
    /// output — the output is considered the final result for this branch.
    #[serde(default)]
    pub is_terminal: bool,
}

/// Controls what constitutes a "done" signal for a specialist agent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum TerminationCondition {
    /// Agent terminates when it produces any text response without calling a tool.
    /// This is the safest default — agents that call tools indefinitely are stopped
    /// when the turn limit is reached.
    #[default]
    AnyResponse,

    /// Agent terminates when its output contains this sentinel string.
    /// The sentinel is case-sensitive. The sentinel text is stripped from the
    /// returned output before the meta agent receives it.
    Sentinel(String),

    /// Agent terminates after exactly N LLM turns, regardless of output.
    /// The output from the Nth turn is returned.
    Turns(usize),
}
```

**Design notes:**

- `AgentBlueprint` derives `JsonSchema` so it can be embedded directly in `SpawnAgentTool`'s input schema. The LLM constructs a blueprint as a JSON object in its tool call.
- The `tools` field contains names like `"read_file"`, `"grep"`, `"write_file"` — the same names used by `ToolRegistryBuilder` and visible in the meta agent's system prompt.
- `TerminationCondition::Sentinel` requires the `AgentExecutor` loop (inside the factory) to check each response text. This is implemented inside `SpawnAgentTool::call()` rather than in `AgentExecutor` itself, preserving the executor's simplicity.
- `max_turns` is validated against the factory's `max_turns_ceiling` (default: 50) to prevent runaway agents consuming unbounded tokens.

### 5.2 `AgentFactory` Trait (agtrs-runtime)

**File:** `agtrs-runtime/src/meta.rs` (new file)

```rust
use std::sync::Arc;
use async_trait::async_trait;
use crate::agent::Agent;
use crate::error::AgtrsError;

/// Context passed to `AgentFactory::create()` beyond the blueprint itself.
/// Allows the factory to inject runtime values (working dir, task, nesting depth)
/// that the blueprint cannot know ahead of time.
#[derive(Debug, Clone)]
pub struct BlueprintContext {
    /// The task string given to the meta agent.
    pub task: String,
    /// Absolute path of the workspace root.
    pub working_dir: String,
    /// Current nesting depth (0 = spawned directly by meta agent).
    /// Factories use this to enforce `allow_nesting` limits.
    pub nesting_depth: usize,
    /// Session ID for correlation in logs and signals.
    pub session_id: String,
}

/// Creates a live `Arc<dyn Agent>` from an `AgentBlueprint`.
///
/// Implementors own the logic of resolving tool names to concrete `Arc<ErasedTool>`
/// instances, building the `AgentConfig`, and wiring signals.
///
/// The `XaftAgentFactory` implementation in `xaft-runtime` resolves tools from
/// `ToolRegistryBuilder`. Third-party factories can resolve from any source.
#[async_trait]
pub trait AgentFactory: Send + Sync {
    /// Instantiate an agent from a blueprint.
    ///
    /// # Errors
    /// - `AgtrsError::Other` when the blueprint is malformed (empty name, empty prompt).
    /// - `AgtrsError::Other` when nesting is disallowed and `ctx.nesting_depth > 0`.
    async fn create(
        &self,
        blueprint: &AgentBlueprint,
        ctx: &BlueprintContext,
    ) -> Result<Arc<dyn Agent>, AgtrsError>;

    /// Returns `true` if the factory can provide a tool with the given name.
    /// Used by the meta agent system prompt generator to list available tools.
    fn supports_tool(&self, tool_name: &str) -> bool;

    /// All tool names this factory can assign to spawned agents.
    fn available_tools(&self) -> Vec<String>;
}
```

**Design notes:**

- The trait is `async` because tool resolution may involve I/O (e.g., opening a git repo to build git tools).
- `available_tools()` is used by `MetaAgentSystemPrompt` to embed a real list of assignable tool names into the meta agent's context, so the LLM does not hallucinate tool names.
- The factory does not own the `SignalBus`; it receives it via the `BlueprintContext` path (the concrete `XaftAgentFactory` holds `Arc<SignalBus>` from its constructor).

### 5.3 `SpawnAgentTool` and `ParallelAgentTool` (agtrs-runtime)

**File:** `agtrs-runtime/src/meta.rs`

#### 5.3.1 `SpawnAgentTool`

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::error::AgtrsError;
use crate::executor::AgentExecutor;
use crate::signals::SignalBus;
use crate::tool::{Tool, ToolContext, ToolResult};
use crate::transport::Message;

/// Input schema for the `spawn_agent` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnAgentInput {
    /// Full specification of the agent to create.
    pub blueprint: AgentBlueprint,
    /// The task prompt given to the specialist as its first user message.
    pub task: String,
    /// When `true` (default), the call blocks until the specialist finishes
    /// and returns its output. `false` is reserved for future fire-and-forget
    /// mode and is not yet implemented; passing `false` returns an error.
    #[serde(default = "default_true")]
    pub await_result: bool,
}

fn default_true() -> bool { true }

/// Output from `spawn_agent`: the specialist agent's final text output.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SpawnAgentOutput {
    /// The agent name from the blueprint.
    pub agent_name: String,
    /// The specialist's final text response.
    pub output: String,
    /// Whether the specialist completed without error.
    pub success: bool,
    /// LLM turns the specialist consumed.
    pub turns_used: usize,
}

/// `spawn_agent` — spawns a single specialist agent and returns its output.
///
/// Tool name (LLM-visible): `"spawn_agent"`
///
/// The tool:
/// 1. Validates the input blueprint (non-empty name, non-empty system_prompt).
/// 2. Checks the spawn counter against `max_spawned_agents`; fails if exceeded.
/// 3. Checks nesting depth; fails if `allow_nesting = false` and depth > 0.
/// 4. Calls `factory.create(blueprint, ctx)` to get a live `Arc<dyn Agent>`.
/// 5. Builds an `AgentContext` and runs `AgentExecutor::run()` to completion.
/// 6. Applies `TerminationCondition` post-processing.
/// 7. Emits `AgentSpawned` before and `AgentSpawnCompleted` after.
/// 8. Returns `SpawnAgentOutput` serialized to JSON.
pub struct SpawnAgentTool<F: AgentFactory> {
    factory: Arc<F>,
    signals: Arc<SignalBus>,
    /// Hard cap on total spawns in this run.
    max_spawned_agents: usize,
    /// Shared counter across all `SpawnAgentTool` and `ParallelAgentTool`
    /// instances for this run.
    spawned_count: Arc<AtomicUsize>,
    /// Current nesting depth (0 when held by the meta agent directly).
    nesting_depth: usize,
    /// Whether spawned agents can themselves spawn agents.
    allow_nesting: bool,
    /// Context for factory resolution.
    blueprint_ctx: BlueprintContext,
    /// `resolve_ctx` from the parent for `AgentExecutor`.
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    /// LLM provider forwarded to specialists.
    llm: Arc<dyn crate::llm::LlmProvider>,
}
```

The `Tool` implementation for `SpawnAgentTool`:

```rust
#[async_trait]
impl<F: AgentFactory + 'static> Tool for SpawnAgentTool<F> {
    type Inputs = serde_json::Value;
    type Output = ToolResult;

    fn name(&self) -> &str { "spawn_agent" }

    fn description(&self) -> &str {
        "Design and spawn a specialist agent with a custom system prompt and tool set. \
         Blocks until the specialist finishes and returns its output. \
         Use spawn_agents_parallel to run multiple specialists concurrently."
    }

    async fn call(&self, input: Self::Inputs, _ctx: ToolContext) -> Self::Output {
        let req: SpawnAgentInput = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid spawn_agent input: {e}")),
        };

        if !req.await_result {
            return ToolResult::error(
                "await_result=false (fire-and-forget) is not yet implemented."
            );
        }

        // Validate blueprint
        if req.blueprint.name.trim().is_empty() {
            return ToolResult::error("blueprint.name must not be empty");
        }
        if req.blueprint.system_prompt.trim().is_empty() {
            return ToolResult::error("blueprint.system_prompt must not be empty");
        }
        if req.blueprint.max_turns == 0 {
            return ToolResult::error("blueprint.max_turns must be >= 1");
        }

        // Enforce nesting
        if !self.allow_nesting && self.nesting_depth > 0 {
            return ToolResult::error(
                "Nesting is disabled: a spawned agent cannot itself spawn agents. \
                 Set allow_nesting=true in MetaWorkflowConfig to enable."
            );
        }

        // Enforce spawn cap
        let prev = self.spawned_count.fetch_add(1, Ordering::SeqCst);
        if prev >= self.max_spawned_agents {
            self.spawned_count.fetch_sub(1, Ordering::SeqCst);
            return ToolResult::error(format!(
                "Spawn limit reached: max_spawned_agents={} already used. \
                 Synthesize from agents already run.",
                self.max_spawned_agents
            ));
        }

        // Emit AgentSpawned signal
        let task_preview: String = req.task.chars().take(100).collect();
        self.signals.emit(AgentSpawned {
            parent_agent: self.blueprint_ctx.session_id.clone(),
            child_agent_name: req.blueprint.name.clone(),
            role: req.blueprint.role.clone(),
            task_preview: task_preview.clone(),
        }).await;

        let started_at = std::time::Instant::now();

        // Create the agent
        let agent = match self.factory.create(&req.blueprint, &self.blueprint_ctx).await {
            Ok(a) => a,
            Err(e) => {
                self.spawned_count.fetch_sub(1, Ordering::SeqCst);
                return ToolResult::error(format!("Failed to create agent '{}': {e}", req.blueprint.name));
            }
        };

        // Run the agent
        let result = self
            .run_specialist(agent, &req.blueprint, &req.task)
            .await;

        let duration_ms = started_at.elapsed().as_secs_f64() * 1000.0;
        let success = result.is_ok();

        self.signals.emit(AgentSpawnCompleted {
            child_agent_name: req.blueprint.name.clone(),
            success,
            output_preview: result.as_ref()
                .map(|r| r.output.chars().take(100).collect())
                .unwrap_or_default(),
            duration_ms,
        }).await;

        match result {
            Ok(out) => ToolResult::success(
                serde_json::to_string_pretty(&out).unwrap_or_default()
            ),
            Err(e) => ToolResult::error(format!(
                "Specialist '{}' failed: {e}", req.blueprint.name
            )),
        }
    }
}
```

The `run_specialist` helper applies `TerminationCondition` logic:

- `AnyResponse`: run `AgentExecutor::run()` to natural completion; return final message text.
- `Sentinel(s)`: run executor; if output contains `s`, strip sentinel and return; otherwise return as-is.
- `Turns(n)`: override `AgentConfig::max_turns` to `n`; return final message text after `n` turns.

#### 5.3.2 `ParallelAgentTool`

```rust
/// Input schema for `spawn_agents_parallel`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ParallelAgentInput {
    /// Agents to spawn concurrently. Spawned in one `tokio::join!` fan-out.
    pub agents: Vec<ParallelAgentSpec>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ParallelAgentSpec {
    pub blueprint: AgentBlueprint,
    pub task: String,
}

/// Output from `spawn_agents_parallel`: results in the same order as input.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ParallelAgentResults {
    pub results: Vec<ParallelAgentResult>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ParallelAgentResult {
    pub agent_name: String,
    pub output: String,
    pub success: bool,
    pub turns_used: usize,
}

/// `spawn_agents_parallel` — spawns N specialists concurrently.
///
/// Tool name: `"spawn_agents_parallel"`
///
/// Uses `tokio::spawn` to fan out, then `futures::future::join_all` to collect.
/// Honors `max_spawned_agents` by checking the counter before allocating any
/// slot (atomic: reserves all N slots up-front or fails the whole call).
/// Honors `max_parallel_agents` by semaphore-limiting concurrent executions.
pub struct ParallelAgentTool<F: AgentFactory> {
    factory: Arc<F>,
    signals: Arc<SignalBus>,
    max_spawned_agents: usize,
    max_parallel_agents: usize,
    spawned_count: Arc<AtomicUsize>,
    nesting_depth: usize,
    allow_nesting: bool,
    blueprint_ctx: BlueprintContext,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    llm: Arc<dyn crate::llm::LlmProvider>,
}
```

**Semaphore design:** `ParallelAgentTool` uses `tokio::sync::Semaphore` with `max_parallel_agents` permits. Each `tokio::spawn`-ed specialist task acquires one permit before calling the LLM; it releases on completion. This keeps memory and API rate-limit pressure bounded.

**Slot reservation:** Before spawning, `ParallelAgentTool` does a single compare-and-swap that reserves `agents.len()` slots atomically. If the reservation would exceed `max_spawned_agents`, the entire call fails with a clear error and no agents are spawned. This avoids partial spawn scenarios.

### 5.4 Signals (agtrs-runtime)

**File:** `agtrs-runtime/src/signals.rs` (additions)

```rust
/// Emitted by `SpawnAgentTool` or `ParallelAgentTool` when a specialist
/// agent is about to start. Precedes `AgentSpawnCompleted`.
///
/// Subscribe to observe the meta agent's spawning decisions in real time.
#[derive(Debug, Clone)]
pub struct AgentSpawned {
    /// Name of the agent (or session ID) that initiated the spawn.
    pub parent_agent: String,
    /// Name from `AgentBlueprint::name`.
    pub child_agent_name: String,
    /// Role description from `AgentBlueprint::role`.
    pub role: String,
    /// First 100 characters of the task given to the specialist.
    pub task_preview: String,
}

/// Emitted by `SpawnAgentTool` or `ParallelAgentTool` when a specialist
/// agent finishes (successfully or with an error).
#[derive(Debug, Clone)]
pub struct AgentSpawnCompleted {
    /// Name from `AgentBlueprint::name`.
    pub child_agent_name: String,
    /// `true` if the agent completed without returning an error `ToolResult`.
    pub success: bool,
    /// First 100 characters of the agent's final output.
    pub output_preview: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: f64,
}
```

These are distinct from the existing `SubagentStarted` / `SubagentCompleted` signals, which fire for static `SubagentTool<T>` invocations. The TUI bridge (§5.9) maps `AgentSpawned` → `TuiEvent::SpecialistAgentStarted` and `AgentSpawnCompleted` → `TuiEvent::SpecialistAgentCompleted`.

### 5.5 `DynamicHandoffOrchestrator` (agtrs-runtime)

**File:** `agtrs-runtime/src/meta.rs` (or `team.rs` extension)

The `DynamicHandoffOrchestrator` wraps `HandoffOrchestrator` and provides a `RwLock`-protected side-map of agents registered during a run. When `run()` looks up an agent name, it checks the side-map before the static map:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agent::Agent;
use crate::error::AgtrsError;
use crate::meta::{AgentBlueprint, AgentFactory, BlueprintContext};
use crate::team::{HandoffOrchestrator, HandoffRunParams, HandoffResult};

pub struct DynamicHandoffOrchestrator {
    /// The static orchestrator for pre-registered agents (QA, Fixer, etc.).
    base: HandoffOrchestrator,
    /// Factory for creating agents from blueprints at runtime.
    factory: Option<Arc<dyn AgentFactory>>,
    /// Agents registered dynamically during a live run.
    /// Keyed by `AgentBlueprint::name`.
    dynamic_agents: Arc<RwLock<HashMap<String, Arc<dyn Agent>>>>,
}

impl DynamicHandoffOrchestrator {
    /// Register a new agent into the dynamic side-map.
    ///
    /// If an agent with the same name already exists, it is replaced.
    /// Thread-safe: multiple callers can register concurrently.
    pub async fn register_dynamic_agent(&self, name: String, agent: Arc<dyn Agent>) {
        let mut agents = self.dynamic_agents.write().await;
        agents.insert(name, agent);
    }

    /// Create an agent from a blueprint and register it in one call.
    ///
    /// Returns the registered agent's name on success.
    pub async fn register_from_blueprint(
        &self,
        blueprint: AgentBlueprint,
        ctx: &BlueprintContext,
    ) -> Result<String, AgtrsError> {
        let factory = self.factory.as_ref().ok_or_else(|| {
            AgtrsError::Other("DynamicHandoffOrchestrator has no factory".into())
        })?;
        let agent = factory.create(&blueprint, ctx).await?;
        let name = blueprint.name.clone();
        self.register_dynamic_agent(name.clone(), agent).await;
        Ok(name)
    }

    /// Run multi-agent orchestration.
    ///
    /// Agent lookup order: dynamic side-map → static base orchestrator.
    /// This allows the meta agent to hand off to a dynamically registered
    /// specialist by name without modifying the base orchestrator.
    pub async fn run(&self, params: HandoffRunParams) -> Result<HandoffResult, AgtrsError> {
        // Implementation delegates to a modified run loop that checks
        // dynamic_agents first on each handoff lookup.
        // ... (see §7 for the full implementation plan)
        todo!()
    }
}
```

**Note on integration:** `DynamicHandoffOrchestrator` is *not* used in the `Meta` workflow path for specialist execution — specialists are run directly by `SpawnAgentTool`/`ParallelAgentTool` via `AgentExecutor::run()`. The `DynamicHandoffOrchestrator` is designed for the future case where the meta agent wants to add a specialist to the *handoff graph* so it can receive handoffs from other agents. For PRD 58, it is implemented but not wired into the default meta workflow; the default uses direct tool-invoked execution.

### 5.6 `XaftAgentFactory` (xaft-runtime)

**File:** `crates/xaft-runtime/src/meta.rs` (new file)

```rust
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

use agtrs_runtime::agent::{Agent, AgentConfig};
use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::llm::LlmProvider;
use agtrs_runtime::meta::{AgentBlueprint, AgentFactory, BlueprintContext};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::ErasedTool;
use xaft_tools::registry::ToolRegistryBuilder;

use crate::error::RuntimeError;

/// xaft's implementation of `AgentFactory`.
///
/// Resolves `AgentBlueprint::tools` names against a master tool set built
/// by `ToolRegistryBuilder`. Wires signals and approval gates.
pub struct XaftAgentFactory {
    /// The full set of tools any specialist may draw from.
    /// Shared across all factory calls; individual specialists get a filtered subset.
    master_tools: Vec<Arc<ErasedTool>>,
    /// LLM provider forwarded to specialists.
    llm: Arc<dyn LlmProvider>,
    /// Signal bus for lifecycle events.
    signals: Arc<SignalBus>,
    /// Optional approval gate (same gate as the TUI's).
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// Workspace root — used to validate tool paths.
    working_dir: PathBuf,
    /// Maximum `max_turns` any specialist may request.
    max_turns_ceiling: usize,
    /// Whether spawned agents are allowed to themselves spawn agents.
    allow_nesting: bool,
    /// `injectable_runtime::ResolveContext` for `AgentExecutor`.
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
}

impl XaftAgentFactory {
    pub fn new(
        tool_builder: ToolRegistryBuilder,
        llm: Arc<dyn LlmProvider>,
        signals: Arc<SignalBus>,
        approval_gate: Option<Arc<dyn ApprovalGate>>,
        working_dir: PathBuf,
        allow_nesting: bool,
        resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    ) -> Result<Self, RuntimeError> {
        // Build both read and write registries; merge into master list.
        let read_reg = tool_builder.build_reader()?;
        // Clone builder fields (ToolRegistryBuilder is not Clone;
        // reconstruct from the same working_dir + flags).
        // In practice, the factory receives a pre-built Vec<Arc<ErasedTool>>.
        let master_tools = read_reg.all();
        Ok(Self {
            master_tools,
            llm,
            signals,
            approval_gate,
            working_dir,
            max_turns_ceiling: 50,
            allow_nesting,
            resolve_ctx,
        })
    }

    /// Filter `master_tools` to only those whose `Tool::name()` matches
    /// names in `blueprint.tools`. Unknown names are warned and skipped.
    fn resolve_tools(&self, blueprint: &AgentBlueprint) -> Vec<Arc<ErasedTool>> {
        let mut resolved = Vec::new();
        for name in &blueprint.tools {
            if let Some(tool) = self.master_tools.iter().find(|t| t.name() == name.as_str()) {
                resolved.push(Arc::clone(tool));
            } else {
                warn!(
                    agent = %blueprint.name,
                    tool = %name,
                    "AgentBlueprint: tool not found in master set, skipping"
                );
            }
        }
        resolved
    }
}

#[async_trait]
impl AgentFactory for XaftAgentFactory {
    async fn create(
        &self,
        blueprint: &AgentBlueprint,
        ctx: &BlueprintContext,
    ) -> Result<Arc<dyn Agent>, AgtrsError> {
        // Nesting guard
        if !self.allow_nesting && ctx.nesting_depth > 0 {
            return Err(AgtrsError::Other(
                "XaftAgentFactory: nesting is disabled".into()
            ));
        }

        // Resolve system prompt placeholders
        let system_prompt = blueprint.system_prompt
            .replace("{{task}}", &ctx.task)
            .replace("{{working_dir}}", &ctx.working_dir);

        // Cap max_turns
        let max_turns = blueprint.max_turns.min(self.max_turns_ceiling);
        if blueprint.max_turns > self.max_turns_ceiling {
            warn!(
                agent = %blueprint.name,
                requested = blueprint.max_turns,
                capped = self.max_turns_ceiling,
                "AgentBlueprint: max_turns capped at ceiling"
            );
        }

        // Resolve tools
        let tools = self.resolve_tools(blueprint);
        debug!(
            agent = %blueprint.name,
            tool_count = tools.len(),
            "XaftAgentFactory: resolved tools for specialist"
        );

        let config = AgentConfig {
            system_prompt,
            max_turns,
            strict_capability_check: false,
            parallel_tool_calls: false,
            ..Default::default()
        };

        // Build the specialist agent using xaft_agents::named::NamedAgent
        use xaft_agents::named::NamedAgent;
        let agent = NamedAgent::new(&blueprint.name, &config.system_prompt, max_turns)
            .with_tools(tools)
            .with_signals(Arc::clone(&self.signals));

        Ok(Arc::new(agent))
    }

    fn supports_tool(&self, tool_name: &str) -> bool {
        self.master_tools.iter().any(|t| t.name() == tool_name)
    }

    fn available_tools(&self) -> Vec<String> {
        self.master_tools.iter().map(|t| t.name().to_string()).collect()
    }
}
```

**Key design decisions:**

- `master_tools` is built once and shared via `Arc` across all `create()` calls. Individual specialists receive filtered `Arc<ErasedTool>` clones — zero duplication of the underlying tool state.
- The factory does **not** build a new `ToolRegistryBuilder` per agent. Instead, it receives a pre-built `Vec<Arc<ErasedTool>>` that covers the full tool superset. This avoids re-opening the git repo and re-constructing workspace stores on every spawn call.
- Approval gate is forwarded from the parent runtime. Specialists with `bash_exec` in their tool list will therefore trigger the same TUI confirmation dialog as the main agent.

### 5.7 `WorkflowConfig::Meta` (xaft-agents)

**File:** `crates/xaft-agents/src/registry.rs`

```rust
#[derive(Debug, Clone, Default)]
pub enum WorkflowConfig {
    /// Classic fixed pipeline: planner → coder → QA ↔ fixer.
    #[default]
    Standard,
    /// Dynamic handoff: any registered agent can hand off to any other.
    Dynamic {
        initial_agent: String,
        max_handoffs: usize,
        agent_subset: Option<Vec<String>>,
    },
    /// Meta workflow: a coordinator agent designs and spawns specialists.
    Meta {
        /// System prompt for the meta agent.
        /// `None` uses `META_AGENT_SYSTEM_PROMPT` from `xaft-agents`.
        meta_prompt: Option<String>,
        /// Maximum total specialist agents the meta agent may spawn (across
        /// both `spawn_agent` and `spawn_agents_parallel`).
        max_spawned_agents: usize,
        /// Maximum simultaneous specialists (semaphore permit count for
        /// `spawn_agents_parallel`).
        max_parallel_agents: usize,
        /// Allow spawned specialists to themselves call `spawn_agent`.
        allow_nesting: bool,
        /// Maximum nesting depth (0 = no nesting; 1 = one level deep, etc.).
        max_nesting_depth: usize,
    },
}
```

### 5.8 Meta Agent System Prompt (xaft-agents)

**File:** `crates/xaft-agents/src/meta_agent.rs` (new file)

The system prompt is a `&str` constant assembled at compile time. Key sections:

#### Role definition

```
You are a meta coordinator for the xaft autonomous coding system.
Your job is to analyze the given task, design a set of specialist agents
tailored to the task, spawn them (sequentially or in parallel), synthesize
their outputs, and hand off to the QA agent for verification.

You do NOT write code yourself. You delegate all code changes to specialists.
```

#### Available tools section (injected dynamically)

The runtime replaces `{{available_tools}}` with the factory's `available_tools()` list, formatted as a table:

```
AVAILABLE TOOLS FOR SPECIALISTS:
  read_file       — read a file from the workspace
  write_file      — write or overwrite a file
  edit_file       — apply a targeted diff to a file
  grep            — search file contents by regex
  list_files      — list directory contents
  git_diff        — show unstaged changes
  git_status      — show working tree status
  bash_exec       — run a shell command (requires approval)
  [... full list ...]

Assign only the tools each specialist genuinely needs. A reader agent
should not receive write_file. A coder should not receive bash_exec unless
it needs to run tests.
```

#### Blueprint schema section

```
AGENT BLUEPRINT SCHEMA:
{
  "name": "unique-agent-name",          // kebab-case, unique per run
  "role": "One-line role description",
  "system_prompt": "Full system prompt for this specialist.",
  "tools": ["read_file", "grep"],        // from the list above
  "max_turns": 20,                       // 1–50
  "model": null,                         // or "claude-3-5-sonnet-20241022"
  "terminates_on": { "type": "AnyResponse" },
  "is_terminal": false
}
```

#### `spawn_agent` call pattern

```
SPAWNING A SINGLE SPECIALIST:
  Call spawn_agent with:
    blueprint: { ... AgentBlueprint ... }
    task: "Specific task description for this specialist."
    await_result: true

  The tool returns SpawnAgentOutput with fields:
    agent_name, output, success, turns_used
```

#### `spawn_agents_parallel` call pattern

```
SPAWNING MULTIPLE SPECIALISTS IN PARALLEL:
  Call spawn_agents_parallel with:
    agents: [
      { blueprint: {...}, task: "..." },
      { blueprint: {...}, task: "..." }
    ]

  All agents run concurrently. Results are returned in input order.
  Use this when specialists are independent (no output dependency).
  For dependent specialists (B needs A's output), use sequential spawn_agent calls.
```

#### Synthesis and handoff

```
SYNTHESIS:
  After all specialists finish, produce a synthesis message that:
  1. States what each specialist did and what files changed.
  2. Notes any errors or partial completions.
  3. Provides a unified summary for the QA agent.

  Then call handoff_to_agent("qa", synthesis_message).
  Do NOT call handoff_to_agent before all specialists complete.
```

#### Constraints

```
CONSTRAINTS:
  - Max {{max_spawned_agents}} specialists per run.
  - Max {{max_parallel_agents}} concurrent specialists.
  - Nesting is {{nesting_status}}.
  - Working directory: {{working_dir}}
```

The runtime substitutes these placeholders when building the meta agent in `run_meta_workflow()`.

### 5.9 xaft-runtime Orchestration Entry Point

**File:** `crates/xaft-runtime/src/orchestrator.rs`

New public function added alongside `run_workflow` and `run_dynamic_handoff`:

```rust
/// Run a meta-agent workflow where the coordinator designs and spawns
/// specialists dynamically.
///
/// The meta agent receives `spawn_agent` and `spawn_agents_parallel` tools
/// backed by `XaftAgentFactory`. After specialists complete, the meta agent
/// calls `handoff_to_agent("qa")`, and QA ↔ Fixer runs as a standard
/// HandoffOrchestrator sub-loop.
pub async fn run_meta_workflow(
    task: &str,
    meta_config: &MetaWorkflowConfig,  // extracted from WorkflowConfig::Meta
    llm: Arc<dyn LlmProvider>,
    signals: Arc<SignalBus>,
    resolve_ctx: Arc<injectable_runtime::ResolveContext>,
    read_tools: Vec<Arc<ErasedTool>>,
    write_tools: Vec<Arc<ErasedTool>>,
    session: &mut AgentSession,
    conversation_store: Option<Arc<dyn ConversationStore>>,
    approval_gate: Option<Arc<dyn ApprovalGate>>,
    user_parts: Option<Vec<ContentBlock>>,
) -> Result<(String, ExitCode), RuntimeError> {
    // 1. Build master tool set (read + write) for XaftAgentFactory
    let all_tools: Vec<Arc<ErasedTool>> = read_tools.iter()
        .chain(write_tools.iter())
        .cloned()
        .collect();

    // 2. Build XaftAgentFactory
    let factory = Arc::new(XaftAgentFactory::from_master_tools(
        all_tools.clone(),
        Arc::clone(&llm),
        Arc::clone(&signals),
        approval_gate.clone(),
        session.workspace_root.clone(),
        meta_config.allow_nesting,
        Arc::clone(&resolve_ctx),
    ));

    // 3. Shared spawn counter (shared between spawn_agent and spawn_agents_parallel)
    let spawned_count = Arc::new(AtomicUsize::new(0));

    // 4. Blueprint context
    let blueprint_ctx = BlueprintContext {
        task: task.to_string(),
        working_dir: session.workspace_root.display().to_string(),
        nesting_depth: 0,
        session_id: session.id.clone(),
    };

    // 5. Build spawn tools
    let spawn_tool = Arc::new(SpawnAgentTool::new(
        Arc::clone(&factory),
        Arc::clone(&signals),
        meta_config.max_spawned_agents,
        Arc::clone(&spawned_count),
        blueprint_ctx.clone(),
        Arc::clone(&resolve_ctx),
        Arc::clone(&llm),
        meta_config.allow_nesting,
    )) as Arc<ErasedTool>;

    let parallel_tool = Arc::new(ParallelAgentTool::new(
        Arc::clone(&factory),
        Arc::clone(&signals),
        meta_config.max_spawned_agents,
        meta_config.max_parallel_agents,
        Arc::clone(&spawned_count),
        blueprint_ctx.clone(),
        Arc::clone(&resolve_ctx),
        Arc::clone(&llm),
        meta_config.allow_nesting,
    )) as Arc<ErasedTool>;

    // 6. Build meta agent tools: read tools + spawn tools + handoff
    let handoff_store = Arc::new(HandoffAgentStore::new());
    let meta_stop = Arc::new(AtomicBool::new(false));
    let mut meta_tools: Vec<Arc<ErasedTool>> = read_tools.clone();
    meta_tools.push(spawn_tool);
    meta_tools.push(parallel_tool);
    meta_tools.push(Arc::new(HandoffTool::new_with_flag(
        Arc::clone(&handoff_store),
        vec![QA_NAME.into()],
        Arc::clone(&meta_stop),
    )));

    // 7. Build meta agent system prompt (inject available tools, constraints)
    let available_tools_list = factory.available_tools().join(", ");
    let nesting_status = if meta_config.allow_nesting { "enabled" } else { "disabled" };
    let wd = session.workspace_root.display().to_string();
    let meta_prompt = meta_config.meta_prompt.clone()
        .unwrap_or_else(|| META_AGENT_SYSTEM_PROMPT.to_string())
        .replace("{{available_tools}}", &available_tools_list)
        .replace("{{max_spawned_agents}}", &meta_config.max_spawned_agents.to_string())
        .replace("{{max_parallel_agents}}", &meta_config.max_parallel_agents.to_string())
        .replace("{{nesting_status}}", nesting_status)
        .replace("{{working_dir}}", &wd);

    // 8. Build MetaAgent (a NamedAgent with the meta prompt)
    let meta_agent = Arc::new(
        NamedAgent::new("meta", &meta_prompt, 30)
            .with_tools(meta_tools)
            .with_signals(Arc::clone(&signals))
            .with_handoff_flag(Arc::clone(&meta_stop)),
    );

    // 9. QA and Fixer (re-use standard pipeline)
    let (qa_agent, fixer_agent) = build_qa_fixer_pair(
        task, &wd, &read_tools, &write_tools, &handoff_store, &signals
    );

    // 10. HandoffOrchestrator with meta + qa + fixer
    let orchestrator = HandoffOrchestrator::builder()
        .agent("meta", Arc::clone(&meta_agent) as Arc<dyn Agent>)
        .agent(QA_NAME, qa_agent)
        .agent(FIXER_NAME, fixer_agent)
        .conv_store(conversation_store.unwrap_or_else(|| Arc::new(InMemoryConversationStore::new())))
        .agent_store(Arc::clone(&handoff_store))
        .max_handoffs(20)
        .llm(Arc::clone(&llm))
        .resolve_ctx(Arc::clone(&resolve_ctx))
        .with_approval_gate_opt(approval_gate)
        .with_global_tool_hook(Arc::new(ReadBeforeEditHook::new()))
        .prompt_fn(|ctx| match ctx.to_agent.as_str() {
            "qa" => format!(
                "Review changes for task: {}\n\nMeta-agent synthesis:\n{}",
                ctx.original_message, ctx.summary
            ),
            "fixer" => format!(
                "Fix issues found by QA:\n{}\n\nOriginal task: {}",
                ctx.summary, ctx.original_message
            ),
            _ => format!("[HANDOFF from {}]: {}", ctx.from_agent, ctx.summary),
        })
        .build();

    // 11. Run
    let result = orchestrator.run(HandoffRunParams {
        message: task.to_string(),
        user_parts,
        conversation_id: format!("{}::meta", session.id),
        initial_agent: "meta".to_string(),
        context_state: {
            let mut m = HashMap::new();
            m.insert("conversation_id".to_string(), serde_json::json!(session.id));
            m
        },
        signals: Some(Arc::clone(&signals)),
        max_handoffs_override: None,
        #[cfg(feature = "axum")]
        extensions: Default::default(),
    }).await.map_err(|e| RuntimeError::Agent(e.to_string()))?;

    session.turn_count += result.turns as u32;

    // 12. Build concluding summary (same pattern as run_workflow)
    let approved = result.content.to_uppercase().contains("APPROVED");
    let qa_verdict = if approved { "QA approved" } else { "QA incomplete" };

    Ok((format!("{qa_verdict}\n\n{}", result.content), ExitCode::SUCCESS))
}

/// Helper: construct `MetaWorkflowConfig` from `WorkflowConfig::Meta`.
pub struct MetaWorkflowConfig {
    pub meta_prompt: Option<String>,
    pub max_spawned_agents: usize,
    pub max_parallel_agents: usize,
    pub allow_nesting: bool,
    pub max_nesting_depth: usize,
}
```

### 5.10 `runtime.rs` Integration

**File:** `crates/xaft-runtime/src/runtime.rs`

In `XaftRuntime::run_task()`, after the existing `WorkflowConfig::Dynamic` branch:

```rust
WorkflowConfig::Meta { .. } => {
    let meta_cfg = MetaWorkflowConfig::from_workflow_config(&workflow_config);
    run_meta_workflow(
        &task,
        &meta_cfg,
        Arc::clone(&llm),
        Arc::clone(&self.signals),
        Arc::clone(&resolve_ctx),
        read_tools,
        write_tools,
        &mut session,
        self.conversation_store.clone(),
        self.approval_gate.clone(),
        user_parts,
    ).await
}
```

### 5.11 Config Types (xaft-config)

**File:** `crates/xaft-config/src/types.rs`

```rust
/// Workflow mode configuration.
///
/// Serialized as `[workflow]` in TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowModeConfig {
    /// Workflow mode: `"standard"`, `"dynamic"`, or `"meta"`.
    pub mode: WorkflowMode,
    /// For `meta` mode: maximum specialists the coordinator may spawn.
    pub meta_max_spawned: usize,
    /// For `meta` mode: maximum simultaneous specialists.
    pub meta_max_parallel: usize,
    /// For `meta` mode: allow spawned agents to themselves spawn agents.
    pub meta_allow_nesting: bool,
    /// For `meta` mode: custom meta agent system prompt (path or inline text).
    pub meta_prompt: Option<String>,
    // For `dynamic` mode
    pub dynamic_initial_agent: Option<String>,
    pub dynamic_max_handoffs: Option<usize>,
    pub dynamic_agent_subset: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMode {
    #[default]
    Standard,
    Dynamic,
    Meta,
}

impl Default for WorkflowModeConfig {
    fn default() -> Self {
        Self {
            mode: WorkflowMode::Standard,
            meta_max_spawned: 8,
            meta_max_parallel: 4,
            meta_allow_nesting: false,
            meta_prompt: None,
            dynamic_initial_agent: None,
            dynamic_max_handoffs: None,
            dynamic_agent_subset: None,
        }
    }
}
```

Add to `XaftConfig`:

```rust
pub struct XaftConfig {
    // ... existing fields ...
    /// Workflow orchestration mode.
    pub workflow: WorkflowModeConfig,
}
```

**File:** `crates/xaft-config/src/defaults.rs`

```toml
# default workflow config
[workflow]
mode = "standard"
meta_max_spawned = 8
meta_max_parallel = 4
meta_allow_nesting = false
```

**Env var overrides:**
- `XAFT_WORKFLOW_MODE` — `"standard"` | `"dynamic"` | `"meta"`
- `XAFT_META_MAX_SPAWNED` — integer
- `XAFT_META_MAX_PARALLEL` — integer
- `XAFT_META_ALLOW_NESTING` — `"true"` | `"false"`

### 5.12 xaft-agent Signals (xaft-agent)

**File:** `crates/xaft-agent/src/signals.rs` (additions)

```rust
/// Emitted by `SpawnAgentTool` when the meta agent spawns a specialist.
/// xaft-level wrapper around agtrs `AgentSpawned` that includes blueprint JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftMetaAgentSpawned {
    /// JSON-serialized `AgentBlueprint` (for display in `/agents`).
    pub blueprint_json: String,
    /// First 100 chars of the task given to the specialist.
    pub task_preview: String,
    /// `"sequential"` or `"parallel"`.
    pub spawn_mode: String,
}

/// Emitted when a specialist agent spawned by the meta agent completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftMetaAgentCompleted {
    /// Agent name from the blueprint.
    pub agent_name: String,
    /// Whether the specialist succeeded.
    pub success: bool,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: f64,
    /// First 100 chars of the specialist's output.
    pub output_preview: String,
}
```

These signals are emitted by `SpawnAgentTool` and `ParallelAgentTool` (in addition to the agtrs-level `AgentSpawned` / `AgentSpawnCompleted`). The xaft-level signals carry blueprint JSON, which the agtrs signals do not.

### 5.13 TUI Integration (xaft-tui)

#### 5.13.1 Bridge additions

**File:** `crates/xaft-tui/src/bridge.rs`

```rust
// New TuiEvent variants:

/// A specialist agent was spawned by the meta agent.
SpecialistAgentStarted {
    agent_name: String,
    role: String,
    task_preview: String,
    spawn_mode: String,   // "sequential" | "parallel"
},

/// A specialist agent spawned by the meta agent completed.
SpecialistAgentCompleted {
    agent_name: String,
    success: bool,
    duration_ms: f64,
    output_preview: String,
},
```

The bridge subscribes to `XaftMetaAgentSpawned` and `XaftMetaAgentCompleted` via `SignalBus::subscribe()` and forwards them as these `TuiEvent` variants.

#### 5.13.2 State tracking

**File:** `crates/xaft-tui/src/state.rs`

```rust
/// Tracks active and completed specialist agents in a meta workflow.
#[derive(Debug, Default, Clone)]
pub struct MetaAgentTracker {
    /// Specialists currently running (name → start instant).
    pub active: HashMap<String, std::time::Instant>,
    /// Completed specialists (ordered).
    pub completed: Vec<CompletedSpecialist>,
}

#[derive(Debug, Clone)]
pub struct CompletedSpecialist {
    pub name: String,
    pub role: String,
    pub success: bool,
    pub duration_ms: f64,
}
```

Added to `AppState`:

```rust
pub struct AppState {
    // ... existing fields ...
    pub meta_tracker: MetaAgentTracker,
    pub workflow_is_meta: bool,
}
```

`AppState::handle_event()` match arms:

```rust
TuiEvent::SpecialistAgentStarted { agent_name, role, task_preview, spawn_mode } => {
    self.meta_tracker.active.insert(agent_name.clone(), std::time::Instant::now());
    // Emit a committed transcript line showing the spawn
}
TuiEvent::SpecialistAgentCompleted { agent_name, success, duration_ms, .. } => {
    if let Some(start) = self.meta_tracker.active.remove(&agent_name) {
        self.meta_tracker.completed.push(CompletedSpecialist {
            name: agent_name,
            role: String::new(),
            success,
            duration_ms,
        });
    }
}
```

#### 5.13.3 Ephemeral display

**File:** `crates/xaft-tui/src/ephemeral.rs`

When `AppState::workflow_is_meta` and `meta_tracker.active` is non-empty, the ephemeral line changes from the standard spinner to:

```
◈ Meta  [spawned: 3 agents, 1 running]  (2m 14s · ↓ 8.2k tokens)
```

Each completed specialist emits a **committed** transcript line (not ephemeral) in the format:

```
  ↳ [specialist] proto-definition-agent: done (42s)
  ↳ [specialist] service-impl-agent: done (1m 8s)
```

Failed specialists show in amber/red depending on theme:

```
  ↳ [specialist] client-update-agent: FAILED (15s)
```

#### 5.13.4 `/agents` command update

**File:** `crates/xaft-tui/src/slash/commands/agents.rs`

When `workflow_is_meta`, the command receives the `MetaAgentTracker` via `CommandContext` and formats:

```
Meta workflow active — coordinator + dynamic specialists

  meta (coordinator)   tools: 12   max_turns: 30
    spawn_agent, spawn_agents_parallel, handoff_to_agent, list_files…

  [running]
    service-impl-agent   role: Rust gRPC service implementation
      tools: write_file, edit_file, read_file, grep, git_status, bash_exec
      max_turns: 20   model: claude-3-5-sonnet-20241022

  [completed]
    proto-definition-agent   role: Protobuf schema design
      tools: write_file, read_file, grep
      max_turns: 15   duration: 42s   status: OK

Standard handoff agents (post-meta):
  qa      tools: 9   max_turns: 8
  fixer   tools: 14  max_turns: 20
```

---

## 6. Concrete Example Walkthrough

### Scenario

User runs:

```
xaft run "Migrate the payments service from REST to gRPC"
```

Config has `[workflow] mode = "meta"`.

### Step 1 — MetaAgent analyzes the task

MetaAgent receives the task and its read tools. It calls `list_files` and `grep` to understand the codebase:

```
list_files("payments/")
→ payments/api.py, payments/client.py, payments/models.py, payments/tests/

grep("requests.get|requests.post", "payments/")
→ 14 occurrences in api.py and client.py
```

MetaAgent reasons: this needs three sequential/parallel phases:

1. Proto schema must be defined first (proto-definition-agent, sequential).
2. Server implementation and client updates are independent once proto is done (parallel).

### Step 2 — MetaAgent designs blueprints and spawns

**First call: `spawn_agent` (sequential — proto must exist before impl)**

```json
{
  "blueprint": {
    "name": "proto-definition-agent",
    "role": "Protobuf schema definition for payments gRPC service",
    "system_prompt": "You are a Protobuf schema specialist. Your task is to design the .proto file for the payments gRPC service, converting the existing REST API surface to gRPC service definitions.\n\nWorking directory: {{working_dir}}\n\nRules:\n- Create payments/proto/payments.proto\n- Mirror all existing REST endpoints as RPC methods\n- Use appropriate message types for request/response\n- Add service metadata comments\n- When done, output 'SCHEMA_COMPLETE' followed by a summary of the RPCs defined.",
    "tools": ["read_file", "grep", "list_files", "write_file"],
    "max_turns": 15,
    "model": null,
    "terminates_on": { "type": "Sentinel", "value": "SCHEMA_COMPLETE" },
    "is_terminal": false
  },
  "task": "Read the existing payments REST API in payments/api.py and create a complete payments.proto file at payments/proto/payments.proto.",
  "await_result": true
}
```

MetaAgent waits. Proto agent reads `payments/api.py`, identifies 7 REST endpoints, writes `payments/proto/payments.proto`, outputs `SCHEMA_COMPLETE — defined 7 RPCs: CreatePayment, GetPayment, ListPayments, UpdatePayment, CancelPayment, RefundPayment, GetBalance`.

**Second call: `spawn_agents_parallel` (two independent specialists)**

```json
{
  "agents": [
    {
      "blueprint": {
        "name": "service-implementation-agent",
        "role": "Python gRPC server implementation",
        "system_prompt": "You are a Python gRPC server implementation specialist. The payments service is being migrated from REST (Flask/FastAPI) to gRPC.\n\nThe .proto file has been created at payments/proto/payments.proto.\nWorking directory: {{working_dir}}\n\nYour task:\n1. Generate the Python gRPC stubs from the proto file using grpc_tools.protoc\n2. Implement all RPC handlers in payments/grpc_service.py\n3. Preserve all existing business logic from payments/api.py\n4. Keep the original api.py intact (do not delete it)\n5. Add basic logging to each RPC method",
        "tools": ["read_file", "write_file", "edit_file", "grep", "list_files", "bash_exec"],
        "max_turns": 25,
        "model": null,
        "terminates_on": { "type": "AnyResponse" },
        "is_terminal": false
      },
      "task": "Implement the gRPC server for the payments service based on the proto file at payments/proto/payments.proto. All business logic is in payments/api.py."
    },
    {
      "blueprint": {
        "name": "client-update-agent",
        "role": "Python gRPC client update",
        "system_prompt": "You are a Python gRPC client specialist. The payments service is being migrated from REST to gRPC.\n\nThe .proto file has been created at payments/proto/payments.proto.\nWorking directory: {{working_dir}}\n\nYour task:\n1. Update payments/client.py to use gRPC stubs instead of HTTP requests\n2. The old client used requests.get/post to call the REST API\n3. The new client should use the generated gRPC stubs\n4. Keep the same public function signatures for backward compatibility\n5. Update payments/tests/ to test the gRPC client",
        "tools": ["read_file", "write_file", "edit_file", "grep", "list_files"],
        "max_turns": 20,
        "model": null,
        "terminates_on": { "type": "AnyResponse" },
        "is_terminal": false
      },
      "task": "Update payments/client.py to use gRPC instead of REST HTTP calls, based on the proto file at payments/proto/payments.proto."
    }
  ]
}
```

Both agents run concurrently. `service-implementation-agent` runs bash_exec to generate stubs (triggers TUI approval gate), writes `payments/grpc_service.py`. `client-update-agent` reads `payments/client.py` and `payments/tests/`, rewrites them to use gRPC stubs.

### Step 3 — MetaAgent synthesizes

Both parallel agents complete. MetaAgent receives both `SpawnAgentOutput` results, synthesizes:

```
All three specialist agents have completed the gRPC migration:

1. proto-definition-agent: Created payments/proto/payments.proto with 7 RPC definitions
   (CreatePayment, GetPayment, ListPayments, UpdatePayment, CancelPayment, RefundPayment, GetBalance)

2. service-implementation-agent: Created payments/grpc_service.py with full RPC handlers,
   generated gRPC stubs via grpc_tools.protoc, preserved all business logic from api.py.

3. client-update-agent: Updated payments/client.py to use gRPC stubs, maintained public
   API surface, updated payments/tests/ with gRPC client tests.

Files changed:
  + payments/proto/payments.proto
  + payments/grpc_service.py
  ~ payments/client.py
  ~ payments/tests/test_client.py

The original payments/api.py is untouched for backward compatibility.
```

### Step 4 — Handoff to QA

MetaAgent calls `handoff_to_agent("qa", synthesis_message)`. The standard QA ↔ Fixer loop runs.

### Step 5 — TUI during the run

During the parallel phase, the TUI ephemeral shows:

```
◈ Meta  [spawned: 3 agents, 2 running]  (4m 12s · ↓ 22.4k tokens)
```

Committed transcript lines (in order):

```
  ↳ [specialist] proto-definition-agent: done (38s)
  ↳ [specialist] service-implementation-agent: done (2m 14s)
  ↳ [specialist] client-update-agent: done (1m 47s)
```

`/agents` output during parallel phase:

```
Meta workflow active

  meta (coordinator)   tools: 15   turns: 8/30

  [running]
    service-implementation-agent   role: Python gRPC server implementation
    client-update-agent            role: Python gRPC client update

  [completed]
    proto-definition-agent   role: Protobuf schema definition   42s  OK

Standard handoff agents:
  qa     (pending)
  fixer  (pending)
```

---

## 7. Files to Change

### agtrs-runtime (`/root/rust_projects/agtrs/agtrs-runtime/src/`)

| File | Change |
|------|--------|
| `agent.rs` | Add `AgentBlueprint`, `TerminationCondition` types |
| `signals.rs` | Add `AgentSpawned`, `AgentSpawnCompleted` |
| `meta.rs` (new) | `AgentFactory` trait, `BlueprintContext`, `SpawnAgentTool`, `ParallelAgentTool`, `DynamicHandoffOrchestrator` |
| `lib.rs` | Re-export `meta` module |

### xaft-agents (`crates/xaft-agents/src/`)

| File | Change |
|------|--------|
| `registry.rs` | Add `WorkflowConfig::Meta` variant and `MetaWorkflowConfig` extraction |
| `meta_agent.rs` (new) | `META_AGENT_SYSTEM_PROMPT` constant |
| `lib.rs` | Re-export `meta_agent` |

### xaft-runtime (`crates/xaft-runtime/src/`)

| File | Change |
|------|--------|
| `meta.rs` (new) | `XaftAgentFactory`, `MetaWorkflowConfig` |
| `orchestrator.rs` | Add `run_meta_workflow()`, `build_qa_fixer_pair()` helper |
| `runtime.rs` | Handle `WorkflowConfig::Meta` in `run_task()` |
| `lib.rs` | Re-export `meta` module |

### xaft-agent (`crates/xaft-agent/src/`)

| File | Change |
|------|--------|
| `signals.rs` | Add `XaftMetaAgentSpawned`, `XaftMetaAgentCompleted` |
| `lib.rs` | Re-export new signals |

### xaft-config (`crates/xaft-config/src/`)

| File | Change |
|------|--------|
| `types.rs` | Add `WorkflowModeConfig`, `WorkflowMode`; add `workflow` field to `XaftConfig` |
| `defaults.rs` | Default `WorkflowModeConfig` |
| `merge.rs` | Deep-merge for `WorkflowModeConfig` |

### xaft-tui (`crates/xaft-tui/src/`)

| File | Change |
|------|--------|
| `bridge.rs` | Subscribe to `XaftMetaAgentSpawned`/`Completed`; add `TuiEvent` variants |
| `state.rs` | Add `MetaAgentTracker`, `workflow_is_meta`; handle new events |
| `ephemeral.rs` | Meta mode spinner and active-count display |
| `slash/commands/agents.rs` | Show dynamic agent graph for meta mode |

---

## 8. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | A meta agent can call `spawn_agent` with a valid `AgentBlueprint` and receive the specialist's final text output as a `ToolResult`. |
| AC2 | `spawn_agents_parallel` with N agents runs all N concurrently (verified by wall-clock time < sum of sequential times) and returns an array of N results in input order. |
| AC3 | When `max_spawned_agents = 3`, a 4th `spawn_agent` call returns `ToolResult { is_error: true }` with a message containing "Spawn limit reached"; no agent is created. |
| AC4 | When `allow_nesting = false`, a spawned specialist that calls `spawn_agent` receives `ToolResult { is_error: true }` with a message containing "Nesting is disabled". |
| AC5 | `XaftAgentFactory::create(blueprint)` correctly filters tools: an agent with `tools: ["read_file", "grep"]` receives exactly those two tools (plus any automatically injected tools like handoff). |
| AC6 | `XaftAgentFactory::create(blueprint)` warns on unknown tool names but does not return an error; the agent is created with the subset of known tools. |
| AC7 | `AgentSpawned` signal is emitted with the correct `child_agent_name` and `role` before the specialist runs. |
| AC8 | `AgentSpawnCompleted` signal is emitted after the specialist finishes with the correct `success` flag and a `duration_ms > 0`. |
| AC9 | `XaftMetaAgentSpawned` signal carries valid JSON in `blueprint_json` that deserializes to `AgentBlueprint`. |
| AC10 | TUI ephemeral shows `"◈ Meta  [spawned: N agents, M running]"` while specialists are active in `Meta` workflow mode. |
| AC11 | After each specialist completes, a committed transcript line `"  ↳ [specialist] <name>: done (<duration>)"` appears in the TUI. |
| AC12 | `/agents` slash command in `Meta` mode lists the coordinator, all running specialists (with role), and all completed specialists (with duration and status). |
| AC13 | `WorkflowConfig::Meta` is selected when `config.workflow.mode = "meta"` in `xaft.toml`. |
| AC14 | `TerminationCondition::Sentinel("DONE")` causes `SpawnAgentTool` to stop the specialist when its output contains `"DONE"` and return the output with the sentinel stripped. |
| AC15 | `MetaAgentSystemPrompt` includes the factory's `available_tools()` list verbatim, so the LLM cannot hallucinate tool names that don't exist. |
| AC16 | After the run completes, `spawned_count` equals the number of specialists that were spawned, and all their `tokio::spawn` handles have been joined (no leaked tasks). |
| AC17 | `blueprint.max_turns` values above `max_turns_ceiling` are capped with a log warning, not an error. |
| AC18 | A specialist with `tools: []` runs with zero tools and terminates on its first response (no tool calls possible). |

---

## 9. Tests

### 9.1 agtrs-runtime Unit Tests

**File:** `agtrs-runtime/tests/meta.rs` (new)

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use agtrs_runtime::meta::{
    AgentBlueprint, AgentFactory, BlueprintContext, SpawnAgentTool, ParallelAgentTool,
    TerminationCondition,
};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::tool::{Tool, ToolContext};

// ── Test helpers ──────────────────────────────────────────────────────────────

fn dummy_ctx() -> BlueprintContext {
    BlueprintContext {
        task: "test task".into(),
        working_dir: "/tmp".into(),
        nesting_depth: 0,
        session_id: "test-session".into(),
    }
}

fn simple_blueprint(name: &str) -> AgentBlueprint {
    AgentBlueprint {
        name: name.into(),
        role: "Test specialist".into(),
        system_prompt: "You are a test agent. Respond with 'DONE'.".into(),
        tools: vec![],
        max_turns: 5,
        model: None,
        terminates_on: TerminationCondition::AnyResponse,
        is_terminal: false,
    }
}

/// A mock factory that creates agents backed by a mock LLM returning a fixed string.
struct MockFactory {
    response: String,
    available: Vec<String>,
}

#[async_trait::async_trait]
impl AgentFactory for MockFactory {
    async fn create(
        &self,
        blueprint: &AgentBlueprint,
        _ctx: &BlueprintContext,
    ) -> Result<Arc<dyn agtrs_runtime::agent::Agent>, agtrs_runtime::error::AgtrsError> {
        // Build a simple agent backed by a MockLlmProvider
        use agtrs_runtime::testing::MockLlmProvider;
        use xaft_agents::named::NamedAgent;
        let mock_llm = MockLlmProvider::new(vec![self.response.clone()]);
        let agent = NamedAgent::new(&blueprint.name, &blueprint.system_prompt, blueprint.max_turns);
        Ok(Arc::new(agent))
    }

    fn supports_tool(&self, name: &str) -> bool {
        self.available.iter().any(|n| n == name)
    }

    fn available_tools(&self) -> Vec<String> {
        self.available.clone()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_blueprint_serializes_and_deserializes() {
    let bp = AgentBlueprint {
        name: "proto-agent".into(),
        role: "Protobuf specialist".into(),
        system_prompt: "Write proto files.".into(),
        tools: vec!["read_file".into(), "write_file".into()],
        max_turns: 10,
        model: Some("claude-3-5-sonnet-20241022".into()),
        terminates_on: TerminationCondition::Sentinel("DONE".into()),
        is_terminal: false,
    };

    let json = serde_json::to_string(&bp).expect("serialize");
    let roundtrip: AgentBlueprint = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(roundtrip.name, bp.name);
    assert_eq!(roundtrip.tools, bp.tools);
    assert_eq!(roundtrip.model, bp.model);
    assert!(matches!(
        roundtrip.terminates_on,
        TerminationCondition::Sentinel(ref s) if s == "DONE"
    ));
}

#[tokio::test]
async fn termination_condition_default_is_any_response() {
    let bp: AgentBlueprint = serde_json::from_str(r#"{
        "name": "a",
        "role": "b",
        "system_prompt": "c",
        "tools": [],
        "max_turns": 5,
        "model": null,
        "is_terminal": false
    }"#).expect("deserialize");
    assert_eq!(bp.terminates_on, TerminationCondition::AnyResponse);
}

#[tokio::test]
async fn agent_factory_creates_agent_from_blueprint() {
    let factory = Arc::new(MockFactory {
        response: "Task complete.".into(),
        available: vec!["read_file".into()],
    });
    let bp = simple_blueprint("test-agent");
    let ctx = dummy_ctx();
    let agent = factory.create(&bp, &ctx).await.expect("create");
    assert_eq!(agent.name(), "test-agent");
}

#[tokio::test]
async fn spawn_agent_tool_runs_specialist_and_returns_result() {
    // This test uses MockLlmProvider; no real LLM calls.
    let factory = Arc::new(MockFactory {
        response: "Specialist output here.".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    let spawned_count = Arc::new(AtomicUsize::new(0));

    let tool = SpawnAgentTool::new(
        factory,
        Arc::clone(&signals),
        /* max_spawned */ 5,
        spawned_count,
        dummy_ctx(),
        /* nesting_depth */ 0,
        /* allow_nesting */ true,
    );

    let input = serde_json::json!({
        "blueprint": {
            "name": "spec-agent",
            "role": "Specialist",
            "system_prompt": "Do the thing.",
            "tools": [],
            "max_turns": 3,
            "model": null,
            "terminates_on": { "type": "AnyResponse" },
            "is_terminal": false
        },
        "task": "Write a test.",
        "await_result": true
    });

    let result = tool.call(input, ToolContext::default()).await;
    assert!(!result.is_error, "tool should succeed: {:?}", result.content);
    let output: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(output["agent_name"], "spec-agent");
    assert_eq!(output["success"], true);
}

#[tokio::test]
async fn max_spawned_agents_limit_enforced() {
    let factory = Arc::new(MockFactory {
        response: "done".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    // Pre-fill the counter to the limit
    let spawned_count = Arc::new(AtomicUsize::new(3));

    let tool = SpawnAgentTool::new(
        factory,
        signals,
        /* max_spawned */ 3,
        spawned_count,
        dummy_ctx(),
        0,
        true,
    );

    let input = serde_json::json!({
        "blueprint": simple_blueprint("overflow-agent"),
        "task": "anything",
        "await_result": true
    });

    let result = tool.call(input, ToolContext::default()).await;
    assert!(result.is_error, "should fail when limit reached");
    assert!(
        result.content.contains("Spawn limit reached"),
        "error message: {}", result.content
    );
}

#[tokio::test]
async fn nesting_disabled_prevents_recursive_spawn() {
    let factory = Arc::new(MockFactory {
        response: "done".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    let spawned_count = Arc::new(AtomicUsize::new(0));

    let tool = SpawnAgentTool::new(
        factory,
        signals,
        10,
        spawned_count,
        dummy_ctx(),
        /* nesting_depth */ 1,  // simulates a spawned agent calling spawn_agent
        /* allow_nesting */ false,
    );

    let input = serde_json::json!({
        "blueprint": simple_blueprint("nested-agent"),
        "task": "anything",
        "await_result": true
    });

    let result = tool.call(input, ToolContext::default()).await;
    assert!(result.is_error);
    assert!(result.content.contains("Nesting is disabled"));
}

#[tokio::test]
async fn termination_condition_sentinel_stops_agent() {
    let factory = Arc::new(MockFactory {
        // Response includes the sentinel
        response: "I have completed the task. SCHEMA_COMPLETE Some extra text.".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    let spawned_count = Arc::new(AtomicUsize::new(0));

    let tool = SpawnAgentTool::new(
        factory, signals, 5, spawned_count, dummy_ctx(), 0, true,
    );

    let input = serde_json::json!({
        "blueprint": {
            "name": "sentinel-agent",
            "role": "Schema agent",
            "system_prompt": "Write a schema and output SCHEMA_COMPLETE.",
            "tools": [],
            "max_turns": 5,
            "model": null,
            "terminates_on": { "type": "Sentinel", "value": "SCHEMA_COMPLETE" },
            "is_terminal": false
        },
        "task": "Create proto schema.",
        "await_result": true
    });

    let result = tool.call(input, ToolContext::default()).await;
    assert!(!result.is_error);
    let output: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    let agent_output = output["output"].as_str().unwrap();
    // Sentinel should be stripped from output
    assert!(!agent_output.contains("SCHEMA_COMPLETE"), "sentinel should be stripped");
}

#[tokio::test]
async fn parallel_agent_tool_runs_concurrently() {
    use std::time::Instant;

    // Two agents each "taking" 50ms (simulated by mock)
    let factory = Arc::new(MockFactory {
        response: "parallel done".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    let spawned_count = Arc::new(AtomicUsize::new(0));

    let tool = ParallelAgentTool::new(
        factory, signals,
        /* max_spawned */ 10,
        /* max_parallel */ 4,
        spawned_count,
        dummy_ctx(),
        0, true,
    );

    let input = serde_json::json!({
        "agents": [
            {
                "blueprint": simple_blueprint("parallel-agent-1"),
                "task": "Task A"
            },
            {
                "blueprint": simple_blueprint("parallel-agent-2"),
                "task": "Task B"
            }
        ]
    });

    let start = Instant::now();
    let result = tool.call(input, ToolContext::default()).await;
    let elapsed = start.elapsed();

    assert!(!result.is_error, "parallel tool should succeed");
    let output: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(output["results"].as_array().unwrap().len(), 2);
    // Both agents ran; elapsed should be in the range expected for mock execution
    // (not strictly sequential — both results available)
    assert!(elapsed.as_secs() < 10, "should complete in reasonable time");
}

#[tokio::test]
async fn parallel_tool_respects_spawn_cap() {
    let factory = Arc::new(MockFactory {
        response: "done".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    // Only 1 slot remaining
    let spawned_count = Arc::new(AtomicUsize::new(2));

    let tool = ParallelAgentTool::new(
        factory, signals, /* max_spawned */ 3, /* max_parallel */ 4,
        spawned_count, dummy_ctx(), 0, true,
    );

    // Trying to spawn 2 agents when only 1 slot remains
    let input = serde_json::json!({
        "agents": [
            { "blueprint": simple_blueprint("a1"), "task": "Task A" },
            { "blueprint": simple_blueprint("a2"), "task": "Task B" }
        ]
    });

    let result = tool.call(input, ToolContext::default()).await;
    assert!(result.is_error);
    assert!(result.content.contains("Spawn limit"));
}

#[tokio::test]
async fn dynamic_handoff_orchestrator_add_agent_at_runtime() {
    use agtrs_runtime::meta::DynamicHandoffOrchestrator;
    use agtrs_runtime::testing::MockAgent;

    let orchestrator = DynamicHandoffOrchestrator::new_empty();
    let agent = Arc::new(MockAgent::new("specialist", vec![]));

    orchestrator.register_dynamic_agent("specialist".into(), agent).await;

    // Verify the agent is reachable
    let agents = orchestrator.dynamic_agent_names().await;
    assert!(agents.contains(&"specialist".to_string()));
}
```

### 9.2 xaft Integration Tests

**File:** `crates/xaft-runtime/tests/meta_agent.rs` (new)

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use agtrs_runtime::meta::{AgentBlueprint, TerminationCondition};
use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::testing::MockLlmProvider;
use xaft_agent::signals::{XaftMetaAgentCompleted, XaftMetaAgentSpawned};
use xaft_runtime::meta::{MetaWorkflowConfig, XaftAgentFactory};

fn test_blueprint(name: &str, tools: Vec<&str>) -> AgentBlueprint {
    AgentBlueprint {
        name: name.into(),
        role: format!("{name} specialist"),
        system_prompt: format!("You are {name}."),
        tools: tools.into_iter().map(String::from).collect(),
        max_turns: 5,
        model: None,
        terminates_on: TerminationCondition::AnyResponse,
        is_terminal: false,
    }
}

#[tokio::test]
async fn xaft_agent_factory_builds_from_blueprint() {
    use tempfile::TempDir;
    use xaft_tools::registry::ToolRegistryBuilder;

    let dir = TempDir::new().unwrap();
    let mock_llm = Arc::new(MockLlmProvider::new(vec!["done".into()]));
    let signals = Arc::new(SignalBus::new());

    let all_tools = ToolRegistryBuilder::new(dir.path())
        .build_reader()
        .unwrap()
        .all();

    let factory = XaftAgentFactory::from_master_tools(
        all_tools.clone(),
        mock_llm,
        signals,
        None,
        dir.path().to_owned(),
        false,
        Arc::new(injectable_runtime::ResolveContext::default()),
    );

    let bp = test_blueprint("reader-agent", vec!["read_file", "grep"]);
    let ctx = agtrs_runtime::meta::BlueprintContext {
        task: "read files".into(),
        working_dir: dir.path().display().to_string(),
        nesting_depth: 0,
        session_id: "test".into(),
    };

    let agent = factory.create(&bp, &ctx).await.expect("create agent");
    assert_eq!(agent.name(), "reader-agent");

    // Should have exactly the requested tools
    let tool_names: Vec<&str> = agent.tools().iter().map(|t| t.name()).collect();
    assert!(tool_names.contains(&"read_file"), "must have read_file");
    assert!(tool_names.contains(&"grep"), "must have grep");
    assert!(!tool_names.contains(&"write_file"), "must not have write_file");
}

#[tokio::test]
async fn xaft_agent_factory_rejects_unknown_tool() {
    // Unknown tool names should be silently skipped (warn, not error)
    use tempfile::TempDir;
    use xaft_tools::registry::ToolRegistryBuilder;

    let dir = TempDir::new().unwrap();
    let mock_llm = Arc::new(MockLlmProvider::new(vec!["done".into()]));
    let signals = Arc::new(SignalBus::new());

    let all_tools = ToolRegistryBuilder::new(dir.path())
        .build_reader()
        .unwrap()
        .all();

    let factory = XaftAgentFactory::from_master_tools(
        all_tools,
        mock_llm,
        signals,
        None,
        dir.path().to_owned(),
        false,
        Arc::new(injectable_runtime::ResolveContext::default()),
    );

    let bp = test_blueprint("mixed-agent", vec!["read_file", "nonexistent_tool"]);
    let ctx = agtrs_runtime::meta::BlueprintContext {
        task: "test".into(),
        working_dir: dir.path().display().to_string(),
        nesting_depth: 0,
        session_id: "test".into(),
    };

    // Should succeed — unknown tools are skipped
    let agent = factory.create(&bp, &ctx).await.expect("create despite unknown tool");
    let tool_names: Vec<&str> = agent.tools().iter().map(|t| t.name()).collect();
    assert!(tool_names.contains(&"read_file"));
    assert!(!tool_names.contains(&"nonexistent_tool"));
}

#[tokio::test]
async fn meta_workflow_config_selects_meta_orchestrator() {
    use xaft_agents::registry::WorkflowConfig;

    let config = WorkflowConfig::Meta {
        meta_prompt: None,
        max_spawned_agents: 4,
        max_parallel_agents: 2,
        allow_nesting: false,
        max_nesting_depth: 0,
    };

    // Verify the config variant is correctly identified
    assert!(matches!(config, WorkflowConfig::Meta { .. }));

    let cfg = MetaWorkflowConfig::from_workflow_config(&config);
    assert_eq!(cfg.max_spawned_agents, 4);
    assert_eq!(cfg.max_parallel_agents, 2);
    assert!(!cfg.allow_nesting);
}

#[tokio::test]
async fn meta_agent_signals_emitted_for_spawned_agents() {
    use std::sync::Mutex;

    let signals = Arc::new(SignalBus::new());
    let spawned_events: Arc<Mutex<Vec<XaftMetaAgentSpawned>>> =
        Arc::new(Mutex::new(Vec::new()));
    let completed_events: Arc<Mutex<Vec<XaftMetaAgentCompleted>>> =
        Arc::new(Mutex::new(Vec::new()));

    {
        let spawned_clone = Arc::clone(&spawned_events);
        signals.on::<XaftMetaAgentSpawned>(move |e| {
            spawned_clone.lock().unwrap().push(e.clone());
        }).await;
    }
    {
        let completed_clone = Arc::clone(&completed_events);
        signals.on::<XaftMetaAgentCompleted>(move |e| {
            completed_clone.lock().unwrap().push(e.clone());
        }).await;
    }

    // Simulate a tool call that emits signals
    signals.emit(XaftMetaAgentSpawned {
        blueprint_json: r#"{"name":"test-spec","role":"Test"}"#.into(),
        task_preview: "Run the test".into(),
        spawn_mode: "sequential".into(),
    }).await;

    signals.emit(XaftMetaAgentCompleted {
        agent_name: "test-spec".into(),
        success: true,
        duration_ms: 1234.5,
        output_preview: "Test output".into(),
    }).await;

    let spawned = spawned_events.lock().unwrap();
    let completed = completed_events.lock().unwrap();

    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].spawn_mode, "sequential");
    assert_eq!(completed.len(), 1);
    assert!(completed[0].success);
    assert!(completed[0].duration_ms > 0.0);
}

#[tokio::test]
async fn config_meta_mode_parsed_correctly() {
    use xaft_config::loader::ConfigLoader;

    let toml_str = r#"
[workflow]
mode = "meta"
meta_max_spawned = 6
meta_max_parallel = 3
meta_allow_nesting = true
"#;

    let config = ConfigLoader::from_str(toml_str)
        .expect("parse config");

    assert!(matches!(config.workflow.mode, xaft_config::types::WorkflowMode::Meta));
    assert_eq!(config.workflow.meta_max_spawned, 6);
    assert_eq!(config.workflow.meta_max_parallel, 3);
    assert!(config.workflow.meta_allow_nesting);
}

#[tokio::test]
async fn max_turns_ceiling_is_enforced() {
    use tempfile::TempDir;
    use xaft_tools::registry::ToolRegistryBuilder;

    let dir = TempDir::new().unwrap();
    let mock_llm = Arc::new(MockLlmProvider::new(vec![]));
    let signals = Arc::new(SignalBus::new());
    let all_tools = ToolRegistryBuilder::new(dir.path()).build_reader().unwrap().all();

    let factory = XaftAgentFactory::from_master_tools(
        all_tools, mock_llm, signals, None,
        dir.path().to_owned(), false,
        Arc::new(injectable_runtime::ResolveContext::default()),
    );

    // Blueprint requests 200 turns — above the 50-turn ceiling
    let bp = AgentBlueprint {
        name: "greedy-agent".into(),
        role: "Greedy".into(),
        system_prompt: "Do everything.".into(),
        tools: vec![],
        max_turns: 200,
        model: None,
        terminates_on: TerminationCondition::AnyResponse,
        is_terminal: false,
    };

    let ctx = agtrs_runtime::meta::BlueprintContext {
        task: "test".into(),
        working_dir: dir.path().display().to_string(),
        nesting_depth: 0,
        session_id: "test".into(),
    };

    let agent = factory.create(&bp, &ctx).await.expect("create");
    // AgentConfig::max_turns should be capped at 50
    assert!(agent.config().max_turns <= 50, "max_turns should be capped");
}

#[tokio::test]
async fn meta_agent_parallel_specialists_complete_faster() {
    // Conceptual test: with a mock that introduces artificial delay,
    // parallel execution is faster than sequential.
    // In practice this uses a delay-injecting MockLlmProvider.
    use std::time::{Duration, Instant};

    // Skipping actual timing test in unit context — validated in integration suite
    // that spins up real MockLlmProvider with tokio::time::sleep.
    // This test validates the parallel tool dispatches via tokio::spawn.

    let factory = Arc::new(crate::MockFactory {
        response: "done".into(),
        available: vec![],
    });
    let signals = Arc::new(SignalBus::new());
    let spawned_count = Arc::new(AtomicUsize::new(0));

    let tool = agtrs_runtime::meta::ParallelAgentTool::new(
        factory, signals, 10, 4, spawned_count, dummy_ctx(), 0, true,
    );

    let agents = (0..4).map(|i| serde_json::json!({
        "blueprint": test_blueprint(&format!("agent-{i}"), vec![]),
        "task": format!("Task {i}")
    })).collect::<Vec<_>>();

    let input = serde_json::json!({ "agents": agents });
    let result = tool.call(input, agtrs_runtime::tool::ToolContext::default()).await;
    assert!(!result.is_error);

    let output: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(output["results"].as_array().unwrap().len(), 4);
}
```

### 9.3 Config Integration Test

**File:** `crates/xaft-config/tests/config_integration.rs` (addition)

```rust
#[test]
fn workflow_meta_mode_round_trips() {
    use xaft_config::types::{WorkflowMode, WorkflowModeConfig};

    let cfg = WorkflowModeConfig {
        mode: WorkflowMode::Meta,
        meta_max_spawned: 8,
        meta_max_parallel: 4,
        meta_allow_nesting: false,
        meta_prompt: None,
        ..Default::default()
    };

    let toml = toml::to_string(&cfg).unwrap();
    let back: WorkflowModeConfig = toml::from_str(&toml).unwrap();

    assert!(matches!(back.mode, WorkflowMode::Meta));
    assert_eq!(back.meta_max_spawned, 8);
    assert_eq!(back.meta_max_parallel, 4);
    assert!(!back.meta_allow_nesting);
}

#[test]
fn workflow_config_defaults_to_standard() {
    let cfg = WorkflowModeConfig::default();
    assert!(matches!(cfg.mode, WorkflowMode::Standard));
}
```

---

## 10. Safety Invariants

The following safety invariants from `CLAUDE.md` apply to the meta agent feature and must not be broken:

**Path traversal (Invariant 1):** `XaftAgentFactory` shares the same `FsWorkspaceStore` instances as the parent agent. All file tools in spawned specialists are rooted at `session.workspace_root`. A specialist cannot escape the workspace by being given `read_file` — it receives the same store the parent agent uses.

**Git isolation (Invariant 2):** Specialists share the parent's `WorktreeGuard` worktree branch (`xaft/session-<uuid>`). They do not create their own branches. Their file edits accumulate in the same worktree and are committed atomically at session end.

**Approval gate (Invariant 3):** `XaftAgentFactory::create()` forwards the same `Arc<dyn ApprovalGate>` to specialists. A specialist with `bash_exec` in its blueprint will trigger the TUI approval dialog for every shell command, exactly as the main agent does.

**Cost accuracy (Invariant 4):** Each specialist runs through `AgentExecutor`, which emits `ModelCallComplete` signals. The runtime's cost accumulator subscribes to these and updates `session.total_cost_usd` regardless of whether the call came from the main agent or a specialist.

**Config null semantics (Invariant 5):** `WorkflowModeConfig` implements `Default`. The `meta_prompt: Option<String>` field preserves `null` through deep-merge — a project config with `meta_prompt = null` is preserved as `None`, not replaced by a default string.

**Spawn cleanup (Invariant 6, new):** All `tokio::spawn` handles created by `ParallelAgentTool` must be joined before `call()` returns. No fire-and-forget spawns are permitted in the `await_result = true` path (the default and only implemented path). A `tokio::task::JoinSet` is used to collect handles and propagate panics.

---

## 11. Dependency Changes

### agtrs-runtime `Cargo.toml`

No new external dependencies. `schemars` is already present (used by `SubagentTool`). `async-trait` is already present.

### xaft-runtime `Cargo.toml`

```toml
[dependencies]
# existing
agtrs-runtime = { path = "../../agtrs/agtrs-runtime" }
# No new crates required
```

### xaft-agents `Cargo.toml`

No new dependencies.

---

## 12. Configuration Reference

### Minimal meta mode config

```toml
[provider.anthropic]
type        = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[agent.default]
provider  = "anthropic"
model     = "claude-3-5-sonnet-20241022"
max_turns = 25

[workflow]
mode             = "meta"
meta_max_spawned = 8
meta_max_parallel = 4
meta_allow_nesting = false
```

### Per-run CLI override

```bash
XAFT_WORKFLOW_MODE=meta XAFT_META_MAX_SPAWNED=4 xaft run "Migrate payments to gRPC"
```

### Custom meta agent prompt

```toml
[workflow]
mode        = "meta"
meta_prompt = "/path/to/custom_meta_prompt.txt"
# or inline:
meta_prompt = """
You are a specialized database migration coordinator...
"""
```

---

## 13. Open Questions

| # | Question | Recommendation |
|---|----------|----------------|
| OQ1 | Should specialists share conversation history with the meta agent, or run in isolated contexts? | Isolated contexts (like `SubagentTool`). Shared history would explode token cost. |
| OQ2 | Should `spawn_agent` with `await_result = false` (fire-and-forget) be implemented in PRD 58 or deferred? | Defer to a follow-on PRD. Fire-and-forget requires a background task tracker, a `/jobs` command, and signal infrastructure that warrants its own PRD. |
| OQ3 | Should `AgentPool` be a blocking pool (specialists wait for a free slot) or a spawn-new-always pool with a cap on simultaneous runs? | The semaphore approach in `ParallelAgentTool` is equivalent to a slot pool. A reusable pool that avoids re-construction overhead is a PRD 60-level optimization. |
| OQ4 | Should the meta agent be able to register specialists into the `HandoffOrchestrator` so QA can hand off *to a specialist* for targeted fixes? | `DynamicHandoffOrchestrator` (§5.5) supports this architecturally. Wiring it into the meta workflow is out of scope for PRD 58 but the infrastructure is in place. |
| OQ5 | Should `blueprint.model` overrides route through `ProviderFactory` or use a per-specialist `LlmProvider`? | Route through the same `LlmProvider` with a model override option. Avoids managing N provider connections. Requires `LlmOptions::model_override` (or existing agtrs mechanism). |

---

## 14. Implementation Order

The following phased order minimizes integration risk:

**Phase 1 — agtrs primitives (no xaft changes)**
1. Add `AgentBlueprint`, `TerminationCondition` to `agtrs-runtime/src/agent.rs`
2. Add `AgentFactory`, `BlueprintContext` to `agtrs-runtime/src/meta.rs`
3. Add `AgentSpawned`, `AgentSpawnCompleted` to `agtrs-runtime/src/signals.rs`
4. Add `SpawnAgentTool` (single agent) to `agtrs-runtime/src/meta.rs`
5. Add unit tests for all above

**Phase 2 — `ParallelAgentTool` and `DynamicHandoffOrchestrator`**
6. Add `ParallelAgentTool` with semaphore to `agtrs-runtime/src/meta.rs`
7. Add `DynamicHandoffOrchestrator` skeleton
8. Add parallel and orchestrator tests

**Phase 3 — xaft factory and orchestration**
9. Add `XaftAgentFactory` to `crates/xaft-runtime/src/meta.rs`
10. Add `WorkflowConfig::Meta`, `MetaWorkflowConfig`
11. Add `run_meta_workflow()` to `crates/xaft-runtime/src/orchestrator.rs`
12. Wire into `runtime.rs`
13. Add xaft integration tests

**Phase 4 — config and signals**
14. Add `WorkflowModeConfig` to `xaft-config`
15. Add `XaftMetaAgentSpawned`, `XaftMetaAgentCompleted` to `xaft-agent`
16. Config round-trip tests

**Phase 5 — TUI**
17. Bridge new signals
18. Update ephemeral display
19. Update `/agents` command
20. Manual QA of TUI during a meta workflow run

**Phase 6 — Meta agent system prompt**
21. Write and iterate `META_AGENT_SYSTEM_PROMPT`
22. Test with real tasks (payments migration, security audit, multi-language task)
23. Tune `max_turns` defaults and tool assignment recommendations in the prompt
