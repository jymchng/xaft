# Testing Strategy

This document describes xaft's testing strategy: the layering of tests from unit to integration, the test infrastructure (in-memory stores, stub runtimes), the conventions for writing and organizing tests, and the patterns that make xaft's async, multi-agent codebase testable without resorting to fragile mocking frameworks.

---

## Test Pyramid

xaft follows the test pyramid: many fast unit tests at the base, fewer integration tests in the middle, and very few end-to-end tests at the top. This distribution maximizes confidence per unit of test execution time — unit tests catch the vast majority of regressions in milliseconds, while integration and end-to-end tests validate the wiring between components but are slower and more expensive to maintain.

```
        ┌──────────┐
        │  E2E     │  1-3 tests per major workflow
        │  Tests   │  (~30s each)
        ├──────────┤
        │Integr-   │  5-10 tests per crate boundary
        │ation     │  (~5s each)
        ├──────────┤
        │  Unit    │  20-50 tests per module
        │  Tests   │  (<10ms each)
        └──────────┘
```

Each layer has a distinct purpose and a distinct set of conventions. Unit tests verify individual functions and methods in isolation. Integration tests verify that two or more crates interact correctly when composed. End-to-end tests verify that the full stack — from CLI invocation to task completion — produces the expected result.

---

## Unit Tests Per Module

Unit tests live in the same file as the code they test, inside a `#[cfg(test)] mod tests` block. This colocated style keeps tests close to the implementation, making it easy to see what is tested when reading the code and easy to update tests when refactoring. Every public method should have at least one unit test; private methods are tested indirectly through their public callers.

```rust
// xaft-tools/src/read_file.rs

pub async fn read_file(path: &str, range: Option<LineRange>) -> Result<String, ToolError> {
    let validated = validate_path(path)?;
    let content = tokio::fs::read_to_string(&validated).await
        .map_err(|e| ToolError::Internal(e.to_string()))?;

    match range {
        Some(r) => Ok(apply_range(&content, r)),
        None => Ok(content),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file_success() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        tokio::fs::write(&file_path, "hello world").await.unwrap();

        let result = read_file(
            file_path.to_str().unwrap(),
            None,
        ).await;

        assert_eq!(result.unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_read_file_with_line_range() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("multiline.txt");
        tokio::fs::write(&file_path, "line1\nline2\nline3\nline4\nline5").await.unwrap();

        let result = read_file(
            file_path.to_str().unwrap(),
            Some(LineRange { start: 2, end: 4 }),
        ).await;

        assert_eq!(result.unwrap(), "line2\nline3\nline4\n");
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let result = read_file("/nonexistent/path.txt", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_file_path_traversal() {
        let result = read_file("../../../etc/passwd", None).await;
        assert!(matches!(result, Err(ToolError::PermissionDenied { .. })));
    }
}
```

Unit tests in xaft follow several conventions that make them consistent and maintainable across the codebase. Every test name begins with `test_` and describes the specific scenario being tested. Tests are independent — they do not share state or depend on execution order. Each test creates its own fixtures (temporary files, mock inputs) rather than relying on shared test data directories. This independence allows tests to run in parallel without interference, and it makes test failures easy to diagnose because each test exercises exactly one code path.

The `tempfile::tempdir()` crate is used extensively for file system tests. It creates a temporary directory that is automatically cleaned up when the `TempDir` value is dropped, ensuring that tests never leave artifacts on the filesystem. This is critical in CI environments where disk space is limited and stale test data can cause subsequent test runs to fail.

---

## Integration Tests Per Crate

Integration tests live in the `tests/` directory at the crate root. Unlike unit tests, integration tests can only access the crate's public API, which ensures that the tests are verifying the crate's contract rather than its implementation details. Each integration test file corresponds to a major feature or workflow within the crate.

```rust
// xaft-runtime/tests/workflow_test.rs

use xaft_runtime::*;
use xaft_agent::*;
use xaft_tools::ToolRegistry;

#[tokio::test]
async fn test_single_agent_workflow_completes() {
    let registry = ToolRegistry::builder()
        .register_builtin_tools()
        .build();

    let agent = AgentBuilder::new()
        .name("test-agent")
        .role(Role::builder()
            .system_prompt("You are a test agent.")
            .max_iterations(5)
            .build())
        .tools(vec![
            registry.get("read_file").unwrap(),
        ])
        .commit_policy(CommitPolicy::Never)
        .stream_sink(Arc::new(CollectSink::new()))
        .build()
        .unwrap();

    let result = xaft_runtime::test_harness::run_agent(
        agent,
        "Say hello",
    ).await;

    assert!(result.is_ok());
    assert!(!result.unwrap().output().is_empty());
}
```

