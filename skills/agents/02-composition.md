# Agent Composition

## Purpose

This document explains how to compose agents in xaft using the builder APIs. Agent composition is the process of assembling an agent from its constituent parts—name, role, tools, policies, signals, and behavioral limits—without subclassing or modifying the core `XaftAgent` implementation. The builder pattern ensures that every agent is fully configured before execution, and the fluent API makes the configuration readable and self-documenting.

Understanding agent composition is essential for creating new agent types, customizing existing agents for specific projects, and building dynamic workflows where agents are constructed at runtime based on configuration. This document covers both the basic `AgentBuilder` and the specialized `PlanAgentBuilder`, as well as the `DynamicNamedAgent` wrapper that enables runtime agent lookup.

---

## Mental Model

Think of agent composition as assembling a specialist for a mission. You choose their specialty (role), equip them with specific tools, set their operational parameters (temperature, max turns, cost limit), define their reporting rules (stream sink, signals), and specify their post-mission protocol (commit policy). The builder is the checklist that ensures no equipment is forgotten and no parameter is left at an inappropriate default.

The key principle is **composition over inheritance**. There is no `PlannerAgent` subclass or `CoderAgent` subclass. Instead, the Planner and Coder are both `XaftAgent` instances composed with different roles, tools, and policies. This means you can create new agent types without modifying the agent crate—you simply compose them differently. A "SecurityReviewer" is not a new class; it's an `XaftAgent` with a security-focused role and a read-only tool set.

---

## Architecture Explanation

### `AgentBuilder` Fluent API

The `AgentBuilder` is the primary mechanism for constructing agents. It follows the builder pattern: methods set a configuration parameter and return `&mut Self`, enabling method chaining. The `build()` method consumes the builder and produces an `XaftAgent`, performing validation to ensure all required fields are set.

#### Required Fields

These fields must be set before calling `build()`. If any is missing, `build()` returns a `BuilderError::MissingField`.

- **`name(name: impl Into<String>)`** — The agent's identifier. This must be unique within a workflow because it is used for conversation key construction, session partitioning, handoff targeting, and signal attribution. The name should be descriptive and stable (e.g., "planner", "coder", "qa", "fixer"). Avoid using dynamic or generated names that change between runs, as this would break session resumption.

- **`role(role: impl Into<String>)`** — The system prompt that defines the agent's behavior. This is the single most important parameter because it directly determines what the LLM does. A well-crafted role prompt should specify: the agent's area of expertise, the kinds of decisions it should make, the tools it should prefer, the format of its output, and any constraints on its behavior (e.g., "never modify files outside the src/ directory"). Poor role prompts are the most common cause of agent misbehavior.

- **`tools(tools: Vec<Box<dyn Tool>>)`** — The set of tools the agent can invoke. The tool set defines the agent's capabilities and, implicitly, its limitations. A planner with only read tools cannot modify files; a coder with write tools can. The tool set also influences the LLM's behavior: the LLM sees tool descriptions and will only use tools that are available. Adding irrelevant tools to an agent's set wastes tokens and can confuse the LLM.

#### Optional Fields with Defaults

These fields have sensible defaults but can be overridden for specific agents.

- **`commit_policy(policy: CommitPolicy)`** — Determines when the agent auto-commits its changes. Default: `CommitPolicy::Never`. The Coder and Fixer agents typically override this to `OnSuccess`. The Planner and QA agents keep the default because they do not make file changes.

- **`stream_sink(sink: mpsc::Sender<Signal>)`** — The channel through which tool results and lifecycle events are forwarded to the signal bus. Default: a no-op sink that discards signals (useful for testing). Production agents always set this to the signal bus's sender.

- **`signals(bus: SignalBus)`** — The signal bus instance, used for emitting typed signals at lifecycle boundaries. Default: a local signal bus with no subscribers (signals are emitted but nobody receives them). Production agents always set this to the runtime's signal bus.

- **`max_turns(turns: usize)`** — The maximum number of ReAct iterations before the agent is forcefully terminated. Default: from the merged configuration (typically 20). Lower values prevent runaway agents; higher values allow more complex tasks. The planner typically uses fewer turns (10-15) because planning is less iterative, while the coder uses more (20-30) because coding often requires multiple edit-test cycles.

- **`temperature(temp: f64)`** — The LLM sampling temperature. Default: from the merged configuration (typically 0.7). Lower temperatures (0.1-0.3) produce more deterministic, consistent outputs, which is desirable for coding and reviewing. Higher temperatures (0.7-1.0) produce more creative, exploratory outputs, which is desirable for planning and brainstorming.

