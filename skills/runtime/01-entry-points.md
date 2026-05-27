# Runtime Entry Points

## Purpose

This document maps every way to enter the xaft runtime, from the `main()` function through dispatch to agent completion. Understanding entry points is essential for debugging flow issues, adding observability, and extending the system with new subcommands or invocation modes. If you need to trace "how did we get here?" for any runtime behavior, start with this document.

Entry points are not just the obvious ones like `main()`. They include the `RuntimeDispatch` trait that routes subcommands, the `XaftRuntime` methods that initiate tasks, and the testing entry point that allows integration tests to exercise the full pipeline without a CLI. Each entry point has different initialization requirements, different error handling strategies, and different shutdown semantics.

---

## Mental Model

Think of xaft's entry points as a funnel. At the widest part are the CLI subcommands and the testing harness. These converge into the `RuntimeDispatch` trait, which normalizes the different invocation modes into a single interface. Below the dispatch layer, `XaftRuntime::bootstrap()` initializes the runtime infrastructure, and then task-specific methods (`run_task`, `resume_session`, `list_sessions`) drive the actual work. At the narrowest part of the funnel, the orchestrator takes over and the entry point is no longer relevant—execution is driven by the agent pipeline.

This funnel model means that every execution path, regardless of how it starts, goes through the same bootstrap sequence and the same orchestration logic. There are no "shortcut" paths that bypass the runtime infrastructure. This ensures consistent behavior, consistent observability, and consistent error handling across all invocation modes.

---

## Architecture Explanation

### `main()` in `xaft-binary`

The absolute top of the funnel. The `main()` function does exactly three things:

1. Calls `xaft_cli::run(std::env::args())` to delegate to the CLI crate.
2. Matches the returned `Result<(), CliError>` to set the process exit code.
3. Returns.

There is no other logic in `main()`. No logging setup, no signal handling, no global state initialization. All of that happens inside `xaft_cli::run()`. This extreme thinness is intentional: it ensures that the binary crate has zero logic to test and zero logic to maintain. Every behavioral change happens in a crate that can be tested independently.

### `xaft_cli::run()` in `xaft-cli`

The CLI crate's `run()` function is the first "real" entry point. It performs four operations in sequence:

1. **Parse arguments** using `clap`. The argument parser defines the full CLI surface: subcommands (`run`, `resume`, `list-sessions`, `config`), global flags (`--verbose`, `--config-path`), and per-subcommand options (`--model`, `--no-tui`, `--session-id`). Parsing produces a typed `CliArgs` struct that is the source of truth for what the user requested.

2. **Initialize tracing** based on the `--verbose` flag and any config-file logging settings. Tracing is set up before anything else because every subsequent operation should be observable. The tracing subscriber is configured with a layered format: structured JSON for log files, human-readable output for the terminal.

3. **Load configuration** by calling `xaft_config::load()` with the config path override (if provided). This executes the six-layer merge pipeline and validates the result. Configuration errors are fatal at this stage—they cause the CLI to print a diagnostic and exit with code 1.

4. **Dispatch** to the appropriate `RuntimeDispatch` method based on the subcommand. The dispatch target is a `XaftRuntime` instance, which is constructed by `XaftRuntime::bootstrap()`.

### `XaftRuntime::bootstrap()`

The bootstrap method is the single point where all runtime infrastructure is initialized. It performs the following steps in strict order:

1. **Create `SignalBus`** — The publish-subscribe backbone. Created first because every subsequent component needs it for observability.
2. **Open `SessionStore`** — SQLite connection pool. Created early because session persistence is needed before any task starts.
3. **Construct provider chain** — A `Vec<Box<dyn LlmProvider>>` ordered by priority. Provider construction may involve validating API keys and establishing HTTP connections.
4. **Assemble tool registries** — Read-tool registry and write-tool registry, populated with all standard tools plus any custom tools defined in the configuration.
5. **Create git worktree** — An isolated working directory for the task. The worktree is created from the current HEAD of the repository, ensuring a clean starting state.
6. **Return `XaftRuntime`** — The assembled runtime, ready to accept task requests.

If any step fails, `bootstrap()` returns an error. There is no partial initialization: either the runtime is fully constructed, or it is not constructed at all. This eliminates the need for "half-initialized" state checks throughout the codebase.

### `XaftRuntime::run_task(prompt: String)`

The primary task execution entry point. Called when the user runs `xaft run "prompt"`. It performs:

1. **Create a new session** in the `SessionStore`, recording the prompt and a generated session ID.
2. **Construct the orchestrator** based on the workflow configuration (standard or dynamic).
3. **Run the orchestrator** asynchronously, passing the session ID, provider chain, tool registries, and signal bus.
4. **Collect the result** from the orchestrator: either a successful output (with optional file changes) or an error.
5. **Persist the result** to the session store.
6. **Optionally auto-commit** based on the agent's commit policy.
7. **Return the result** to the caller (the CLI, which formats it for the user).

The `run_task` method is the boundary between the "setup" phase (bootstrap) and the "execution" phase (orchestration). Everything before `run_task` is deterministic and synchronous in effect; everything after is driven by LLM responses and tool executions, which are inherently unpredictable.

### `XaftRuntime::for_testing()`

A special entry point designed for integration tests. It creates a `XaftRuntime` with:

- An in-memory SQLite database (no file I/O)
- A mock LLM provider that returns canned responses
- A temporary directory as the worktree (automatically cleaned up on drop)
- All signal subscriptions enabled (for test assertions)

This entry point allows tests to exercise the full pipeline—from `run_task()` through orchestration to result collection—without needing an actual LLM API key, a real git repository, or a file system. Tests can verify signal emission, session persistence, and tool execution in complete isolation.

The `for_testing()` method accepts a `TestConfig` struct that overrides specific defaults (e.g., which mock responses to return, which tools to enable, whether to simulate provider failures). This enables targeted testing of error paths and edge cases.

### `RuntimeDispatch` trait

The `RuntimeDispatch` trait normalizes the different CLI subcommands into a single interface. It defines three methods:

```rust
#[async_trait]
pub trait RuntimeDispatch {
    async fn run(self) -> Result<()>;
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>>;
    async fn resume_session(&self, session_id: Uuid) -> Result<SessionOutput>;
}
```

Each subcommand maps to a different implementation of this trait:

- **`RunDispatch`** — Implements `run()` by calling `XaftRuntime::run_task()`. This is the default dispatch for `xaft run "prompt"`.
- **`ResumeDispatch`** — Implements `run()` by calling `XaftRuntime::resume_session()`. This is used for `xaft resume <session-id>`.
- **`ListDispatch`** — Implements `run()` by calling `list_sessions()` and printing the results. This is used for `xaft list-sessions`.

The trait abstraction allows the CLI crate to treat all subcommands uniformly: parse args, construct a dispatch, call `dispatch.run()`. The specific behavior is encapsulated in the dispatch implementation, keeping the CLI's main loop simple and consistent.

---

## Extension Patterns

### Adding a new subcommand

To add a new CLI subcommand (e.g., `xaft diff <session-id>` that shows the changes made in a session):

1. Add the subcommand to the `clap` enum in `xaft-cli/src/args.rs`.
2. Create a new `DiffDispatch` struct that implements `RuntimeDispatch`.
3. In the CLI's dispatch logic, match the new subcommand variant and construct `DiffDispatch`.
4. Implement `DiffDispatch::run()` by querying the session store and formatting the diff output.

The key insight is that the dispatch pattern isolates the new subcommand from the existing ones. The runtime infrastructure (bootstrap, signal bus, session store) is shared, but the specific behavior is encapsulated in the dispatch implementation.

### Adding a new runtime method

To add a new method to `XaftRuntime` (e.g., `cancel_task(session_id)`):

1. Add the method to the `XaftRuntime` impl block.
2. Ensure it respects the existing invariants (no partial initialization, consistent error handling).
3. Add a corresponding `RuntimeDispatch` implementation or extend an existing one.
4. Add a CLI subcommand that routes to the new dispatch.

### Custom bootstrap for specialized deployments

If you need a runtime with different initialization (e.g., a headless server mode without a worktree), create a new bootstrap method rather than modifying the existing one. The existing `bootstrap()` method serves the standard use case; specialized deployments should have their own entry points that share infrastructure but diverge where necessary.

---

## Common Pitfalls

1. **Bypassing bootstrap.** Every execution path must go through `XaftRuntime::bootstrap()` (or `for_testing()`). There is no way to construct a `XaftRuntime` directly because its fields are private. Attempting to work around this (e.g., by manually constructing the individual components) will result in missing signal subscriptions, unopened sessions, or uninitialized tool registries.

2. **Calling `run_task` before bootstrap completes.** The `run_task` method requires that the runtime is fully initialized. Calling it on a partially constructed runtime will panic. Always use the `bootstrap()` return value, which is guaranteed to be fully initialized.

