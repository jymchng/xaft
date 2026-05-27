# Crate Responsibilities

## Purpose

This document catalogs what each crate in the xaft workspace owns, what it does not own, and what its dependency edges look like. When you need to modify behavior, this document tells you which crate to open. When you need to add a cross-cutting feature, this document tells you which crates need coordinated changes. Violating crate boundaries is the fastest way to introduce coupling that makes the system untestable and fragile—so treat this document as a binding contract.

Every crate in xaft exists because it encapsulates a distinct responsibility that changes at a different rate than its neighbors. The config crate changes when users request new settings. The agent crate changes when we add new lifecycle hooks. The TUI crate changes when we redesign the interface. These rates of change are independent, and the crate boundaries ensure that a change in one doesn't force a recompile—or a regression—in another.

---

## Mental Model

Think of the xaft workspace as a layered cake where dependencies only point downward. The binary crate sits at the top, depending on everything. The CLI crate depends on the runtime and config. The runtime depends on the agent, tools, session, and config. The agent depends on tools and session. The TUI depends on the runtime's signal types but not on the runtime itself. At the bottom, the session and config crates depend on nothing but external libraries.

This layering means you can test the agent crate without starting a runtime, test the tools crate without an LLM provider, and test the config crate in complete isolation. The dependency graph has no cycles, and there are no "utility" crates that everything depends on—shared types live in the crate that owns them, and downstream crates depend on the owner.

---

## Architecture Explanation

### `xaft-binary`

**Owns:** The `main()` function and nothing else.

The binary crate is the thinnest possible entry point. It calls `xaft_cli::run()` with the OS arguments and handles the process exit code. It does not parse arguments, it does not configure tracing, it does not initialize the runtime. Its entire purpose is to be the compilation target that produces the `xaft` executable. If you find yourself adding logic here, you are almost certainly in the wrong crate.

**Dependencies:** `xaft-cli`

**Does not own:** Argument parsing, runtime initialization, configuration, anything with state.

### `xaft-cli`

**Owns:** Command-line argument parsing, subcommand dispatch, tracing initialization, and the `RuntimeDispatch` trait implementation that routes subcommands to the appropriate runtime methods.

The CLI crate is the control plane. It defines the `clap` command structure, parses arguments, initializes the tracing subscriber (which determines where logs go and at what level), and then dispatches to the runtime based on the subcommand. It owns the mapping from CLI intent to runtime action. For example, `xaft run "prompt"` maps to `XaftRuntime::bootstrap().run_task(prompt)`, while `xaft resume <id>` maps to `XaftRuntime::bootstrap().resume_session(id)`.

The CLI crate also owns the top-level error handling: if the runtime returns an error, the CLI crate formats it for the user and sets the exit code. It does not, however, own the error types themselves—those live in the crate that produces them.

**Dependencies:** `xaft-runtime`, `xaft-config`

**Does not own:** Runtime logic, agent logic, configuration definitions.

### `xaft-config`

**Owns:** Configuration struct definitions, the six-layer loading pipeline, deep merge logic, validation rules, and hot-reload file watching.

The config crate is the single source of truth for every configurable parameter in xaft. It defines the `XaftConfig` struct and all its nested sub-structs (agent configs, provider configs, tool configs, TUI configs). It implements the loading logic that reads from default values, system config, user config, project config, environment variables, and CLI flags, then deeply merges them into a single `XaftConfig`. It also implements validation: after merging, it checks for contradictory settings (e.g., a temperature outside [0, 2] or a max_turns of zero) and returns a descriptive error.

Hot-reload is owned by this crate as well. When a config file changes on disk, a file watcher triggers a reload, the merge is re-executed, and a `ConfigReloaded` signal is emitted on the SignalBus. Consumers that depend on config (the runtime, the TUI) subscribe to this signal and update their behavior accordingly.

**Dependencies:** External only (toml, serde, notify for file watching)

**Does not own:** Runtime behavior that is configured; it only owns the configuration itself.

### `xaft-runtime`

**Owns:** Bootstrap sequence, orchestration, provider chain construction, session lifecycle management, git worktree management, and the `XaftRuntime` facade.

The runtime crate is the orchestrator of orchestrators. It owns the `XaftRuntime::bootstrap()` method that wires everything together, the `HandoffOrchestrator` that drives agents through handoffs, and the provider chain that handles LLM failover. It also owns git worktree creation and cleanup, which means it is the crate that interacts with `git2` or shell-based git commands.

The `XaftRuntime` struct is the primary facade for the system. All higher-level operations—running a task, listing sessions, resuming a session—go through it. It holds the SignalBus, SessionStore, provider chain, and tool registries as fields, and it passes them to the orchestrator as needed.