- **`cost_limit(limit: f64)`** — An optional dollar limit on LLM API costs for this agent. If the cumulative cost exceeds this limit, the agent terminates with a `CostLimitExceeded` error. Default: no limit. Setting a cost limit is recommended for agents that may enter expensive loops (e.g., a fixer that repeatedly attempts to fix a failing test).

#### `build()` Validation

The `build()` method performs the following checks:

1. **Required fields are set.** `name`, `role`, and `tools` must be provided.
2. **Name is not empty.** An empty name would break conversation key construction.
3. **Temperature is in [0, 2].** Values outside this range are rejected by most LLM APIs.
4. **Max turns is at least 1.** An agent that cannot make any LLM calls is useless.
5. **Cost limit is positive.** A negative or zero cost limit would immediately terminate the agent.
6. **Tool names are unique.** Duplicate tool names would confuse the LLM's tool selection.

If all checks pass, `build()` returns `Ok(XaftAgent)`. If any check fails, it returns `Err(BuilderError)` with a descriptive message.

### `PlanAgentBuilder`

The `PlanAgentBuilder` is a specialized builder that wraps `AgentBuilder` with planning-specific configuration. It produces an agent that is optimized for the planning role, with additional parameters that control how the planner interacts with the planning cascade.

#### Construction

The `PlanAgentBuilder` is created from an existing `AgentBuilder`:

```rust
let planner = PlanAgentBuilder::from(
    AgentBuilder::new()
        .name("planner")
        .role("You are a senior software architect...")
        .tools(read_tools)
        .max_turns(15)
        .temperature(0.4)
)
.plan_config(PlanConfig { ... })
.build()?;
```

This design ensures that all the standard `AgentBuilder` parameters are available, plus the planning-specific parameters. The `PlanAgentBuilder` does not duplicate the `AgentBuilder`'s API; it delegates to it.

#### `plan_config` Fields

The `PlanConfig` struct controls the planner's behavior through the following fields:

- **`escalation_policy: EscalationPolicy`** — What happens when the planner determines that the task is too complex for a single planning pass. The options are:
  - `AutoEscalate` — Automatically break the task into sub-tasks and create a multi-step plan. This is the default because it handles the most common case (tasks that require multiple files or multiple types of changes).
  - `AskUser` — Emit a signal that prompts the user for guidance. The planner waits for a user response before continuing. This is useful for tasks where the planner is uncertain about the user's intent.
  - `FailFast` — Return an error indicating that the task is too complex. This is useful in automated pipelines where human intervention is not possible.

- **`max_refinement_iterations: usize`** — How many times the planner can refine its own plan before committing. Each refinement iteration involves the planner reviewing its previous plan, evaluating it against the codebase, and producing an improved version. The default is 2: one initial plan and one refinement. Higher values allow more thorough planning at the cost of increased LLM usage. Setting this to 0 disables refinement entirely—the planner's first draft is final.

- **`inject_plan_message: bool`** — Whether to inject the final plan as a system message into the coder agent's conversation. When true, the coder receives the plan as a structured message (not just as handoff context), ensuring it is prominently visible in the conversation history. When false, the plan is only available through the handoff context message. Default: true.

- **`enable_replan_tool: bool`** — Whether to give the planner a `ReplanTool` that allows mid-execution re-planning. When enabled, the planner receives the `ReplanTool` in addition to its read tools. If the coder encounters unexpected obstacles and reports them back (via a special signal), the planner can use the `ReplanTool` to revise the plan without restarting the entire workflow. Default: false (because re-planning adds complexity and token cost).

#### How PlanConfig Affects Agent Construction

When `build()` is called on a `PlanAgentBuilder`, it performs the following steps:

1. **Build the inner `AgentBuilder`** — This produces a base `XaftAgent` with the standard configuration.
2. **Add planning tools** — If `enable_replan_tool` is true, the `ReplanTool` is added to the agent's tool set.
3. **Wrap the role prompt** — The role prompt is extended with planning-specific instructions based on the `PlanConfig`. For example, if `escalation_policy` is `AutoEscalate`, the prompt is extended with "When the task is too complex for a single step, break it into sub-tasks and create a multi-step plan."
4. **Set up refinement context** — If `max_refinement_iterations > 0`, the `before_llm_call` hook is configured to inject a "refinement iteration N of M" message after the initial plan is produced, prompting the LLM to review and improve its plan.

### `DynamicNamedAgent`

The `DynamicNamedAgent` is a wrapper that pairs an agent with its name, enabling runtime lookup by the orchestrator. It is the unit of registration in the `AgentRegistry`.

