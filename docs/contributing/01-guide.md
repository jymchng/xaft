# Contributing Guide

This guide covers everything you need to know to contribute to xaft effectively: navigating the codebase, understanding the architectural invariants that must be preserved, and meeting the requirements for pull requests. Whether you are fixing a bug, adding a feature, or improving documentation, following these conventions ensures that your contribution integrates smoothly and is reviewed promptly.

---

## Codebase Navigation

The xaft workspace is organized as a Cargo workspace with a flat crate structure. Every crate lives in a top-level directory under the workspace root — there are no nested workspaces or workspace members inside other workspace members. This flat structure makes it easy to find a crate's source code: if you need to work on `xaft-agent`, the code is in `xaft-agent/src/`.

### Key Directories

| Directory | Contents |
|-----------|----------|
| `xaft/src/` | Binary entry point. Three lines: call `xaft_cli::run()`, map to exit code, return. |
| `xaft-cli/src/` | Argument parsing with `clap`, subcommand dispatch, tracing initialization. |
| `xaft-config/src/` | Configuration loading, six-layer merge, validation, hot-reload. |
| `xaft-runtime/src/` | Bootstrap, provider chain construction, agent orchestration, event loop. |
| `xaft-agent/src/` | Agent implementations (`XaftAgent`, `PlanModeAgent`), lifecycle hooks, approval gate. |
| `xaft-tools/src/` | Tool implementations (file, shell, git, grep). Each tool in its own module. |
| `xaft-tui/src/` | Ratatui-based terminal UI. Event bridge, panels, keybinding handling. |
| `xaft-session/src/` | SQLite-backed session persistence, conversation history, session resume. |

### Finding Where Code Lives

When you need to find where a particular type or function is implemented, use `cargo doc --document-private-items` to generate local documentation, or use `rg` to search the codebase. The module structure within each crate mirrors the public API: `xaft_agent::XaftAgent` is defined in `xaft-agent/src/agent.rs`, `xaft_tools::ToolRegistry` is defined in `xaft-tools/src/registry.rs`, and so on.

The re-export conventions are important to understand. Feature crates re-export types from framework crates through their own public APIs, but they never expose the framework crate names directly. For example, `xaft_agent::Agent` is actually `agtrs_runtime::Agent`, but consumers should always use the `xaft_agent` path. This insulation allows the framework crates to be reorganized without breaking the public API.

### Dependency Flow

Dependencies flow strictly downward through the crate layers. The application layer (`xaft`) depends only on `xaft-cli`. The feature layer crates depend on each other only through `xaft-runtime` — no feature crate directly depends on another feature crate. The framework layer crates (`agtrs-*`) are independent of xaft entirely. Violating this dependency direction (for example, having `xaft-agent` depend on `xaft-tools`) is one of the most common architectural mistakes in pull requests and will be caught in review.

---

## Architectural Invariants

Architectural invariants are rules that the codebase must always satisfy, regardless of the feature being added or the bug being fixed. These invariants are maintained by convention and code review — there is no automated enforcement. When contributing, you must understand these invariants and ensure that your changes do not violate them.

### 1. No Upward Dependencies

A lower-layer crate must never depend on a higher-layer crate. `agtrs-runtime` must not depend on `xaft-agent`. `xaft-tools` must not depend on `xaft-runtime`. This rule ensures that each layer can be understood, tested, and evolved independently. If you find yourself wanting to import a type from a higher layer, the correct approach is to define a trait in the lower layer and implement it in the higher layer, using dependency inversion.

### 2. Agent Does Not Know Its Tools At Construction Time

Wait — actually, the agent receives its tools at construction time via `AgentBuilder::tools()`. The invariant is that the agent does not discover or create tools dynamically during execution. The tool set is fixed for the agent's lifetime. If you need dynamic tool availability, build a new agent with a different tool set rather than modifying an existing agent's tools mid-execution.

### 3. The Signal Bus Is the Only Inter-Agent Communication Channel

Agents must not hold direct references to each other. All communication between agents — handoffs, escalation notifications, status updates — flows through the `SignalBus`. This invariant ensures that agents are loosely coupled and can be replaced, reordered, or removed without modifying other agents' code. If you are tempted to pass an `Arc<Agent>` from one agent to another, use the signal bus instead.

### 4. Errors Are Mapped At Crate Boundaries

Each crate defines its own error type. When an error crosses a crate boundary, it must be wrapped in the receiving crate's error type. For example, when `xaft-runtime` receives a `ToolError` from `xaft-tools`, it wraps it in `RuntimeError::Tool(ToolError)`. This mapping is done automatically by the `#[from]` attribute on the error enum variants. Never leak a lower crate's error type through a higher crate's public API — it couples the higher crate to the lower crate's error hierarchy.

### 5. Async Functions Never Block

Every async function in xaft must be non-blocking. If you need to perform a blocking operation (file I/O, CPU-intensive computation), use `tokio::task::spawn_blocking` to offload the work to the blocking thread pool. Never call `std::thread::sleep` or perform synchronous file I/O inside an async context — it blocks the tokio runtime thread and can cause the entire application to stall.

### 6. CancellationToken Must Be Propagated