Integration tests for `xaft-runtime` use a `test_harness` module that provides simplified entry points for constructing and running the runtime without going through the full CLI. The `test_harness::run_agent` function creates a minimal runtime with a single agent, runs it to completion, and returns the result. This avoids the boilerplate of bootstrap, signal bus setup, and session store initialization that the production runtime requires, while still exercising the full agent execution path.

Integration tests for `xaft-agent` focus on the lifecycle hooks and the interaction between the agent and the LLM provider. These tests use a mock provider (see [Mocking Providers](./02-mocking-providers.md)) to control the LLM's responses, enabling deterministic testing of agent behavior without making actual API calls. The mock provider returns pre-scripted responses that exercise specific agent code paths — tool invocation, iteration limits, error handling, and handoff decisions.

Integration tests for `xaft-tools` exercise the full tool execution pipeline, from input validation through execution to output serialization. These tests create a temporary workspace, register tools against it, and invoke tools through the `Tool::execute()` method. They verify that the tool's output matches the expected format and that the tool handles edge cases (missing files, permission errors, cancellation) correctly.

---

## InMemoryWorkspaceStore

The `InMemoryWorkspaceStore` is the primary test double for the `WorkspaceStore` trait. It implements the full `WorkspaceStore` interface backed by an in-memory `HashMap`, providing fast, deterministic, and isolated workspace operations for tests. Because it implements the same trait as `FsWorkspaceStore`, it can be substituted seamlessly in any code that accepts `Arc<dyn WorkspaceStore>`.

```rust
use xaft_runtime::store::{WorkspaceStore, InMemoryWorkspaceStore};

fn create_test_workspace() -> Arc<dyn WorkspaceStore> {
    let store = InMemoryWorkspaceStore::new();

    // Pre-populate with test files
    store.write_file("src/main.rs", "fn main() {}").unwrap();
    store.write_file("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }").unwrap();
    store.write_file("Cargo.toml", "[package]\nname = \"test\"\nversion = \"0.1.0\"").unwrap();

    Arc::new(store)
}
```

The `InMemoryWorkspaceStore` supports all operations that `FsWorkspaceStore` supports: reading, writing, listing directories, checking existence, computing diffs, and committing changes. The `commit()` method records a snapshot of the current file state with an auto-generated commit hash, and the `diff()` method compares the current state against any previous commit. This allows tests to verify that agents make the expected file changes without requiring a real git repository.

The in-memory store is not a toy implementation — it tracks file metadata (creation time, modification time, size) and enforces the same path validation rules as the file system implementation. Path traversal attempts (e.g., `../../../etc/passwd`) are rejected with the same `StoreError::PathTraversal` error. This ensures that tests exercise the same validation logic that runs in production, catching security issues before they reach the real filesystem.

One important difference from `FsWorkspaceStore` is that `InMemoryWorkspaceStore` does not support concurrent access from multiple processes. The in-memory store is designed for single-process tests where only one test at a time accesses the store. For tests that need to simulate concurrent access, use the file system store with a temporary directory instead.

---

## StubRuntime

The `StubRuntime` is a minimal runtime implementation designed for testing components that depend on the runtime's infrastructure — the signal bus, the cancellation token, and the stream sink — without starting the full runtime. It provides pre-configured instances of these components, allowing tests to focus on the component under test rather than the runtime's bootstrap sequence.

```rust
use xaft_runtime::test_util::StubRuntime;

#[tokio::test]
async fn test_agent_emits_signal_on_completion() {
    let stub = StubRuntime::new();

    // The stub runtime provides a real signal bus and cancel token
    let signal_bus = stub.signal_bus();
    let cancel_token = stub.cancel_token();
    let sink = stub.collect_sink(); // CollectSink that accumulates events

    // Subscribe to the signal we expect
    let mut rx = signal_bus.subscribe::<XaftAgentOutput>();

    // Build an agent using the stub's infrastructure
    let agent = AgentBuilder::new()
        .name("test-agent")
        .role(Role::builder()
            .system_prompt("You are a test agent.")
            .max_iterations(3)
            .build())
        .stream_sink(sink)
        .signal_bus(signal_bus.clone())
        .build()
        .unwrap();

    // Run the agent with the stub's cancel token
    let _result = agent.turn(
        TurnInput::user("Say hello"),
        cancel_token,
    ).await;

    // Verify the signal was emitted
    let signal = rx.try_recv().expect("agent should emit XaftAgentOutput");
    assert_eq!(signal.agent_name(), "test-agent");
}
```

