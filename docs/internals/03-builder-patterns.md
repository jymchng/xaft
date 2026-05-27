# Builder Patterns

This document describes the builder patterns used throughout xaft, their design rationale, and the conventions that ensure consistency. The three primary builders are `AgentBuilder`, `PlanAgentBuilder`, and `ToolRegistryBuilder`, but the pattern is also used for configuration types like `Role::builder()` and `Runtime::builder()`.

---

## Design Rationale

xaft uses the consuming builder pattern — each method on the builder consumes `self` and returns a new builder with the updated state. This pattern was chosen over the more common mutable builder (`&mut self -> &mut Self`) for several reasons:

**Compile-time completeness checking.** The consuming builder can encode required fields in the type system. `AgentBuilder::build()` is only available after `role()` has been called, because the `build()` method is defined on a type state that includes the role. This means the compiler rejects programs that attempt to build an agent without a role, rather than failing at runtime with a "missing required field" error. Type-state builders trade some implementation complexity for absolute compile-time guarantees.

**No partial mutation.** Because each method consumes and returns a new builder, there is no way to hold a reference to a partially-built object and mutate it from two different code paths. This eliminates a class of bugs where one code path sets a field and another code path overwrites it, resulting in an inconsistent configuration.

**Fluent API ergonomics.** The consuming builder naturally produces a fluent API where each method call chains to the next. This reads like a declarative specification of the object's configuration, which is easier to understand than a sequence of mutable assignments. Compare:

```rust
// Consuming builder (fluent)
let agent = AgentBuilder::new()
    .name("coder")
    .role(role)
    .tools(tools)
    .commit_policy(CommitPolicy::OnSuccess)
    .build()?;

// Mutable builder (imperative)
let mut builder = AgentBuilder::new();
builder.name("coder");
builder.role(role);
builder.tools(tools);
builder.commit_policy(CommitPolicy::OnSuccess);
let agent = builder.build()?;
```

The fluent style is more compact and visually groups the configuration into a single expression. It also makes it easy to see all the configuration at a glance, which is important for readability in a codebase with many agent definitions.

---

## AgentBuilder

The `AgentBuilder` is the most commonly used builder in xaft. It constructs `Agent` instances with all their configuration: name, role, tools, commit policy, stream sink, and signal bus. The builder enforces that the role is specified before `build()` is called — all other fields have sensible defaults.

### Method Chaining Conventions

Every setter method on `AgentBuilder` follows these conventions:

1. **Named after the field.** The method `.name()` sets the `name` field, `.role()` sets the `role` field, etc. There are no abbreviated or aliased names.

2. **Takes ownership of the value.** Methods take the value by value, not by reference. This avoids lifetime annotations on the builder and allows the caller to move values into the builder without cloning.

3. **Returns a new builder.** The return type is `Self`, which in practice means `AgentBuilder` with the updated field. The old builder is consumed and cannot be reused.

4. **Optional fields have defaults.** Fields that are not required have default values that are applied in the `build()` method. The `commit_policy` defaults to `OnSuccess`, the `stream_sink` defaults to a no-op sink, and the `signal_bus` defaults to a local (non-shared) bus.

```rust
impl AgentBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            name: None,
            role: None,
            tools: Vec::new(),
            commit_policy: CommitPolicy::OnSuccess,
            stream_sink: Arc::new(NoOpSink),
            signal_bus: None,
        }
    }

    /// Set the agent's name. Names must be unique within an AgentRegistry.
    /// If not set, a generated name like "agent-0x7f3a" will be used.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the agent's role. This is the only required field.
    pub fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Set the agent's tools. Replaces any previously set tools.
    pub fn tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    /// Add a single tool to the agent's tool set.
    pub fn with_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set the commit policy.
    pub fn commit_policy(mut self, policy: CommitPolicy) -> Self {
        self.commit_policy = policy;
        self
    }

    /// Set the stream sink.
    pub fn stream_sink(mut self, sink: Arc<dyn StreamSink>) -> Self {
        self.stream_sink = sink;
        self
    }

    /// Set the signal bus.
    pub fn signal_bus(mut self, bus: Arc<SignalBus>) -> Self {
        self.signal_bus = Some(bus);
        self
    }

    /// Build the agent. Returns an error if required fields are missing.
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        let name = self.name.unwrap_or_else(|| format!("agent-{:x}", rand::random::<usize>()));
        let role = self.role.ok_or(AgentBuildError::MissingRole)?;
        
        Ok(Agent {
            name,
            role,
            tools: self.tools,
            commit_policy: self.commit_policy,
            stream_sink: self.stream_sink,
            signal_bus: self.signal_bus.unwrap_or_else(|| Arc::new(SignalBus::new())),
        })
    }
}
```

