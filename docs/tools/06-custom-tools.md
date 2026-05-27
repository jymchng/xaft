# Building Custom Tools

The xaft tool system is designed for extension. While the built-in tools cover filesystem, shell, and git interactions, many workflows need domain-specific capabilities — querying a database, calling an internal API, interacting with a build system, or executing project-specific validation. This guide walks through the process of implementing the `Tool` trait, handling edge cases, and registering custom tools with the `ToolRegistry`.

---

## Implementing the `Tool` Trait

Every custom tool must implement `agtrs_runtime::tool::Tool`. Let's build a concrete example: a tool that queries an internal issue tracker.

### Step 1: Define the Struct

```rust
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};
use agtrs_runtime::error::AgtrsError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub struct IssueTrackerTool {
    client: reqwest::Client,
    base_url: String,
}
```

The struct holds any state the tool needs. In this case, an HTTP client and a base URL for the issue tracker API. Because the `Tool` trait requires `Send + Sync`, the struct's fields must also be `Send + Sync`. `reqwest::Client` satisfies this, as does `String`.

### Step 2: Define the Input Type

```rust
#[derive(Deserialize)]
pub struct IssueTrackerInput {
    pub issue_id: String,
    pub fields: Option<Vec<String>>,
}
```

The input type is a plain Rust struct that derives `Deserialize`. It does not need to implement `Serialize` — the tool receives JSON from the agent loop and never serializes back. The `fields` parameter is optional, allowing the LLM to request specific fields or get a default summary.

### Step 3: Implement the Trait

```rust
#[async_trait]
impl Tool for IssueTrackerTool {
    fn name(&self) -> &str {
        "issue_tracker"
    }

    fn description(&self) -> &str {
        "Queries the internal issue tracker for a specific issue. \
         Provide an issue_id to look up. Optionally specify fields \
         to limit the response to specific attributes (e.g., title, status, assignee)."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "issue_id": {
                    "type": "string",
                    "description": "The issue identifier (e.g., PROJ-1234)"
                },
                "fields": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of fields to include in the response"
                }
            },
            "required": ["issue_id"]
        })
    }

    fn requires_confirmation(&self) -> bool {
        false  // Read-only operation, no confirmation needed
    }

    async fn call(
        &self,
        input: Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, AgtrsError> {
        // 1. Check cancellation
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled);
        }

        // 2. Parse input
        let input: IssueTrackerInput = serde_json::from_value(input)
            .map_err(|e| AgtrsError::ToolInputValidation(e.to_string()))?;

        // 3. Execute the operation
        let url = format!("{}/issues/{}", self.base_url, input.issue_id);
        let response = self.client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await
                    .map_err(|e| AgtrsError::ToolExecution(e.to_string()))?;
                Ok(ToolResult::ok(body))
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                Ok(ToolResult::error(format!(
                    "Issue tracker returned {}: {}", status, body
                )))
            }
            Err(e) => Ok(ToolResult::error(format!(
                "Failed to reach issue tracker: {}", e
            ))),
        }
    }
}
```

### Key Implementation Notes

**Cancellation is mandatory.** Every custom tool must check `ctx.cancel_token.is_cancelled()` at the start of `call()` and at any natural yield point during long operations. A tool that ignores cancellation can block workflow shutdown indefinitely, which is unacceptable in production deployments.

**Use `ToolResult::error()` for expected failures.** Network timeouts, "not found" responses, and validation errors should be soft errors — the LLM might be able to fix them by adjusting its input. Reserve `AgtrsError` for truly exceptional conditions like corrupt internal state or missing dependencies.

**Schema must match the input struct.** The `schema()` method is the source of truth for what the agent loop will accept. If the schema says a field is required but the input struct makes it optional (or vice versa), you'll get confusing validation errors at runtime. Keep them in sync.

**`requires_confirmation()` should reflect risk.** Read-only tools should return `false`. Tools that modify external state (write to a database, send a message, deploy a service) should return `true`. When in doubt, default to `true` — the cost of an unnecessary confirmation prompt is far lower than the cost of an unintended side effect.

---

## Input Validation Strategies

The agent loop validates every tool invocation against `schema()` before `call()` is entered, but schema validation is coarse — it checks types and required fields, not semantic constraints. For deeper validation, add checks inside `call()`:

### Range Validation

```rust
if input.timeout_secs < 1 || input.timeout_secs > 300 {
    return Ok(ToolResult::error(
        "timeout_secs must be between 1 and 300"
    ));
}
```

### Format Validation

```rust
if !input.issue_id.matches(|c: char| c.is_ascii_alphanumeric() || c == '-').all(|_| true) {
    return Ok(ToolResult::error(
        "issue_id must contain only alphanumeric characters and hyphens"
    ));
}
```

### Path Validation (for file-adjacent tools)

Always use `validate_path()` if your tool touches the filesystem, even indirectly:

```rust
let validated = validate_path(&input.path, &self.workspace_root)
    .map_err(|_| AgtrsError::PathTraversal)?;
```

---

## Registration