The `StubRuntime` is constructed with `StubRuntime::new()`, which creates a fresh signal bus, cancellation token, and collect sink. Each test gets its own stub, ensuring complete isolation between tests. The stub's components are real implementations — the signal bus actually dispatches events, the cancellation token actually propagates cancellation, and the collect sink actually records events. This means that tests using the `StubRuntime` exercise the real infrastructure code, not a simplified mock, which increases confidence that the tested behavior will work correctly in production.

The `StubRuntime` also provides convenience methods for common test operations. The `cancel_after(duration)` method schedules the cancellation token to be cancelled after the specified duration, which is useful for testing cancellation handling. The `advance_time(duration)` method (when the `test-util` feature is enabled) advances a simulated clock, which is useful for testing timeout behavior without actually waiting.

---

## Test Conventions

### Naming

Test functions follow the pattern `test_<unit>_<scenario>_<expected_result>`. For example, `test_agent_turn_cancellation_returns_cancelled_error`. This naming convention makes test failures immediately diagnosable from the test name alone — you know which unit failed, what scenario was being tested, and what the expected result was.

### Assertions

Prefer `assert_eq!` and `assert!(matches!(...))` over `assert!` with boolean expressions. The structured assertion macros produce better failure messages that show the actual and expected values, which reduces the time needed to diagnose test failures. For error assertions, use `matches!` to verify the error variant without checking every field:

```rust
// Good: Clear failure message
assert_eq!(result.unwrap().output(), "expected output");

// Good: Verifies the error variant without over-specifying
assert!(matches!(result, Err(AgentError::Cancelled)));

// Bad: Opaque failure message
assert!(result.is_err());
```

### Async Test Helpers

For tests that need to wait for an async condition to become true, use the `tokio::time::timeout` wrapper with a reasonable duration. This prevents tests from hanging indefinitely when a condition is never met:

```rust
#[tokio::test]
async fn test_event_emitted_within_timeout() {
    let mut rx = signal_bus.subscribe::<ModelCallComplete>();

    // Trigger the action that should emit the event
    agent.turn(input, cancel).await.unwrap();

    // Wait for the event with a timeout
    let event = tokio::time::timeout(
        Duration::from_secs(5),
        rx.recv(),
    ).await.expect("event should arrive within 5 seconds");

    assert_eq!(event.unwrap().model(), "claude-sonnet-4-20250514");
}
```

### Feature Flags for Test Utilities

Test utilities like `InMemoryWorkspaceStore`, `StubRuntime`, and `test_harness` are gated behind the `test-util` feature flag. This prevents them from being compiled into production binaries, where they would increase binary size and could accidentally be used in production code. The `test-util` feature is enabled in the `[dev-dependencies]` section of dependent crates:

```toml
# Cargo.toml
[dev-dependencies]
xaft-runtime = { path = "../xaft-runtime", features = ["test-util"] }
```

---

## Continuous Integration

All tests run on every pull request. The CI pipeline executes tests in three stages:

1. **Unit tests** (`cargo test --workspace`) — Runs all unit tests across all crates. This is the fastest stage and catches the majority of regressions. Target: < 60 seconds total.

2. **Integration tests** (`cargo test --test '*' --workspace`) — Runs all integration tests. These tests exercise crate boundaries and require more setup (temporary directories, mock providers). Target: < 300 seconds total.

3. **End-to-end tests** (`cargo test --test e2e`) — Runs a small number of full-stack tests that invoke the CLI, interact with a mock LLM, and verify the output. These tests are the slowest but provide the highest confidence. Target: < 600 seconds total.

Each stage only runs if the previous stage passed, which avoids wasting CI time on known-broken code. The test results are reported as GitHub check statuses, and a failing test blocks the pull request from being merged.
