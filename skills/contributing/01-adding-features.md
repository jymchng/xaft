# Adding Features Safely

## Purpose

Every feature added to xaft must be integrated correctly across multiple layers: the core implementation, the registration system, the configuration layer, and the test suite. A feature that works in isolation but is not registered, configured, or tested is a feature that will break in production. This document provides a checklist-driven guide for adding each type of feature (tools, agents, providers, planners, workflows, events) so that every new capability is complete, consistent, and safe. Following this guide ensures that new features don't violate safety invariants, don't break existing functionality, and are testable without real API calls.

## Mental Model

Think of adding a feature as assembling a machine with five parts: the core (the actual logic), the interface (how other parts talk to it), the registration (how the runtime discovers it), the configuration (how the user controls it), and the tests (how you verify it). Missing any part means the machine either doesn't work (missing registration), can't be controlled (missing configuration), or can't be verified (missing tests). The order matters: implement the core first, then the interface, then register it, then add configuration, then write tests. Implementing tests last ensures you're testing the feature as it will be used, not as you imagined it during development.

## Extension Patterns

### Adding a Tool

1. **Implement the `Tool` trait** in the appropriate crate (e.g., `xaft-file-tools`, `xaft-git-ops`). The trait requires `name()`, `description()`, `parameters()` (JSON schema), and `execute()`.
2. **Add to the `ToolRegistryBuilder`** via a new flag method (e.g., `.with_search_tool()`). This controls which tools are available in a given runtime configuration.
3. **Test with `InMemoryWorkspaceStore`** for file tools or `tempfile::TempDir` for filesystem tools. Test both success and error paths, including path traversal protection for file tools.

### Adding an Agent

1. **Use `AgentBuilder`** to construct the agent with a name, model, and tool list.
2. **Define the role and system prompt** in the agent's constructor or via a `prompt_fn` that generates context-specific prompts.
3. **Assign tools** via the builder's `.tool()` method. Only assign tools the agent actually needs—over-provisioning leads to confusion and cost waste.
4. **Test the lifecycle**: create → step → (handoff or done). Use a mock provider via `for_testing()` that returns canned responses.

### Adding a Provider

1. **Implement `LlmProvider`** with `complete`, `complete_stream`, and `model_info`.
2. **Add variant to `ProviderType`** and handle in `ProviderFactory::build()`.
3. **Add config section** in `ProviderConfig`.
4. **Test with `for_testing()`** for integration tests and with the provider's sandbox API key for live tests.

### Adding a Planner

1. **Configure escalation rules** that determine when the planner hands off to a specialist agent.
2. **Test plan quality** by verifying that the planner produces the expected agent sequence for representative inputs.

### Adding a Workflow

1. **Register agents** in the workflow's agent registry.
2. **Configure `WorkflowConfig`** with the workflow name, agent sequence, and handoff rules.
3. **Test handoff** by verifying that the orchestrator transfers context correctly between agents.

### Adding an Event

1. **Define a signal struct** named `Xaft<EventName>` (e.g., `XaftCostUpdated`).
2. **Emit the signal** from the appropriate component via `SignalBus::emit()`.
3. **Subscribe to the signal** in the TUI or other consumers via `SignalBus::subscribe::<XaftCostUpdated>()`.
4. **Bridge to TUI** by adding a handler in the TUI's event loop that renders the signal data.

## Common Pitfalls

- **Adding a tool but not registering it**: A tool that implements `Tool` but isn't added to `ToolRegistryBuilder` will never be available to agents. The LLM won't know it exists, and the agent can't call it.
- **Assigning too many tools to an agent**: Each tool in the agent's tool list adds tokens to the system prompt and increases the LLM's decision space. Assign only the tools the agent needs for its specific role.
- **Testing with real API calls**: Integration tests that hit real LLM APIs are slow, expensive, and flaky. Always use `for_testing()` or mock providers.
- **Adding configuration without defaults**: A new config key without a default breaks existing config files. Always provide a sensible default in the `Default` implementation.
- **Forgetting the signal bridge**: A new event that is emitted but not bridged to the TUI will be invisible to the user. The event is logged, but the user won't see real-time feedback.

