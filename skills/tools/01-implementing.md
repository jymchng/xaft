# Implementing Tools: The Tool Trait and Registration

## Purpose

Tools are the primary mechanism by which an xaft agent interacts with the outside world. Every capability—from reading files to executing shell commands—is exposed as a tool implementing the `Tool` trait. This document describes how to implement a tool from scratch, handle type erasure for dynamic dispatch, validate inputs, respect cancellation tokens, return structured results, and register tools with the agent runtime. Understanding this pipeline is essential for anyone extending xaft with new capabilities or debugging existing tool behavior.

The tool abstraction serves as the contract boundary between the LLM's decision-making layer and the side-effect-producing execution layer. By keeping tools as discrete, typed, well-described units, xaft ensures that the model can reason about available actions, that safety checks operate uniformly, and that test authors can mock or replace any tool without touching the agent loop.

## Mental Model

Think of a tool as a **typed RPC endpoint**. The LLM selects a tool by name, provides a JSON object matching the tool's schema, and receives a `ToolResult` back. The `Tool` trait defines the endpoint's contract:

```
Tool trait
├── name() -> &str              // unique identifier the model uses to call
├── description() -> &str       // natural-language hint for the model
├── schema() -> Value           // JSON Schema for the input object
├── requires_confirmation() -> bool  // does this tool need human approval?
└── call(input: Value, ctx: &ToolContext) -> ToolResult  // execute
```

The `ToolContext` bundles the cancellation token, workspace reference, and any ambient state the tool needs without pulling it from global scope. The `ToolResult` enum is either `ToolResult::Ok(Value)` for successful execution or `ToolResult::Error(String)` for any failure—whether input validation, runtime error, or cancellation.

Behind the scenes, the `ToolRegistry` stores tools as `ErasedTool` objects. `ErasedTool` is a type-erased wrapper (`Box<dyn ErasedToolSend>`) that hides the generic parameter of `Tool<I>` so the registry can hold heterogeneous tools in a single `HashMap<String, ErasedTool>`. This is a standard Rust pattern: the typed `Tool<I>` trait is ergonomic for implementers; the erased `ErasedTool` trait object is ergonomic for the runtime.

```
Tool<I> ──(type erasure)──> ErasedTool ──(stored in)──> ToolRegistry
                                                          │
                                                     .get(name)
                                                          │
                                                          v
                                                    ErasedTool.call_erased(input, ctx)
```

## Extension Patterns

### Implementing a New Tool

1. Define your input struct and derive or manually implement `Deserialize`:
   ```rust
   #[derive(Deserialize)]
   struct ReadFileInput {
       path: String,
   }
   ```

2. Implement `Tool<ReadFileInput>` for your struct:
   ```rust
   struct ReadFileTool;

   #[async_trait]
   impl Tool<ReadFileInput> for ReadFileTool {
       fn name(&self) -> &str { "read_file" }
       fn description(&self) -> &str { "Read the contents of a file at the given path." }
       fn schema(&self) -> Value { json_schema::<ReadFileInput>() }
       fn requires_confirmation(&self) -> bool { false }

       async fn call(&self, input: ReadFileInput, ctx: &ToolContext) -> ToolResult {
           // Validate the path before use
           let safe_path = validate_path(&input.path, ctx.workspace.root())?;
           // Check cancellation before expensive work
           if ctx.cancellation_token.is_cancelled() {
               return ToolResult::Error("cancelled".into());
           }
           let content = tokio::fs::read_to_string(&safe_path).await
               .map_err(|e| ToolResult::Error(e.to_string()))?;
           ToolResult::Ok(json!({ "content": content }))
       }
   }
   ```

3. Register via `ToolRegistryBuilder`:
   ```rust
   let registry = ToolRegistryBuilder::new()
       .add(ReadFileTool)
       .add(WriteFileTool)
       .build();
   ```

   Or add dynamically to an existing registry:
   ```rust
   registry.add(ReadFileTool)?;
   ```

### Input Validation Helpers

xaft provides composable helpers that return `Result<_, ToolResult::Error>` so they chain naturally inside `call()`:

- **`require_str(map, key)`**: Extracts a required string field. Returns error if missing or not a string.
- **`opt_str(map, key)`**: Extracts an optional string field. Returns `None` if absent.
- **`validate_path(user_path, root)`**: Resolves the user-supplied path against the workspace root and rejects path-traversal attempts (`../` escapes). Returns the canonical safe path.