### Validation in build()

The `build()` method validates the builder's state before constructing the agent. Validation errors are returned as `AgentBuildError` variants, which are specific and actionable:

| Error | Cause | Fix |
|-------|-------|-----|
| `MissingRole` | `role()` was never called | Call `.role(role)` before `.build()` |
| `DuplicateTool` | Two tools with the same name were added | Remove the duplicate or rename one |
| `EmptyToolSet` | No tools were registered | Add tools or call `.allow_no_tools()` |

The `EmptyToolSet` error is interesting because it is a warning, not a hard error. An agent with no tools can still function — it just cannot take any actions. This might be intentional for a conversational agent that only provides advice. The `.allow_no_tools()` method suppresses the error:

```rust
let agent = AgentBuilder::new()
    .name("advisor")
    .role(advisor_role)
    .allow_no_tools()  // suppress EmptyToolSet error
    .build()?;
```

---

## PlanAgentBuilder

The `PlanAgentBuilder` extends `AgentBuilder` with planner-specific fields: the `AgentRegistry`, escalation policy, handoff rules, and initial plan. It wraps an `AgentBuilder` internally and delegates common methods (name, role, tools, commit_policy) to the inner builder.

```rust
impl PlanAgentBuilder {
    pub fn new() -> Self {
        Self {
            inner: AgentBuilder::new(),
            agent_registry: None,
            escalation_policy: EscalationPolicy::AskUser,
            handoff_rules: Vec::new(),
            initial_plan: None,
        }
    }

    /// Delegate to inner AgentBuilder
    pub fn name(self, name: impl Into<String>) -> Self {
        Self { inner: self.inner.name(name), ..self }
    }

    pub fn role(self, role: Role) -> Self {
        Self { inner: self.inner.role(role), ..self }
    }

    pub fn tools(self, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { inner: self.inner.tools(tools), ..self }
    }

    /// Planner-specific methods
    pub fn agent_registry(mut self, registry: AgentRegistry) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    pub fn escalation_policy(mut self, policy: EscalationPolicy) -> Self {
        self.escalation_policy = policy;
        self
    }

    pub fn handoff_rules(mut self, rules: Vec<HandoffRule>) -> Self {
        self.handoff_rules = rules;
        self
    }

    pub fn initial_plan(mut self, plan: Plan) -> Self {
        self.initial_plan = Some(plan);
        self
    }

    pub fn build(self) -> Result<Agent, AgentBuildError> {
        let registry = self.agent_registry
            .ok_or(AgentBuildError::MissingAgentRegistry)?;
        
        // Build the inner agent, then wrap it with planner capabilities
        let agent = self.inner.build()?;
        
        Ok(Agent::planner(agent, registry, self.escalation_policy, 
                          self.handoff_rules, self.initial_plan))
    }
}
```

The delegation pattern used here (`Self { inner: self.inner.name(name), ..self }`) is the idiomatic way to compose builders in Rust. It applies the method to the inner builder, then spreads the outer builder's fields back into the new struct. This is more efficient than it looks — the spread operator moves the unchanged fields, so there is no cloning or allocation.

---

## ToolRegistryBuilder

The `ToolRegistryBuilder` constructs a `ToolRegistry` — an immutable collection of `Arc<dyn Tool>` instances. Unlike `AgentBuilder`, the `ToolRegistryBuilder` has no required fields; an empty registry is valid (though not very useful).

```rust
impl ToolRegistryBuilder {
    /// Register all built-in tools: read_file, write_file, run_shell,
    /// search_files, list_directory, etc.
    pub fn register_builtin_tools(mut self) -> Self {
        for tool in xaft_tools::builtin::all_tools() {
            self.tools.insert(tool.name().to_string(), Arc::from(tool));
        }
        self
    }

    /// Register a custom tool. Returns an error if a tool with the
    /// same name is already registered.
    pub fn register(mut self, tool: Arc<dyn Tool>) -> Result<Self, RegistryBuildError> {
        if self.tools.contains_key(tool.name()) {
            return Err(RegistryBuildError::DuplicateName(tool.name().to_string()));
        }
        self.tools.insert(tool.name().to_string(), tool);
        Ok(self)
    }

    /// Register a tool, replacing any existing tool with the same name.
    /// Useful for overriding built-in tools with custom implementations.
    pub fn register_or_replace(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    /// Build the immutable registry.
    pub fn build(self) -> ToolRegistry {
        ToolRegistry {
            tools: Arc::new(self.tools),
        }
    }
}
```