Custom tools are registered with the `ToolRegistry` using the `register()` method:

### Direct Registration

```rust
let mut registry = ToolRegistry::new();

registry.register(IssueTrackerTool {
    client: reqwest::Client::new(),
    base_url: "https://issues.example.com/api".to_string(),
});

registry.register(DeployTool {
    cluster_url: "https://k8s.example.com".to_string(),
});
```

### Registration with the Builder

For tools that should be included in every workflow, extend the builder pattern:

```rust
pub struct MyToolRegistryBuilder {
    inner: ToolRegistryBuilder,
    issue_tracker_url: Option<String>,
}

impl MyToolRegistryBuilder {
    pub fn issue_tracker_url(mut self, url: String) -> Self {
        self.issue_tracker_url = Some(url);
        self
    }

    pub fn build_coder_with_extras(self) -> ToolRegistry {
        let mut registry = self.inner.build_coder();

        if let Some(url) = self.issue_tracker_url {
            registry.register(IssueTrackerTool {
                client: reqwest::Client::new(),
                base_url: url,
            });
        }

        registry
    }
}
```

This pattern keeps the builder ergonomic while allowing custom tools to be conditionally included based on configuration.

### Shared Instances with `add()`

When the same tool instance must be shared across multiple registries (e.g., a tool that maintains a connection pool or cache), use `add()` with a pre-erased `Arc`:

```rust
let tracker = Arc::new(ErasedTool::from_tool(IssueTrackerTool {
    client: reqwest::Client::new(),
    base_url: "https://issues.example.com/api".to_string(),
}));

let mut coder_registry = builder.build_coder();
coder_registry.add(tracker.clone());

let mut reader_registry = builder.build_reader();
reader_registry.add(tracker.clone());
```

Because `ErasedTool` is behind an `Arc`, cloning the `Arc` is cheap (just an atomic increment), and all registries share the same underlying tool instance.

---

## Testing Custom Tools

Testing a tool is straightforward because `ToolContext` and `ToolResult` are simple value types:

```rust
#[tokio::test]
async fn test_issue_tracker_success() {
    let mut server = mockito::Server::new_async().await;
    let mock = server.mock("GET", "/issues/PROJ-1234")
        .with_status(200)
        .with_body(r#"{"title": "Fix login bug", "status": "open"}"#)
        .create_async()
        .await;

    let tool = IssueTrackerTool {
        client: reqwest::Client::new(),
        base_url: server.url(),
    };

    let ctx = ToolContext {
        tool_use_id: "test-1".to_string(),
        cancel_token: CancellationToken::new(),
    };

    let input = json!({"issue_id": "PROJ-1234"});
    let result = tool.call(input, ctx).await.unwrap();

    assert!(!result.is_error);
    assert!(result.content.contains("Fix login bug"));
    mock.assert_async().await;
}

#[tokio::test]
async fn test_issue_tracker_cancellation() {
    let tool = IssueTrackerTool {
        client: reqwest::Client::new(),
        base_url: "https://unused.example.com".to_string(),
    };

    let cancel_token = CancellationToken::new();
    cancel_token.cancel();

    let ctx = ToolContext {
        tool_use_id: "test-2".to_string(),
        cancel_token,
    };

    let input = json!({"issue_id": "PROJ-1234"});
    let result = tool.call(input, ctx).await;

    assert!(matches!(result, Err(AgtrsError::Cancelled)));
}
```

The cancellation test is particularly important — it verifies that your tool respects the cooperative cancellation contract, which is essential for workflow reliability.

---

## Advanced Patterns

### Stateful Tools

Tools can maintain mutable state behind interior mutability:

```rust
pub struct CacheTool {
    cache: Arc<Mutex<HashMap<String, String>>>,
}

#[async_trait]
impl Tool for CacheTool {
    // ...
    async fn call(&self, input: Value, ctx: ToolContext) -> Result<ToolResult, AgtrsError> {
        let parsed: CacheInput = serde_json::from_value(input)?;
        let mut cache = self.cache.lock().await;
        // ...
    }
}
```

Be careful with lock ordering — if two tools share a `Mutex`, a deadlock can occur if they acquire locks in different orders. Prefer lock-free patterns (e.g., `DashMap`) or design your tools so that each one owns its state exclusively.

### Tool Composition

A tool can delegate to other tools by accepting an `Arc<ToolRegistry>` in its constructor:

```rust
pub struct SmartSearchTool {
    registry: Arc<ToolRegistry>,
}

#[async_trait]
impl Tool for SmartSearchTool {
    async fn call(&self, input: Value, ctx: ToolContext) -> Result<ToolResult, AgtrsError> {
        // First grep, then read the matching files
        let grep_result = self.registry.get("grep").unwrap()
            .call(json!({"pattern": "..."}), ctx.clone()).await?;
        // Parse grep output, then read relevant files...
    }
}
```

This pattern enables meta-tools that orchestrate other tools, providing higher-level abstractions for common workflows. The `HandoffTool` and `RequestFixTool` in the workflow system are examples of this pattern.