These helpers keep validation consistent across tools and avoid the common anti-pattern of raw string manipulation.

## Common Pitfalls

1. **Forgetting `requires_confirmation` on destructive tools.** Any tool that modifies files, executes shell commands, or makes network requests should set `requires_confirmation() -> true`. Missing this flag means the tool bypasses the human approval gate, which is a safety violation.

2. **Ignoring the cancellation token.** Long-running tools (e.g., shell execution) must check `ctx.cancellation_token.is_cancelled()` at natural checkpoints. Without this, a user's Ctrl+C won't take effect until the tool finishes naturally.

3. **Returning `ToolResult::Error` from validation but not from I/O.** Be consistent: both validation failures and runtime I/O errors should produce `ToolResult::Error`. Mixing `Err(...)` returns with `ToolResult::Error` confuses the agent loop.

4. **Stale schemas.** If you change the input struct but forget to update `schema()`, the model will send malformed JSON that your `Deserialize` impl rejects. Always derive schema from the same type, ideally via `json_schema::<I>()`.

5. **Duplicate tool names.** The registry uses the tool name as a unique key. Registering two tools with the same name silently overwrites the first. Use distinct, descriptive names.

6. **Capturing environment in tool structs.** Tool structs should be stateless or hold only configuration. Avoid capturing `Arc<Mutex<...>>` references that create hidden coupling between tools.

## Invariants

- **Tool names are globally unique within a registry.** Registering a duplicate name panics in debug builds and overwrites in release builds.
- **`call()` must not panic.** All errors must be returned as `ToolResult::Error`. A panic inside `call()` tears down the agent loop.
- **`schema()` must match the deserialized input type exactly.** The JSON Schema is the contract with the model; a mismatch causes cryptic deserialization errors at runtime.
- **`requires_confirmation` is a static property.** It cannot vary per invocation. If a tool sometimes needs confirmation and sometimes doesn't, split it into two tools.
- **`ErasedTool` preserves send+sized bounds.** All tools must be `Send + 'static` so they can be stored across await points and moved between tasks.

## Examples

### Minimal Read-Only Tool

```rust
struct ListFilesTool;

#[derive(Deserialize)]
struct ListFilesInput {
    directory: String,
}

#[async_trait]
impl Tool<ListFilesInput> for ListFilesTool {
    fn name(&self) -> &str { "list_files" }
    fn description(&self) -> &str {
        "List files and directories at the given path. Returns names only."
    }
    fn schema(&self) -> Value { json_schema::<ListFilesInput>() }
    fn requires_confirmation(&self) -> bool { false }

    async fn call(&self, input: ListFilesInput, ctx: &ToolContext) -> ToolResult {
        let safe_dir = validate_path(&input.directory, ctx.workspace.root())?;
        let mut entries = vec![];
        let mut rd = tokio::fs::read_dir(&safe_dir).await
            .map_err(|e| ToolResult::Error(e.to_string()))?;
        while let Some(entry) = rd.next_entry().await
            .map_err(|e| ToolResult::Error(e.to_string()))?
        {
            entries.push(entry.file_name().to_string_lossy().into_owned());
        }
        ToolResult::Ok(json!({ "entries": entries }))
    }
}
```

### Testing with InMemoryWorkspaceStore

```rust
#[tokio::test]
async fn test_list_files() {
    let store = InMemoryWorkspaceStore::new();
    store.write_file("dir/a.txt", "hello").await;

    let ctx = ToolContext {
        workspace: store.workspace(),
        cancellation_token: CancellationToken::new(),
    };

    let tool = ListFilesTool;
    let result = tool.call(
        ListFilesInput { directory: "dir".into() },
        &ctx,
    ).await;

    match result {
        ToolResult::Ok(val) => {
            let entries = val["entries"].as_array().unwrap();
            assert!(entries.iter().any(|e| e == "a.txt"));
        }
        ToolResult::Error(msg) => panic!("unexpected error: {msg}"),
    }
}
```

The `InMemoryWorkspaceStore` avoids touching the real filesystem, making tests fast, deterministic, and safe to run in parallel. It implements the same `Workspace` trait that production code uses, so your tool cannot distinguish it from a real workspace.