Every function that performs long-running or potentially infinite work must accept a `CancellationToken` parameter and check it cooperatively. If you add a new loop, a new recursive call, or a new network request, you must add cancellation checking. The convention is to check `cancel.is_cancelled()` at the top of each loop iteration and to use `tokio::select! { biased }` to race work against cancellation.

### 7. No Panics in Library Code

Library crates (`xaft-agent`, `xaft-tools`, `xaft-runtime`, etc.) must never panic. All error conditions must be returned as `Result::Err` with an appropriate error type. Panics are acceptable only in the binary crate (`xaft`) and in test code, where they produce clear failure messages. If you are calling an operation that can panic (like `unwrap()` on an `Option` or `Result`), replace it with proper error handling using `ok_or()` or the `?` operator.

---

## Development Workflow

### Setting Up

Clone the repository and build the workspace:

```bash
git clone https://github.com/example/xaft.git
cd xaft
cargo build --workspace
cargo test --workspace
```

The build requires Rust 1.80 or later. No external dependencies (beyond the Rust toolchain) are needed — all testing infrastructure is provided by the crate's `test-util` features.

### Running Tests

Run the full test suite:

```bash
# All unit and integration tests
cargo test --workspace

# Tests for a specific crate
cargo test -p xaft-agent

# A specific test by name
cargo test -p xaft-agent test_agent_turn_cancellation

# With test-util features enabled
cargo test -p xaft-runtime --features test-util
```

### Running the TUI

For manual testing, build and run the binary:

```bash
cargo run -- "Write a hello world program in Rust"
```

The TUI requires a terminal that supports true color and the alternate screen buffer. Most modern terminals (iTerm2, Windows Terminal, Kitty, Alacritty) work correctly. If the TUI renders incorrectly, try setting `COLORTERM=truecolor` in your environment.

### Linting

The CI pipeline runs `cargo clippy --workspace -- -D warnings` and `cargo fmt --check`. All clippy warnings are treated as errors, and the codebase must be formatted according to `rustfmt`. Before submitting a PR, run these locally:

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

---

## Pull Request Requirements

Every pull request must satisfy the following requirements before it can be merged. These requirements are enforced by CI and by code review.

### 1. All Tests Pass

The full test suite must pass on all supported platforms (Linux, macOS, Windows). CI runs the tests automatically, but you should run them locally before pushing to catch failures early.

### 2. No Clippy Warnings

`cargo clippy --workspace -- -D warnings` must produce no output. If clippy suggests a change, apply it. If you believe the clippy suggestion is incorrect, add an allow annotation with a comment explaining why:

```rust
#[allow(clippy::manual_range_contains)] // The range check is clearer than contains()
if x >= 0 && x < 100 {
    // ...
}
```

### 3. No Breaking Public API Changes Without a Migration Path

If your PR changes a public API (removes a method, changes a type signature, renames a struct), you must provide a migration path. For minor changes, add a deprecated alias that wraps the new API. For major changes, open an issue first to discuss the migration strategy with the maintainers.

### 4. New Public Types Have Documentation

Every public type, method, and module must have a doc comment. The doc comment should explain what the type does, not just repeat its name. For methods, the doc comment should describe the contract: what the method does, what arguments it expects, what it returns, and what errors it can produce.

```rust
/// Tracks cumulative token usage and cost across all LLM calls in a session.
///
/// The accumulator is updated by the `CostedProvider` after each LLM call completes.
/// Consumers can call `snapshot()` to get a point-in-time view of the accumulated costs.
///
/// # Thread Safety
///
/// The accumulator uses `tokio::sync::Mutex` internally, so it is safe to share
/// across tasks via `Arc`. The mutex is held only during the brief update operation,
/// so contention is minimal.
pub struct RunCostAccumulator {
    // ...
}
```

### 5. New Features Include Tests

Every new feature must include tests that verify its behavior. At minimum, include unit tests for the happy path and one error path. For features that involve multiple crates, include an integration test that exercises the cross-crate interaction.

### 6. Commit Messages Follow Conventional Format

Commit messages should follow the Conventional Commits format:

```
type(scope): description

[optional body]

[optional footer]
```

Where `type` is one of `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, and `scope` is the crate name (e.g., `agent`, `runtime`, `tools`). Examples:

- `feat(agent): add auto-retry hook for failed tool calls`
- `fix(runtime): resolve deadlock in cost accumulator on cancellation`
- `docs(config): clarify six-layer merge precedence`

This format enables automatic changelog generation and makes the commit history easy to scan.

---

## Code Review Process

Pull requests are reviewed by at least one maintainer. The review focuses on:

1. **Correctness**: Does the code do what it claims? Are edge cases handled?
2. **Architecture**: Does the change respect the invariants described above?
3. **Testing**: Are the tests sufficient? Do they cover the important scenarios?
4. **Documentation**: Is the new code documented? Are the docs accurate?
5. **Performance**: Does the change introduce any performance regressions? Are there unnecessary allocations or blocking operations?

Review turnaround is typically 1-2 business days. If you haven't received a review after 3 days, ping the maintainers on the pull request thread. Reviews are prioritized by urgency: bug fixes are reviewed before features, and features are reviewed before refactorings.