The `ToolRegistry` is immutable after construction. Tools are stored in an `Arc<HashMap<String, Arc<dyn Tool>>>`, which is cheaply clonable and safe to share across threads. This immutability is critical because the tool registry is shared between the runtime (which reads it), the agent (which reads it), and the TUI (which reads it for the help display). If the registry were mutable, concurrent reads would require synchronization, which would add latency to every tool lookup.

The `register_or_replace()` method is a deliberate escape hatch that allows overriding built-in tools with custom implementations. For example, if you want to replace the built-in `run_shell` tool with a sandboxed version that restricts which commands can be executed, you can do so:

```rust
let registry = ToolRegistry::builder()
    .register_builtin_tools()
    .register_or_replace(Arc::new(SandboxedShellTool::new()))
    .build();
```

This pattern — register built-ins, then override specific ones — is more convenient than manually registering every built-in tool except the one you want to replace. It also ensures that your custom tool is compatible with the built-in tool's schema, because the LLM will use the same tool definition regardless of which implementation handles the call.

---

## Consumption Semantics

All xaft builders use consumption semantics: calling `build()` consumes the builder and returns the constructed object. This means the builder cannot be reused after `build()` is called. If you need to build multiple similar objects, use the builder's methods to create a base configuration, then clone the relevant parts before calling `build()`.

```rust
// Build multiple agents from a shared base configuration
let base_builder = AgentBuilder::new()
    .commit_policy(CommitPolicy::OnSuccess)
    .tools(shared_tools.clone())
    .stream_sink(shared_sink.clone())
    .signal_bus(shared_bus.clone());

// Each agent gets its own builder (consumption means we can't reuse base_builder)
let coder = AgentBuilder::new()
    .name("coder")
    .role(coder_role)
    .commit_policy(CommitPolicy::OnSuccess)
    .tools(shared_tools.clone())
    .stream_sink(shared_sink.clone())
    .signal_bus(shared_bus.clone())
    .build()?;

let reviewer = AgentBuilder::new()
    .name("reviewer")
    .role(reviewer_role)
    .commit_policy(CommitPolicy::Never)
    .tools(shared_tools.clone())
    .stream_sink(shared_sink.clone())
    .signal_bus(shared_bus.clone())
    .build()?;
```

The repetition is intentional — it makes each agent's configuration explicit and self-contained. A factory function can reduce the boilerplate:

```rust
fn build_agent(
    name: &str,
    role: Role,
    commit_policy: CommitPolicy,
    tools: Vec<Arc<dyn Tool>>,
    sink: Arc<dyn StreamSink>,
    bus: Arc<SignalBus>,
) -> Result<Agent, AgentBuildError> {
    AgentBuilder::new()
        .name(name)
        .role(role)
        .commit_policy(commit_policy)
        .tools(tools)
        .stream_sink(sink)
        .signal_bus(bus)
        .build()
}
```

---

## Type-State Pattern (Advanced)

For builders where certain fields are absolutely required and should be enforced at compile time, xaft uses the type-state pattern. This pattern encodes the builder's state in its type parameters, so the `build()` method is only available when all required fields have been set.

```rust
// Type-state markers
pub struct NoRole;
pub struct HasRole(Role);

pub struct AgentBuilder<R = NoRole> {
    name: Option<String>,
    role: R,
    tools: Vec<Arc<dyn Tool>>,
    // ... other fields
}

impl AgentBuilder<NoRole> {
    pub fn new() -> Self { /* ... */ }

    pub fn role(self, role: Role) -> AgentBuilder<HasRole> {
        AgentBuilder {
            name: self.name,
            role: HasRole(role),
            tools: self.tools,
        }
    }
}

// build() is only available when the role has been set
impl AgentBuilder<HasRole> {
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        let AgentBuilder { name, role: HasRole(role), tools, .. } = self;
        Ok(Agent { name, role, tools, /* ... */ })
    }
}
```

With this pattern, `AgentBuilder::new().build()` does not compile — the compiler sees that `build()` is not defined on `AgentBuilder<NoRole>`. Only after calling `.role()` does the builder's type change to `AgentBuilder<HasRole>`, which has the `build()` method. This moves the "missing required field" check from runtime to compile time, eliminating an entire category of bugs.

The type-state pattern is used sparingly in xaft because it adds complexity to the builder's implementation and its public API. It is reserved for builders where forgetting a required field would have severe consequences (like building a runtime without a provider, which would crash on the first LLM call). For most builders, runtime validation in `build()` is sufficient and produces clearer error messages than a compile-time type error.
