# Builder Pattern

## Purpose

The builder pattern in xaft solves a specific problem: constructing complex objects with many optional parameters without sacrificing type safety or readability. Agents, tool registries, and provider chains each have one or two required parameters (name, workspace root) and dozens of optional ones (model, temperature, tool list, retry config). Using a struct with `Option<T>` for every field is verbose and error-prone—callers must remember which fields are required. Using a function with positional parameters is worse—the parameter list becomes unreadable. The builder pattern lets callers construct objects with fluent method chains, validates all constraints at `build()` time, and consumes the builder so it can't be reused accidentally. This pattern is mandatory for any struct with more than three optional fields.

## Mental Model

Think of a builder as a configurator that collects settings and then stamps out a finished product. The builder is mutable during configuration (fluent methods return `&mut Self`), but immutable after construction (the built object has no setter methods). The `build()` method is the gatekeeper: it checks that all required fields are set, all constraints are satisfied (no conflicting options), and then constructs the target object, consuming the builder in the process. This "consume on build" convention prevents a common bug where someone builds an object, then accidentally mutates the builder and builds a second object with stale state. The builder itself should never validate in setters—validation at setter time makes it impossible to set fields in an order that temporarily violates a constraint (e.g., setting `max_retries` before `retry_enabled`).

## Extension Patterns

When adding a new optional parameter to an existing builder, add a fluent method that takes the parameter, sets the field, and returns `&mut Self`. Give the field a sensible default in the builder's constructor so existing callers don't break. When adding a new builder for a new struct, follow the `AgentBuilder` pattern: a `new()` constructor that takes required parameters, fluent methods for optional parameters, and a `build()` method that validates and consumes. When adding validation logic, put it in `build()`, not in the setter methods. If two options conflict (e.g., both `streaming` and `batch_mode` are enabled), `build()` should return an error or panic with a clear message. When a builder produces a `Result`, use `try_build()` instead of `build()` to signal that construction can fail.

## Common Pitfalls

- **Returning `Self` instead of `&mut Self` from fluent methods**: Returning `Self` (by value) requires the builder to be mutable and moved on every call, which breaks method chaining unless every intermediate result is bound. Always return `&mut Self` for fluent setters.
- **Validating in setters**: If `set_port(0)` panics in the setter, you can't construct a builder with a placeholder port and fix it later. Always defer validation to `build()`.
- **Not consuming the builder on `build()`**: If `build(&self)` returns a reference instead of `build(self)` consuming the builder, callers can build multiple objects from the same builder, leading to aliasing bugs. Always use `build(self)` to consume the builder.
- **Missing defaults for optional fields**: If a builder field has no default, every caller must set it, which defeats the purpose of the builder. Always provide sensible defaults (empty vec, zero, None) for optional fields in the builder's constructor.
- **Builder that doesn't enforce required fields**: A builder where `name` is optional but the built object requires it means `build()` must check for `None` and return an error. Instead, make `name` a required parameter of `new()` so the type system enforces it at compile time.

## Invariants

1. Builders must be consumed on `build()`. The method signature must be `fn build(self) -> T` or `fn try_build(self) -> Result<T, E>`.
2. Fluent setter methods must return `&mut Self` to enable method chaining.
3. Validation must happen in `build()`, not in setter methods. Setters must be pure state assignment.
4. Required parameters must be arguments to the builder's `new()` constructor, not optional setters.
5. Optional parameters must have sensible defaults set in `new()`.
6. If construction can fail due to conflicting options, use `try_build() -> Result<T, E>` instead of `build() -> T`.

## Examples

```rust
// AgentBuilder: name required, everything else optional with defaults
pub struct AgentBuilder {
    name: String,           // required
    model: String,          // default: "claude-sonnet-4-20250514"
    tools: Vec<ToolKind>,   // default: empty
    temperature: f64,       // default: 0.0
    max_tokens: Option<u32>,// default: None (use model default)
    system_prompt: Option<String>, // default: None
}

impl AgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: "claude-sonnet-4-20250514".to_string(),
            tools: Vec::new(),
            temperature: 0.0,
            max_tokens: None,
            system_prompt: None,
        }
    }

    pub fn model(&mut self, model: impl Into<String>) -> &mut Self {
        self.model = model.into();
        self
    }

    pub fn tool(&mut self, tool: ToolKind) -> &mut Self {
        self.tools.push(tool);
        self
    }

    pub fn temperature(&mut self, temp: f64) -> &mut Self {
        self.temperature = temp;
        self
    }

    pub fn build(self) -> Agent {
        Agent {
            name: self.name,
            model: self.model,
            tools: self.tools,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            system_prompt: self.system_prompt,
        }
    }
}

// Usage: fluent construction
let agent = AgentBuilder::new("editor")
    .model("claude-sonnet-4-20250514")
    .tool(ToolKind::WriteFile)
    .tool(ToolKind::BashExec)
    .temperature(0.1)
    .build();

// ToolRegistryBuilder: workspace_root required, flags control assembly
pub struct ToolRegistryBuilder<S: WorkspaceStore> {
    workspace_root: PathBuf,   // required
    store: S,                  // required (generic)
    with_file_tools: bool,     // default: false
    with_bash_tool: bool,      // default: false
    with_git_tools: bool,      // default: false
}

impl<S: WorkspaceStore> ToolRegistryBuilder<S> {
    pub fn new(workspace_root: PathBuf, store: S) -> Self {
        Self {
            workspace_root,
            store,
            with_file_tools: false,
            with_bash_tool: false,
            with_git_tools: false,
        }
    }

    pub fn with_file_tools(&mut self) -> &mut Self {
        self.with_file_tools = true;
        self
    }

    pub fn with_bash_tool(&mut self) -> &mut Self {
        self.with_bash_tool = true;
        self
    }

    pub fn build(self) -> ToolRegistry<S> {
        let mut registry = ToolRegistry::new(self.workspace_root, self.store);
        if self.with_file_tools {
            registry.register(ReadFileTool::new());
            registry.register(WriteFileTool::new());
        }
        if self.with_bash_tool {
            registry.register(BashExecTool::new());
        }
        registry
    }
}
```
