# Extension Points

## Purpose

This document catalogs every point in the xaft system where behavior can be extended without modifying existing code. Each extension point is defined by a trait, a builder API, or a configuration hook. By implementing the appropriate interface and registering your implementation with the appropriate registry, you can add new capabilities—tools, agents, providers, planners, workflows, signals, and TUI widgets—while the rest of the system remains oblivious to your addition.

Understanding extension points is critical for two reasons. First, it tells you the correct way to add a feature: implement the trait, register it, done. Second, it tells you what *not* to do: if there is no extension point for what you want, you must add one to the core system rather than hacking around the existing ones. Extension points are the contract between the core and its plugins; respecting them keeps the system modular and testable.

---

## Mental Model

xaft is built on the principle of **open for extension, closed for modification**. The core runtime, orchestrator, and agent loop define stable interfaces (traits) and stable composition mechanisms (registries, builders). New behavior is added by implementing these interfaces and plugging the implementations into the composition mechanisms. The core never needs to know about specific implementations; it only knows about the trait.

This is not accidental architecture—it is a deliberate design constraint that ensures the system can grow without becoming a monolith. Every time you add a feature by implementing a trait rather than editing core code, you are preserving the ability of every other feature to evolve independently.

---

## Architecture Explanation

### Custom Tools (implement `Tool` trait)

The `Tool` trait is the primary extension mechanism for adding new capabilities to agents. Every operation an agent can perform—from reading a file to running a shell command—is a `Tool` implementation. Adding a new tool requires three steps:

1. **Implement the `Tool` trait.** This trait requires four items:
   - `fn name(&self) -> &str` — A unique identifier for the tool. This is what the LLM uses to invoke the tool.
   - `fn description(&self) -> &str` — A natural language description that the LLM reads to decide when to use the tool.
   - `fn parameters(&self) -> serde_json::Value` — A JSON Schema object describing the tool's input parameters. This tells the LLM what arguments to provide.
   - `async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, ToolError>` — The actual implementation. Receives deserialized parameters and returns either a successful output (which can be text, structured data, or an error message for the LLM) or a tool error (which the agent loop handles according to its error policy).

2. **Register the tool in the `ToolRegistry`.** The registry maps tool names to implementations. After registration, the tool is available for assignment to agents.

3. **Assign the tool to agents.** In the orchestration configuration, specify which agents receive the new tool. Tools can be categorized as "read" or "write" to simplify assignment—read-only tools are safe for any agent, while write tools are restricted to agents that are expected to modify the codebase.

Tool implementations must be `Send + Sync + 'static` because they are shared across async tasks. They should be stateless when possible; if state is needed, it should be encapsulated within the tool struct and cloned per invocation.

### Custom Agents (`AgentBuilder`)

The `AgentBuilder` provides a fluent API for constructing new agents without subclassing or modifying the core `XaftAgent` struct. An agent is defined by its name, role description, tool set, commit policy, signal subscriptions, and behavioral limits (max turns, temperature, cost limit). The builder pattern ensures that all required fields are set before the agent is constructed, and it provides sensible defaults for optional fields.

To create a custom agent:

```rust
let reviewer = AgentBuilder::new()
    .name("reviewer")
    .role("You are a senior code reviewer. You examine changes for correctness, style, and security issues.")
    .tools(review_tool_set)
    .commit_policy(CommitPolicy::Never) // Reviewers never commit
    .max_turns(10)
    .temperature(0.2) // Low temperature for consistent reviews
    .stream_sink(signal_bus.sink())
    .build()?;
```

Custom agents are registered in the `AgentRegistry`, which maps agent names to `DynamicNamedAgent` instances. The orchestrator looks up agents by name when constructing the handoff workflow.

### Custom Providers (`LlmProvider` trait)

The `LlmProvider` trait abstracts LLM backends. Implementing this trait allows xaft to use any LLM API—commercial, self-hosted, or local. The trait defines:

- `fn name(&self) -> &str` — Provider identifier for logging and error messages.
- `async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>` — Non-streaming completion.
- `async fn complete_stream(&self, request: LlmRequest) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>>>>, LlmError>` — Streaming completion.

The `LlmRequest` struct contains the model identifier, the conversation messages, the available tools (serialized as function declarations), and generation parameters (temperature, max tokens, stop sequences). The `LlmResponse` struct contains the generated content (text and/or tool calls) and usage metadata (prompt tokens, completion tokens).

Custom providers are added to the provider chain in the runtime configuration. The chain is ordered by priority; the first provider is tried first, and subsequent providers are tried on retriable failures. This means you can add a fallback provider without modifying any existing provider code.

### Custom Planners (planning cascade config)

The planning subsystem is configurable through the `PlanConfig` struct, which controls the planner's behavior without requiring code changes. The cascade configuration specifies:

- **Escalation policy** — What happens when the planner determines that the task is too complex for a single planning pass. Options include: `AutoEscalate` (automatically break the task into sub-tasks), `AskUser` (prompt the user for guidance), and `FailFast` (return an error).
- **Max refinement iterations** — How many times the planner can refine its own plan before committing. Each refinement incorporates feedback from the previous iteration's evaluation.
- **Inject plan message** — Whether to inject the final plan as a system message into the coder agent's conversation, ensuring the coder sees the plan without needing to read it from a file.
- **Enable replan tool** — Whether to give the planner a `ReplanTool` that allows mid-execution re-planning if the coder encounters unexpected obstacles.

For more radical customization, you can implement a custom planner by creating an agent with `PlanAgentBuilder` and a specialized set of planning tools (e.g., a dependency analysis tool, an architecture decision tool). Register the custom planner in the `AgentRegistry` and reference it in the workflow configuration.

### Custom Workflows (`AgentRegistry` + `WorkflowConfig::Dynamic`)

The default workflow is a fixed sequence: Planner → Coder → QA → Fixer. Custom workflows allow arbitrary agent topologies. There are two mechanisms:

1. **`WorkflowConfig::Dynamic`** — Specify the initial agent name, and let the handoff tools determine the flow at runtime. Each agent's `HandoffTool` specifies which agent to hand off to, enabling conditional routing (e.g., the QA agent hands off to the Fixer only if it finds issues, otherwise it hands off to the "Done" sentinel).

2. **`AgentRegistry`** — Register custom `DynamicNamedAgent` instances that the orchestrator can look up by name. This allows you to define entirely new workflows composed of arbitrary agents with arbitrary handoff targets.

Custom workflows are configured in the project-level config file (`.xaft/config.toml`). The runtime reads the workflow config at bootstrap time and constructs the orchestrator accordingly. Changing the workflow config and restarting xaft is sufficient to switch workflows; no code changes are required.

### New Signals (define struct, derive traits, subscribe/emit)

The signal system is extensible by design. Adding a new signal type requires:

1. **Define a struct** that represents the signal event. It must derive `Clone`, `Send`, `'static`, and the `Signal` marker trait (which is a simple derive macro).
2. **Emit the signal** from the appropriate lifecycle hook or tool implementation using the `SignalBus::emit()` method.
3. **Subscribe to the signal** from any component that needs to react to it using the `SignalBus::subscribe::<T>()` method, which returns a broadcast receiver.

Signal types are defined in the `xaft-agent` crate because that is where most signals originate. The TUI and other consumers subscribe to signal types they care about. Adding a new signal does not require modifying any existing code—only adding the new type and the code that emits and subscribes to it.

Example: adding a `CodeMetricsComputed` signal that fires after the QA agent computes code quality metrics:

```rust
#[derive(Clone, Debug, Signal)]
pub struct CodeMetricsComputed {
    pub session_id: Uuid,
    pub agent_name: String,
    pub complexity: f64,
    pub coverage_estimate: f64,
}

// Emit from the QA agent's on_finish hook:
self.signal_bus.emit(CodeMetricsComputed {
    session_id: self.session_id,
    agent_name: self.name.clone(),
    complexity: metrics.complexity(),
    coverage_estimate: metrics.coverage_estimate(),
});

// Subscribe from the TUI:
let mut rx = signal_bus.subscribe::<CodeMetricsComputed>();
tokio::spawn(async move {
    while let Ok(signal) = rx.recv().await {
        app_state.update_metrics(signal.complexity, signal.coverage_estimate);
    }
});
```

### TUI Widgets (ratatui `Widget` trait + `AppState` data)

The TUI is built on ratatui, which uses the `Widget` trait for rendering. Adding a new widget requires:

1. **Implement the `Widget` trait** for your widget struct. The `render` method receives a `Buffer` and an `Area`, and it writes characters into the buffer to produce the visual output.
2. **Add state to `AppState`** if the widget needs runtime data. `AppState` is a shared mutable struct that the TUI updates in response to signals.
3. **Subscribe to the relevant signals** and update `AppState` in the signal handler. The widget reads from `AppState` during rendering and has no knowledge of how the state was populated.
4. **Add the widget to the layout** in the main TUI application's `render` method. This determines where on the screen the widget appears and how much space it gets.

Widgets are completely self-contained: they receive data through `AppState` and render it through the `Widget` trait. They never interact with the runtime directly. This means you can add, remove, or rearrange widgets without touching any other part of the system.

---

## Extension Patterns

### Pattern: Tool with Approval Gate

Some tools need user approval before execution (e.g., running a shell command, deleting a file). Implement this pattern by:

1. Defining a `RequiresApproval` signal that carries a description of the action and a oneshot sender for the user's response.
2. Emitting the signal from the tool's `execute` method before performing the action.
3. Awaiting the oneshot receiver to get the user's decision.
4. Proceeding or returning an error based on the decision.

This pattern keeps the approval logic out of the tool implementation—the tool simply asks "may I?" and waits for an answer. The TUI (or any other subscriber) provides the answer.

### Pattern: Agent with Conditional Handoff

Some agents need to decide at runtime whether to hand off or complete. Implement this by:

1. Giving the agent a `HandoffTool` with a configurable target.
2. In the agent's system prompt, instruct it to use the handoff tool only when certain conditions are met (e.g., "hand off to the fixer only if you find issues").
3. The orchestrator interprets the handoff tool call and routes control to the specified agent.

This pattern enables dynamic workflows where the path through agents depends on the content of the work, not just a fixed sequence.

### Pattern: Provider with Rate Limit Awareness

Custom providers can implement rate limit handling internally by:

1. Tracking request timestamps and token counts in the provider struct.
2. In the `complete` method, checking if the rate limit would be exceeded and, if so, sleeping until the limit resets.
3. Returning a `LlmError::RateLimited` variant if the wait would exceed a configured timeout.

The runtime's provider chain failover complements this pattern: if a provider returns `RateLimited`, the runtime tries the next provider in the chain, so the user sees no delay if a fallback is available.

---

## Common Pitfalls

1. **Forgetting to register a tool in the registry.** Implementing the `Tool` trait is necessary but not sufficient. If the tool is not registered in the `ToolRegistry`, no agent will ever see it, and no LLM will ever invoke it. Always test that your tool appears in the registry after registration.

2. **Using the wrong conversation key for a custom agent.** Each agent's conversation history is partitioned by its conversation key. If two agents share a key, their messages will be interleaved, confusing the LLM. Always use a unique key per agent (typically `"{session_id}::{agent_name}"`).

3. **Making a tool that depends on runtime state.** Tools should be self-contained. If a tool needs runtime state (e.g., the session ID, the worktree root), pass it as a parameter at construction time, not by reaching into global state. Global state makes tools untestable and breaks the extension contract.

4. **Subscribing to signals too late.** The `SignalBus` uses broadcast channels, which do not buffer messages for future subscribers. If you subscribe after a signal has been emitted, you will miss it. Subscribe during bootstrap, before the orchestrator starts.

