# Testing Conventions

## Purpose

Testing in xaft must be fast, deterministic, and isolated. The runtime touches the filesystem (git worktrees, session SQLite files), the network (LLM provider APIs), and the terminal (TUI rendering)—all of which are expensive or non-deterministic. Without strict testing conventions, tests become flaky (network timeouts), order-dependent (shared filesystem state), or useless (testing mocks instead of real logic). This document specifies the testing patterns that ensure every test runs in isolation, completes quickly, and tests real behavior via in-memory substitutes that faithfully replicate the production interface.

## Mental Model

Think of testing as a stack of fakes, one per layer. At the bottom, unit tests within each file test pure logic (parsing, serialization, validation) with no infrastructure. In the middle, integration tests in `tests/` test component interactions using in-memory stores (`InMemoryWorkspaceStore`, `InMemorySessionStore`) and stub runtimes (`StubRuntime`). At the top, end-to-end tests exercise the full stack with a mock LLM provider that returns canned responses. Each layer replaces only the external dependency (filesystem, network, terminal) with a fake that has the same interface as the real thing. This means `InMemoryWorkspaceStore` implements the same `WorkspaceStore` trait as `FilesystemWorkspaceStore`, so the test exercises the real tool logic against a fake store.

## Extension Patterns

When adding a new tool, write unit tests in `#[cfg(test)] mod tests` at the bottom of the tool's source file. Test parsing, validation, and edge cases (missing files, invalid paths) using `tempfile::TempDir` for any filesystem needs. When adding a new store or service, create an in-memory implementation of its trait (e.g., `InMemorySessionStore` implements `SessionStore`) and use it in integration tests. When adding a new CLI command, use `StubRuntime` (which implements `RuntimeDispatch` with no-ops or canned responses) to test argument parsing and command routing without starting the real runtime. When adding a new agent, create a mock provider via `for_testing()` that returns predetermined LLM responses, then verify the agent's tool calls and handoff decisions. When adding a new provider, test it against the provider test suite (streaming, error recovery, token counting) using the provider's sandbox API key.

## Common Pitfalls

- **Tests that depend on execution order**: Two tests that write to the same temporary directory will interfere. Always create a fresh `tempfile::TempDir` per test and let it clean up on drop.
- **Testing with real network calls**: Tests that hit real LLM APIs are slow, expensive, and flaky (rate limits, network failures). Always use mock providers or `for_testing()` helpers in automated tests.
- **Testing mocks instead of logic**: A test that verifies the mock was called with the right arguments is testing the mock, not the system under test. Instead, test the observable behavior: given this input, does the tool produce this output? Does the agent make this decision?
- **Skipping `#[instrument]` in tests**: If your test exercises code that uses `tracing`, initialize the test subscriber with `tracing_subscriber::fmt().with_test_writer().init()` so spans are captured. Without this, `#[instrument]` calls silently do nothing in tests.
- **Using `async fn` in tests without a runtime**: Async test functions need `#[tokio::test]`, not `#[test]`. Using `#[test]` on an async function compiles but never actually awaits anything, leading to silently passing tests that exercise nothing.

## Invariants

1. Unit tests must live in `#[cfg(test)] mod tests` within the source file they test. Integration tests must live in the `tests/` directory of the crate.
2. Every test must be independent: no shared mutable state between tests, no assumptions about execution order.
3. Filesystem tests must use `tempfile::TempDir` for isolated temporary directories. Never write to the project directory or `/tmp`.
4. Network-dependent tests must use mock providers or `for_testing()` helpers. Never make real API calls in automated tests.
5. `InMemoryWorkspaceStore` must implement the same `WorkspaceStore` trait as the real store, ensuring test fidelity.
6. `InMemorySessionStore` must implement the same `SessionStore` trait as the real store.
7. `StubRuntime` must implement `RuntimeDispatch` with sensible defaults so CLI tests can run without the full runtime.
8. Async tests must use `#[tokio::test]`, not `#[test]` with a manual runtime.
9. All test helper functions must be documented with their purpose and invariants.

## Examples

```rust
// Unit test in source file
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn read_file_returns_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();

        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let result = tool.execute(&ToolContext::default(), &path).await;
        assert!(!result.is_error);
        assert_eq!(result.output, "hello world");
    }

    #[tokio::test]
    async fn read_file_soft_error_on_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.txt");

        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let result = tool.execute(&ToolContext::default(), &path).await;
        assert!(result.is_error);
    }
}

// Integration test with InMemoryWorkspaceStore
#[tokio::test]
async fn tool_registry_assembles_correctly() {
    let store = InMemoryWorkspaceStore::new();
    let registry = ToolRegistryBuilder::new(store)
        .with_file_tools()
        .with_bash_tool()
        .build();

    assert!(registry.get("read_file").is_some());
    assert!(registry.get("bash_exec").is_some());
    assert!(registry.get("nonexistent").is_none());
}

// CLI test with StubRuntime
#[test]
fn cli_session_command_parses() {
    let runtime = StubRuntime::new();
    let cli = Cli::try_parse_from(["xaft", "session", "--name", "test"]);
    assert!(cli.is_ok());
}

// Agent test with mock provider
#[tokio::test]
async fn planner_agent_selects_editor() {
    let provider = MockProvider::for_testing()
        .with_response("I need to use the editor agent.")
        .build();
    let agent = PlannerAgent::new(provider);
    let action = agent.step(&mut context).await.unwrap();
    assert!(matches!(action, AgentAction::Handoff { agent: "editor" }));
}
```