**Dependencies:** `xaft-agent`, `xaft-tools`, `xaft-session`, `xaft-config`

**Does not own:** Agent internals (those are in `xaft-agent`), tool implementations (those are in `xaft-tools`), UI rendering (that's in `xaft-tui`).

### `xaft-agent`

**Owns:** The `Agent` trait, `XaftAgent` struct, `AgentBuilder`, `PlanAgentBuilder`, agent lifecycle hooks, planning cascade logic, signal emission from agents, and the `DynamicNamedAgent` wrapper.

The agent crate defines what an agent is and how it runs. The `Agent` trait specifies lifecycle hooks: `on_start`, `before_llm_call`, `on_tool_result`, `on_turn_complete`, `on_finish`. The `XaftAgent` struct implements this trait, running a ReAct loop that calls the LLM, executes tools, and processes results. The `AgentBuilder` provides a fluent API for constructing agents with specific names, roles, tool sets, and policies. The `PlanAgentBuilder` extends this with planning-specific configuration like escalation policies and refinement iteration limits.

Signal emission is a key responsibility of this crate. Every lifecycle hook emits a typed signal, and the agent's `stream_sink` field allows tool results to be forwarded to subscribers in real time. This is how the TUI learns about tool invocations as they happen.

**Dependencies:** `xaft-tools` (for the Tool trait), `xaft-session` (for conversation persistence)

**Does notOwn:** How agents are composed into workflows (that's in `xaft-runtime`'s orchestrator), how tools are implemented (that's in `xaft-tools`).

### `xaft-tools`

**Owns:** The `Tool` trait, all tool implementations, the `ToolRegistry`, tool categorization (read vs. write), and the `HandoffTool` / `RequestFixTool` meta-tools.

The tools crate is where the rubber meets the road. Every capability that an agent can exercise—reading files, writing files, running commands, searching code—is a `Tool` implementation in this crate. The `Tool` trait defines `name`, `description`, `parameters` (as a JSON Schema), and `execute`. The `ToolRegistry` maps tool names to implementations and supports filtering by category.

Two special tools deserve mention: `HandoffTool` allows an agent to pass control to another agent (this is how the orchestrator implements handoffs), and `RequestFixTool` allows the QA agent to signal that a fix is needed (this triggers the Fixer agent). These are not regular tools—they are control flow mechanisms that the orchestrator interprets specially.

**Dependencies:** External only (async-trait, serde, schemars for JSON Schema generation, tokio for async command execution)

**Does not own:** Which tools are assigned to which agent (that's in `xaft-runtime`'s orchestration logic).

### `xaft-tui`

**Owns:** The ratatui application, all widgets, the `EventBridge` that translates signals to UI events, the approval dialog for dangerous operations, and the `AppState` that drives rendering.

The TUI crate is completely decoupled from the runtime. It does not depend on `xaft-runtime`; instead, it subscribes to signals from the `SignalBus` and reads state from `AppState`, which is a snapshot of the runtime's current condition. The `EventBridge` is the adapter that converts typed signals into ratatui `Event` values, ensuring the UI never needs to know about runtime internals.

The approval dialog is a critical safety feature. When an agent wants to execute a dangerous operation (e.g., deleting a file, running a shell command with `sudo`), the tool emits a `RequiresApproval` signal. The TUI intercepts this signal, displays an approval dialog, and the user's response flows back to the tool via a oneshot channel. This means the TUI can block a tool execution without blocking the runtime—the tool simply awaits the oneshot response.

**Dependencies:** `xaft-agent` (for signal types), external (ratatui, crossterm, tokio)

**Does not own:** Runtime logic, agent logic, or any state that is not purely presentational.

### `xaft-session`

**Owns:** The `SessionStore` trait, SQLite persistence implementation, session creation, session resumption, conversation history storage, and token usage aggregation.

The session crate is the system's memory. It provides an async trait for persisting and retrieving sessions, backed by SQLite. Each session record contains: a UUID, the initial prompt, conversation histories partitioned by agent, tool invocation logs, cumulative token usage, and the final output. The SQLite schema is versioned, and the crate includes migration logic that runs on first connection.

An important invariant: the session crate never deletes data. Sessions can be listed and resumed, but not purged through the API. Cleanup is a separate administrative operation (or a future feature). This ensures that audit trails are always complete.

**Dependencies:** External only (rusqlite or sqlx, tokio, uuid, serde)

**Does not own:** How sessions are used by the runtime or agents; it only owns storage and retrieval.

---

## Extension Patterns

When extending xaft, use these crate ownership rules to determine where your code belongs:

| What you're adding | Crate to modify |
|---|---|
| New command-line flag | `xaft-cli` (parsing) + `xaft-config` (struct definition) |
| New configuration key | `xaft-config` |
| New tool implementation | `xaft-tools` |
| New agent lifecycle hook | `xaft-agent` |
| New orchestration stage | `xaft-runtime` |
| New TUI widget | `xaft-tui` |
| New session query | `xaft-session` |

Cross-crate changes are sometimes necessary—for example, adding a new signal type requires defining the struct in `xaft-agent` and subscribing to it in `xaft-tui`. In these cases, define the type in the crate that owns the domain (the one that emits the signal) and have the consuming crate depend on it.

---

## Common Pitfalls

1. **Adding logic to `xaft-binary`.** This crate should remain a one-liner. If you need to add startup logic, it belongs in `xaft-runtime::bootstrap()` or `xaft-cli::run()`.

2. **Making `xaft-tui` depend on `xaft-runtime`.** The TUI must remain decoupled from the runtime. If the TUI needs runtime state, expose it through the `AppState` or through signals—never through a direct reference to the runtime.

3. **Implementing tools outside `xaft-tools`.** Even if a tool is only used by one agent, it belongs in the tools crate. The tools crate is the single registry of all capabilities; scattering tool implementations across crates makes them undiscoverable and untestable.

4. **Putting config defaults in `xaft-runtime`.** All defaults belong in `xaft-config`. The runtime should never hardcode a value that could be configured; it should always read from the merged config.

5. **Ignoring the session schema version.** If you add a new column to the SQLite schema, you must add a migration in `xaft-session`. Failing to do so will cause runtime panics when existing databases are opened.

---

## Invariants

- **I1: Dependency direction.** Dependencies only point downward in the layer hierarchy. No crate depends on a crate above it.
- **I2: No circular dependencies.** The dependency graph is a DAG. If you find yourself wanting to add a circular dependency, extract the shared type into the lower crate.
- **I3: Single ownership.** Every type, every constant, every piece of domain logic has exactly one crate that owns it. If two crates need the same type, the lower crate owns it and the upper crate depends on it.
- **I4: Binary crate is trivial.** `xaft-binary` contains only `main()` with a single call to `xaft-cli::run()`.
- **I5: TUI independence.** `xaft-tui` never imports from `xaft-runtime`.

---

## Lifecycle Expectations

**Compile time:** The crate structure is designed to minimize recompilation during development. Changes to tool implementations only recompile `xaft-tools`. Changes to agent logic only recompile `xaft-agent`. Changes to the TUI only recompile `xaft-tui`. The runtime crate is the most expensive to recompile because it depends on everything, but it changes least frequently.

**Test isolation:** Each crate can be tested independently. `xaft-config` tests verify merge logic with no other crates. `xaft-tools` tests verify tool execution with mock LLM responses. `xaft-agent` tests verify lifecycle hooks with mock tools. `xaft-runtime` integration tests verify the full pipeline.

**Release boundaries:** In the future, crates may be released independently. The config crate could be published as a library for external tools. The tools crate could be extended by third-party plugins. The session crate could be replaced with a different storage backend. The crate boundaries make all of these possible without breaking the system.

---

## Examples

### Dependency graph visualization

```text
xaft-binary
  └── xaft-cli
        ├── xaft-runtime
        │     ├── xaft-agent
        │     │     ├── xaft-tools
        │     │     └── xaft-session
        │     ├── xaft-tools
        │     ├── xaft-session
        │     └── xaft-config
        └── xaft-config

xaft-tui (independent, depends only on xaft-agent for signal types)
  └── xaft-agent (signal types only)
```

### Adding a new tool: the crate journey

```text
1. Define the Tool impl in xaft-tools/src/tools/my_tool.rs
2. Register it in xaft-tools/src/registry.rs
3. Add config keys in xaft-config/src/tools.rs
4. Assign it to agents in xaft-runtime/src/orchestration.rs
5. Add CLI flag in xaft-cli/src/args.rs (if needed)
6. Display its output in xaft-tui/src/widgets/ (if needed)
```

---

## Implementation Guidance

Before modifying any crate, verify that the modification belongs in that crate by checking the ownership table above. If the change touches multiple crates, plan the order of modifications from the bottom up: config first, then tools/session, then agent, then runtime, then CLI, then TUI. This ensures that each crate compiles against the updated version of its dependencies before you move to the next.

When adding a new crate (which should be rare), ensure it has a single, well-defined responsibility and that it fits into the dependency hierarchy without creating cycles. Document the new crate in this file immediately—do not leave the documentation out of date.