## Invariants

1. Every new tool must implement `Tool`, be added to `ToolRegistryBuilder`, and have unit tests and integration tests.
2. Every new agent must use `AgentBuilder`, define a role/prompt, be assigned a minimal tool set, and have lifecycle tests.
3. Every new provider must implement `LlmProvider`, add a `ProviderType` variant, handle in `ProviderFactory`, add config, and have `for_testing()`.
4. Every new planner must configure escalation rules and test plan quality.
5. Every new workflow must register agents, configure `WorkflowConfig`, and test handoff.
6. Every new event must define a `Xaft<EventName>` signal, emit it, subscribe to it, and bridge it to the TUI.
7. No feature may violate the safety invariants (path traversal, git isolation, cost accuracy, approval gates, config null semantics).

## Examples

```rust
// Adding a new tool: SearchTool
// Step 1: Implement Tool trait
pub struct SearchTool {
    workspace_root: PathBuf,
}

impl Tool for SearchTool {
    fn name(&self) -> &str { "search_files" }
    fn description(&self) -> &str { "Search for patterns in workspace files" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Relative path within workspace" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, params: &Value) -> ToolResult {
        let pattern = params["pattern"].as_str().ok_or(/* ... */)?;
        let path = params["path"].as_str().unwrap_or(".");
        let resolved = self.validate_path(Path::new(path))?; // Path traversal protection!
        let matches = self.search(resolved, pattern)?;
        ToolResult { output: serde_json::to_string(&matches)?, is_error: false }
    }
}

// Step 2: Add to ToolRegistryBuilder
impl<S: WorkspaceStore> ToolRegistryBuilder<S> {
    pub fn with_search_tool(&mut self) -> &mut Self {
        self.with_search = true;
        self
    }

    pub fn build(self) -> ToolRegistry<S> {
        let mut registry = ToolRegistry::new(self.workspace_root, self.store);
        if self.with_search {
            registry.register(SearchTool::new(self.workspace_root.clone()));
        }
        // ... other tools ...
        registry
    }
}

// Step 3: Test with InMemoryWorkspaceStore
#[tokio::test]
async fn search_tool_finds_pattern() {
    let store = InMemoryWorkspaceStore::new();
    store.write_file("src/main.rs", "fn main() {}").await;
    let tool = SearchTool::new(store.workspace_root());
    let result = tool.execute(&serde_json::json!({"pattern": "fn main"})).await;
    assert!(!result.is_error);
    assert!(result.output.contains("main.rs"));
}

#[tokio::test]
async fn search_tool_blocks_path_traversal() {
    let store = InMemoryWorkspaceStore::new();
    let tool = SearchTool::new(store.workspace_root());
    let result = tool.execute(&serde_json::json!({
        "pattern": "secret",
        "path": "../../../etc"
    })).await;
    assert!(result.is_error); // Path traversal blocked
}

// Adding a new event: XaftSearchCompleted
// Step 1: Define signal
pub struct XaftSearchCompleted {
    pub pattern: String,
    pub matches: usize,
    pub duration_ms: u64,
}

// Step 2: Emit from SearchTool
impl SearchTool {
    async fn execute_with_signal(&self, params: &Value, bus: &SignalBus) -> ToolResult {
        let start = Instant::now();
        let result = self.execute(params).await;
        let _ = bus.emit(XaftSearchCompleted {
            pattern: params["pattern"].as_str().unwrap_or("").to_string(),
            matches: 0, // parse from result
            duration_ms: start.elapsed().as_millis() as u64,
        });
        result
    }
}

// Step 3: Bridge to TUI
// In TUI event loop:
let mut search_events = bus.subscribe::<XaftSearchCompleted>();
tokio::select! { biased;
    _ = cancel.cancelled() => break,
    event = search_events.recv() => {
        if let Ok(event) = event {
            tui.render_search_summary(&event);
        }
    }
}
```