```rust
pub struct DynamicNamedAgent {
    pub name: String,
    pub agent: Box<dyn Agent>,
    pub default_handoff_target: Option<String>,
}
```

The `default_handoff_target` field specifies which agent this agent should hand off to by default. This is used by the `HandoffTool` when the agent does not specify an explicit target. For example, the Planner's default handoff target is "coder", and the Coder's is "qa".

The `DynamicNamedAgent` is constructed manually or via a convenience method on `AgentBuilder`:

```rust
let named_agent = AgentBuilder::new()
    .name("reviewer")
    .role("You are a code reviewer...")
    .tools(review_tools)
    .into_named_agent(Some("fixer"))  // default handoff target
    .build()?;
```

The `AgentRegistry` stores `DynamicNamedAgent` instances in a `HashMap<String, DynamicNamedAgent>`. The orchestrator looks up agents by name when constructing the handoff workflow. If a name is not found in the registry, the orchestrator returns an error at construction time (not at runtime), ensuring that misconfigured workflows fail fast.

---

## Extension Patterns

### Creating a Specialized Agent for a Domain

To create an agent tailored to a specific domain (e.g., database migrations):

```rust
let migrator = AgentBuilder::new()
    .name("migrator")
    .role("You are a database migration specialist. You analyze schema changes, \
           generate migration files, and verify backward compatibility. \
           Always create both up and down migrations. Never modify existing \
           migrations. Use the `run_command` tool to test migrations against \
           a local database before marking them as complete.")
    .tools(vec![
        Box::new(ReadFileTool::new(worktree_root.clone())),
        Box::new(WriteFileTool::new(worktree_root.clone())),
        Box::new(EditFileTool::new(worktree_root.clone())),
        Box::new(RunCommandTool::new(worktree_root.clone())),
        Box::new(HandoffTool::new("qa")),
    ])
    .commit_policy(CommitPolicy::OnSuccess)
    .max_turns(25)
    .temperature(0.2)
    .cost_limit(2.0)
    .stream_sink(signal_bus.sink())
    .signals(signal_bus.clone())
    .build()?;
```

The key is the role prompt: it is specific enough to guide the LLM's behavior without being so rigid that it prevents the LLM from adapting to unexpected situations. Good role prompts include: what the agent does, what tools it should prefer, what constraints it must respect, and what format its output should follow.

### Composing a Planning Agent with Custom Escalation

To create a planner that asks the user for guidance on complex tasks:

```rust
let interactive_planner = PlanAgentBuilder::from(
    AgentBuilder::new()
        .name("planner")
        .role("You are a software architect who creates implementation plans...")
        .tools(read_tools)
        .max_turns(15)
        .temperature(0.5)
        .stream_sink(signal_bus.sink())
        .signals(signal_bus.clone())
)
.plan_config(PlanConfig {
    escalation_policy: EscalationPolicy::AskUser,
    max_refinement_iterations: 3,
    inject_plan_message: true,
    enable_replan_tool: true,
})
.build()?;
```

This planner will emit a signal asking the user for guidance when it encounters a task that is too complex for a single planning pass. The TUI displays the prompt, the user responds, and the planner incorporates the response into its plan.

### Registering a Custom Agent in the AgentRegistry

To make a custom agent available for dynamic workflows:

```rust
let registry = AgentRegistry::new();

registry.register(DynamicNamedAgent {
    name: "security_reviewer".into(),
    agent: Box::new(security_agent),
    default_handoff_target: Some("fixer".into()),
});

// The orchestrator can now reference "security_reviewer" in workflow configs
let orchestrator = HandoffOrchestrator::new(registry, workflow_config);
```

---

## Common Pitfalls

1. **Setting an inappropriate temperature for the agent's role.** A coder agent with temperature 1.0 will produce inconsistent code. A planner agent with temperature 0.1 will produce unimaginative plans. Match the temperature to the role: low for precision tasks, high for creative tasks.

2. **Forgetting to set `stream_sink` and `signals` in production.** The defaults are no-op sinks and local signal buses. If you forget to set these, the agent will run correctly but the TUI will show no activity and no signals will be emitted. This is the most common mistake when constructing agents manually (as opposed to using the orchestrator, which sets these automatically).

3. **Using the same name for two agents in a workflow.** The `AgentRegistry` uses a `HashMap`, so registering two agents with the same name will silently overwrite the first. The orchestrator will only see the second agent, and any handoff targeting the first agent's name will go to the second. Always use unique names.