3. **Forgetting to handle the dispatch error.** The `RuntimeDispatch::run()` method returns a `Result`. If the CLI does not handle the error (by printing it and setting the exit code), the process will exit with code 0 even on failure, which is incorrect and breaks CI pipelines.

4. **Using `for_testing()` in production code.** The `for_testing()` method creates a mock provider and an in-memory database. Using it in production code would result in no actual LLM calls and no persistent sessions. The method is gated behind `#[cfg(test)]` to prevent this mistake.

5. **Assuming `list_sessions` returns sessions in creation order.** The session store returns sessions ordered by last modification time, not creation time. If you need creation order, sort the results after retrieval.

---

## Invariants

- **I1: Single bootstrap path.** All non-test execution paths go through `XaftRuntime::bootstrap()`. There are no alternative initialization sequences.
- **I2: No partial initialization.** Bootstrap either returns a fully initialized runtime or an error. There is no "partially initialized" state.
- **I3: Dispatch isolation.** Each `RuntimeDispatch` implementation is independent. They share the runtime but do not share state with each other.
- **I4: Test isolation.** `for_testing()` creates a completely independent runtime instance. Multiple tests can run concurrently without interfering with each other.
- **I5: Exit code correctness.** The CLI always sets the process exit code based on the result of the dispatch. Success → 0, error → 1.

---

## Lifecycle Expectations

**Startup:** The journey from `main()` to the start of orchestration takes on the order of hundreds of milliseconds. The most expensive steps are provider construction (which may involve network calls for API key validation) and worktree creation (which involves git operations). Configuration loading and session store initialization are typically sub-millisecond.

**Execution:** Once `run_task()` is called, the runtime's role shifts from initialization to orchestration. The runtime does not drive the agent loop directly; it delegates to the orchestrator. The runtime's main responsibilities during execution are: emitting lifecycle signals, persisting session data after each agent turn, and handling provider failover.

**Shutdown:** When the orchestrator completes (or errors), `run_task()` returns, the CLI formats the output, and the process exits. The runtime does not have an explicit shutdown method; all cleanup happens via `Drop` implementations on the session store (flushing pending writes), the worktree (removing temporary files), and the signal bus (closing channels).

---

## Examples

### Tracing `xaft run "Fix the bug"` from main() to completion

```text
main()
  └── xaft_cli::run(args)
        ├── clap::parse() → CliArgs::Run { prompt: "Fix the bug" }
        ├── tracing::init()
        ├── xaft_config::load() → XaftConfig
        └── RuntimeDispatch::run(RunDispatch { runtime })
              └── XaftRuntime::bootstrap(config)
                    ├── SignalBus::new()
                    ├── SessionStore::open("xaft.db")
                    ├── ProviderChain::from_config(config.providers)
                    ├── ToolRegistry::with_standard_tools()
                    └── Worktree::from_head()
              └── runtime.run_task("Fix the bug")
                    ├── session_store.create("Fix the bug") → session_id
                    ├── HandoffOrchestrator::new(agents, tools, providers)
                    ├── orchestrator.run(session_id) → OrchestratorOutput
                    ├── session_store.persist_result(session_id, output)
                    ├── maybe_auto_commit(output, commit_policy)
                    └── return Ok(output)
```

### Integration test using `for_testing()`

```rust
#[tokio::test]
async fn test_planner_produces_plan() {
    let runtime = XaftRuntime::for_testing(TestConfig {
        mock_responses: vec![
            MockResponse::tool_call("plan", json!({"steps": ["read file", "edit file"]})),
        ],
        ..Default::default()
    });

    let output = runtime.run_task("Fix the bug".into()).await.unwrap();
    assert!(output.contains_plan());
    assert_eq!(output.agent_count(), 1); // Only planner ran
}
```

---

## Implementation Guidance

When adding a new entry point, follow this checklist:

1. **Determine the layer.** Is this a new CLI subcommand (add to `xaft-cli`), a new runtime method (add to `xaft-runtime`), or a new dispatch implementation (add to `xaft-cli`)?
2. **Ensure bootstrap consistency.** Every execution path must go through `bootstrap()`. If the new entry point needs different initialization, add a new bootstrap variant rather than bypassing the existing one.
3. **Add signal emission.** Every significant event should emit a signal. If the new entry point performs a new kind of action (e.g., cancelling a task), define a new signal type for it.
4. **Persist session data.** If the new entry point produces observable results, persist them to the session store. This enables `list-sessions` and `resume` to work correctly.
5. **Write integration tests.** Use `for_testing()` to verify the new entry point works end-to-end. Test both the success path and the error path.
6. **Update this document.** Add the new entry point to the architecture explanation and the examples section.