5. **Implementing a provider that panics on error.** The provider chain relies on errors being returned, not panicked. A panic in a provider will crash the entire runtime, bypassing the failover mechanism. Always return `Err(LlmError::...)` instead of panicking.

6. **Adding TUI widgets that block the render loop.** The TUI render loop runs on every frame (typically 60 FPS). If a widget's `render` method performs expensive computation or blocking I/O, it will cause the entire UI to stutter. Pre-compute widget data in background tasks and store it in `AppState`.

---

## Invariants

- **I1: Trait-based extension.** All extension points are defined by traits. There are no "convention-based" extensions (e.g., "name your function `tool_*` and it will be discovered").
- **I2: Registration required.** Implementations must be explicitly registered in their respective registries. There is no auto-discovery.
- **I3: Thread safety.** All trait implementations must be `Send + Sync + 'static`. The runtime shares them across async tasks without further synchronization.
- **I4: No cross-crate signal reach-through.** A signal type is defined in the crate that owns the domain. Consumers depend on that crate for the type definition; they do not duplicate it.
- **I5: Widget isolation.** TUI widgets read from `AppState` and render to a `Buffer`. They never call runtime methods, emit signals, or perform I/O.

---

## Lifecycle Expectations

**Registration phase:** During bootstrap, the runtime registers all tools, agents, and providers based on the configuration. This is the only time registries are mutable. After bootstrap, registries are frozen: no tools can be added, no agents can be registered, and no providers can join the chain. This ensures that the orchestrator has a consistent view of available capabilities throughout the task.

**Execution phase:** Registered implementations are used but not modified. Tools are executed, agents are invoked, providers are called, and signals are emitted and received. The registries themselves are read-only during this phase.

**Hot-reload exception:** Configuration can be hot-reloaded during execution, but this only changes parameter values (e.g., temperature, max_turns), not the set of registered implementations. If you need to add a tool at runtime, you must restart the runtime.

**Cleanup phase:** When the runtime shuts down, registries are dropped. There is no "unregistration" step; implementations are simply deallocated.

---

## Examples

### Complete custom tool implementation

```rust
use xaft_tools::{Tool, ToolOutput, ToolError};
use async_trait::async_trait;

pub struct CountLinesTool;

#[async_trait]
impl Tool for CountLinesTool {
    fn name(&self) -> &str { "count_lines" }

    fn description(&self) -> &str {
        "Counts the number of lines in a file. Returns the line count and file size."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the worktree root"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path: String = serde_json::from_value(params)
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let content = tokio::fs::read_to_string(&path).await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let lines = content.lines().count();
        Ok(ToolOutput::Text(format!("{} lines in {}", lines, path)))
    }
}
```

### Custom workflow configuration

```toml
# .xaft/config.toml
[workflow]
type = "dynamic"
initial_agent = "architect"

[workflow.agents.architect]
role = "Design the system architecture before coding"
handoff_target = "coder"

[workflow.agents.coder]
role = "Implement the designed architecture"
handoff_target = "reviewer"

[workflow.agents.reviewer]
role = "Review for security and performance"
handoff_target = "fixer"  # only if issues found
```

---

## Implementation Guidance

When adding an extension point that does not yet exist, follow these steps:

1. **Define the trait** in the crate that owns the domain. The trait should be minimal—include only the methods that the core system needs to call. Avoid "kitchen sink" traits that try to cover every possible use case.
2. **Add a registry** in the same crate, if one does not exist. The registry should support insertion during bootstrap and read-only access during execution.
3. **Add a builder method** for the new extension point to the appropriate builder (e.g., `AgentBuilder::add_custom_hook`).
4. **Document the extension point** in this file, including the trait definition, registration procedure, and an example implementation.
5. **Write integration tests** that exercise the extension point end-to-end, including registration, invocation, and cleanup.

When in doubt about whether something should be an extension point, err on the side of making it one. It is much easier to consolidate extension points later than to extract them from hardcoded behavior.