4. **Setting `max_turns` too low for complex tasks.** A coder agent with `max_turns=5` may not have enough iterations to implement a feature that requires reading multiple files, making several edits, and running tests. Estimate the number of tool calls your task will require and set `max_turns` with a comfortable margin.

5. **Enabling `enable_replan_tool` without handling re-plan signals.** The `ReplanTool` emits a signal when the planner decides to re-plan. If the orchestrator does not handle this signal (by pausing the coder, re-invoking the planner, and then resuming the coder), the re-plan will have no effect. Always implement the handler when enabling this tool.

6. **Overriding `PlanConfig` defaults without understanding the implications.** Setting `max_refinement_iterations=0` means the planner's first draft is final, which can lead to poor plans for complex tasks. Setting `inject_plan_message=false` means the coder may not see the plan prominently, leading to implementations that deviate from the plan.

---

## Invariants

- **I1: Builder consumption.** Calling `build()` consumes the builder. The builder cannot be reused to produce multiple agents. If you need multiple identical agents, create multiple builders.
- **I2: Name uniqueness in registry.** Each agent name in an `AgentRegistry` must be unique. Duplicate names cause silent overwrites.
- **I3: Tool set immutability after build.** Once an agent is built, its tool set cannot be modified. The ReAct loop uses the tool set as-is for the entire execution.
- **I4: PlanConfig delegation.** `PlanAgentBuilder` always delegates to an `AgentBuilder` for standard parameters. It does not duplicate or override the builder's validation logic.
- **I5: DynamicNamedAgent registration.** Agents must be registered in the `AgentRegistry` before the orchestrator is constructed. The orchestrator does not support runtime agent addition.

---

## Lifecycle Expectations

**Construction:** Building an agent is cheap—it involves no I/O, no network calls, and no heavy computation. It is simply assembling a struct from its parts. This means you can construct agents eagerly during bootstrap without impacting startup time.

**Registration:** Registering a `DynamicNamedAgent` in the `AgentRegistry` is an O(1) operation (HashMap insert). The registry is mutable only during the registration phase (bootstrap). After the orchestrator is constructed, the registry is frozen.

**Execution:** During execution, the `DynamicNamedAgent` wrapper is used only for lookup. The orchestrator extracts the inner `Agent` and runs it independently. The wrapper's `default_handoff_target` is read once at handoff time.

**Destruction:** After execution, agents are dropped. There is no explicit "unregistration" from the registry. The registry outlives individual agent executions and is dropped when the runtime shuts down.

---

## Examples

### Composing all four standard agents

```rust
// Shared configuration
let sink = signal_bus.sink();
let bus = signal_bus.clone();
let worktree_root = worktree.root().to_path_buf();

// Read tools (shared by Planner and QA)
let read_tools: Vec<Box<dyn Tool>> = vec![
    Box::new(ReadFileTool::new(worktree_root.clone())),
    Box::new(GrepTool::new(worktree_root.clone())),
    Box::new(ListDirTool::new(worktree_root.clone())),
    Box::new(GlobTool::new(worktree_root.clone())),
];

// Write tools (used by Coder and Fixer)
let write_tools: Vec<Box<dyn Tool>> = vec![
    Box::new(ReadFileTool::new(worktree_root.clone())),
    Box::new(WriteFileTool::new(worktree_root.clone())),
    Box::new(EditFileTool::new(worktree_root.clone())),
    Box::new(RunCommandTool::new(worktree_root.clone())),
];

// Planner
let planner = PlanAgentBuilder::from(
    AgentBuilder::new()
        .name("planner")
        .role("You are a software architect. Analyze the codebase and create \
               a detailed implementation plan. Use read tools to understand \
               the existing code. Hand off to the coder when your plan is ready.")
        .tools({
            let mut tools = read_tools.clone();
            tools.push(Box::new(HandoffTool::new("coder")));
            tools
        })
        .max_turns(15)
        .temperature(0.4)
        .stream_sink(sink.clone())
        .signals(bus.clone())
)
.plan_config(PlanConfig {
    escalation_policy: EscalationPolicy::AutoEscalate,
    max_refinement_iterations: 2,
    inject_plan_message: true,
    enable_replan_tool: false,
})
.build()?;

// Coder
let coder = AgentBuilder::new()
    .name("coder")
    .role("You are an expert software engineer. Implement the plan provided \
           by the planner. Use write tools to make changes. Run tests to \
           verify your implementation. Hand off to QA when you are done.")
    .tools({
        let mut tools = write_tools.clone();
        tools.push(Box::new(HandoffTool::new("qa")));
        tools
    })
    .commit_policy(CommitPolicy::OnSuccess)
    .max_turns(25)
    .temperature(0.2)
    .stream_sink(sink.clone())
    .signals(bus.clone())
    .build()?;

// QA
let qa = AgentBuilder::new()
    .name("qa")
    .role("You are a quality assurance engineer. Review the coder's changes \
           for correctness, style, and security issues. Run tests. If you \
           find issues, use the request_fix tool. Otherwise, complete.")
    .tools({
        let mut tools = read_tools.clone();
        tools.push(Box::new(RunCommandTool::new(worktree_root.clone())));
        tools.push(Box::new(RequestFixTool::new()));
        tools
    })
    .commit_policy(CommitPolicy::Never)
    .max_turns(10)
    .temperature(0.2)
    .stream_sink(sink.clone())
    .signals(bus.clone())
    .build()?;

// Fixer
let fixer = AgentBuilder::new()
    .name("fixer")
    .role("You are a bug fix specialist. Apply the fixes requested by the QA \
           engineer. Use write tools to make changes. Run tests to verify. \
           Hand off to QA for re-verification when done.")
    .tools({
        let mut tools = write_tools.clone();
        tools.push(Box::new(HandoffTool::new("qa")));
        tools
    })
    .commit_policy(CommitPolicy::OnSuccess)
    .max_turns(20)
    .temperature(0.2)
    .cost_limit(1.5)
    .stream_sink(sink.clone())
    .signals(bus.clone())
    .build()?;
```

### Registering agents for a dynamic workflow

```rust
let mut registry = AgentRegistry::new();

registry.register(DynamicNamedAgent {
    name: "planner".into(),
    agent: Box::new(planner),
    default_handoff_target: Some("coder".into()),
});

registry.register(DynamicNamedAgent {
    name: "coder".into(),
    agent: Box::new(coder),
    default_handoff_target: Some("qa".into()),
});

registry.register(DynamicNamedAgent {
    name: "qa".into(),
    agent: Box::new(qa),
    default_handoff_target: None, // QA completes or uses RequestFixTool
});

registry.register(DynamicNamedAgent {
    name: "fixer".into(),
    agent: Box::new(fixer),
    default_handoff_target: Some("qa".into()),
});

// Construct orchestrator with the registry
let orchestrator = HandoffOrchestrator::with_registry(
    registry,
    WorkflowConfig::Dynamic { initial_agent: "planner".into() },
);
```

### Minimal agent for testing

```rust
let test_agent = AgentBuilder::new()
    .name("test_agent")
    .role("You are a test agent. Return 'done' immediately.")
    .tools(vec![])  // No tools needed
    .max_turns(1)
    .temperature(0.0)
    .build()?;

// No stream_sink or signals needed for unit tests
// The agent will run but emit no signals
```

---

## Implementation Guidance

When composing a new agent, follow this workflow:

1. **Define the role prompt first.** Write the role prompt before choosing any other parameters. The role prompt determines the agent's behavior; everything else is optimization. Test the role prompt by running the agent with default parameters and evaluating its output.

2. **Choose the minimal tool set.** Include only the tools the agent needs. Every additional tool increases token usage (because tool descriptions are included in every LLM call) and increases the risk of the LLM using the wrong tool. If in doubt, start with fewer tools and add more only if the agent needs them.

3. **Set temperature based on the role.** Precision tasks (coding, reviewing, fixing) benefit from low temperatures (0.1-0.3). Creative tasks (planning, brainstorming, exploring) benefit from higher temperatures (0.4-0.7). Never use temperatures above 1.0 for production agents.

4. **Set max_turns with a safety margin.** Estimate the number of tool calls the agent will need, then add 50%. This gives the agent room to recover from mistakes (e.g., reading the wrong file, making an incorrect edit) without hitting the limit.

5. **Set a cost limit for expensive agents.** If an agent could potentially make many LLM calls (e.g., a fixer stuck in a loop), set a cost limit to prevent runaway spending. A reasonable limit is $1-2 for standard tasks and $5-10 for complex tasks.

6. **Always set `stream_sink` and `signals` for production agents.** Without these, the TUI will show no activity and no signals will be emitted. The only exception is unit tests, where you may want to suppress signal emission to reduce noise.

7. **Test the composed agent in isolation** using `XaftRuntime::for_testing()` with a mock provider that returns responses appropriate for the agent's role. Verify that the agent uses the expected tools, produces the expected output, and terminates within the expected number of turns.

8. **Register the agent in the `AgentRegistry`** and verify that the orchestrator can look it up by name. Test that handoffs target the correct agent by tracing the handoff sequence in the session store.
