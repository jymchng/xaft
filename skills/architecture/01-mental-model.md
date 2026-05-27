# xaft Mental Model

## Purpose

This document provides the complete mental model of the xaft runtime—the foundational conceptual framework that every other skill document builds upon. If you understand this document, you can predict how any subsystem will behave without reading its code. The mental model answers the question: "When I run xaft, what actually happens from the moment the process starts to the moment it exits?"

Understanding this model is prerequisite to working on any part of xaft. It defines the major abstractions, the data flow between them, and the sequencing guarantees that hold throughout execution. Every architectural decision in xaft traces back to the relationships described here.

---

## Mental Model

xaft is a **runtime that executes AI-driven coding tasks through orchestrated agent handoffs**. At the highest level, it follows this sequence:

1. **Bootstrap** — The runtime initializes its foundational infrastructure: a `SignalBus` for inter-component communication, a `SessionStore` backed by SQLite for persistence, and a configuration system that loads from six layered sources.
2. **Provider Chain** — The runtime constructs a chain of LLM providers, ordered by priority, so that if the primary provider fails (rate limit, outage), the next provider in the chain is tried automatically.
3. **Git Worktree** — For every task, xaft opens an isolated git worktree so that the agent's file modifications never touch the user's working directory until explicitly merged.
4. **Tool Registries** — The runtime assembles registries of tools, partitioned by access level (read-only vs. write) and by agent role, so that each agent receives only the tools it needs.
5. **Orchestration** — An orchestrator drives a sequence of specialized agents—Planner → Coder → QA → Fixer—through handoffs. Each agent runs a ReAct loop (think → act → observe) until it completes its sub-task or hands off to the next agent.
6. **Signal Emission** — At every lifecycle transition, agents and the runtime emit typed signals onto the SignalBus. The TUI and any external subscribers receive these signals in real time.
7. **Session Persistence** — Every conversation turn, tool invocation, and agent output is persisted to SQLite via the SessionStore, enabling session resumption and audit trails.
8. **Completion** — The orchestrator collects the final output, optionally auto-commits changes based on the agent's commit policy, and returns the result to the caller.

Think of it as a pipeline where data flows from the user's prompt through planning, coding, quality assurance, and fixing stages, with observability signals emitted at every stage boundary and all intermediate state persisted for recovery.

---

## Architecture Explanation

### SignalBus

The `SignalBus` is the central nervous system of xaft. It is a typed publish-subscribe channel where any component can emit a signal and any component can subscribe to signals of a given type. Signals are defined as Rust structs that derive the appropriate traits (`Clone`, `Send`, `'static`, and the signal marker trait). The bus uses tokio broadcast channels internally, meaning late subscribers miss messages that were emitted before they subscribed—this is intentional, as signals represent events, not state.

The TUI connects to the SignalBus through an `EventBridge` that translates signals into ratatui events, decoupling the rendering layer from the runtime's async infrastructure. This means the TUI never blocks the runtime; if the TUI falls behind, it simply drops stale signals and renders the latest state.

### SessionStore

The `SessionStore` abstracts SQLite persistence behind an async trait. Every session is identified by a UUID and contains: the initial prompt, the full conversation history (partitioned by agent), tool invocation records, token usage metrics, and the final output. Sessions support resumption: calling `resume_session` with a session ID restores the full conversation context so the orchestrator can continue from where it left off.

### Configuration Layering

Configuration loads from six layers, each overriding the previous via deep merge:

1. **Default values** — Hardcoded in the config structs.
2. **System config** — `/etc/xaft/config.toml` (or platform equivalent).
3. **User config** — `~/.config/xaft/config.toml`.
4. **Project config** — `.xaft/config.toml` in the repository root.
5. **Environment variables** — Prefixed with `XAFT_`, using `__` as nesting separator.
6. **CLI flags** — Highest priority, override everything.

Deep merge means that nested tables are merged recursively rather than replaced wholesale. For example, if the user config sets `agents.planner.temperature` but not `agents.planner.max_turns`, the merged config will have the user's temperature and the default max_turns. This is critical for predictable behavior: users only need to specify what they want to change.

### Provider Chain

LLM providers implement the `LlmProvider` trait, which defines a single `complete` method (streaming and non-streaming variants). The runtime constructs a `Vec<Box<dyn LlmProvider>>` ordered by priority. When a request is made, the primary provider is tried first. If it returns a retriable error, the next provider is tried, and so on down the chain. This failover is transparent to the agent, which simply sees a successful or failed LLM response.

### Git Worktree Isolation

Before any agent touches the filesystem, the runtime creates a git worktree rooted at a temporary directory. The worktree shares the same object store as the main repository (so it's cheap) but has its own working tree (so file modifications are isolated). When the task completes, the runtime can optionally merge the worktree's branch back into the user's branch, or discard it entirely. This isolation is non-negotiable: xaft never modifies files in the user's working directory directly.

---

## Extension Patterns

The mental model reveals several natural extension points. Because the system is organized as a pipeline of typed stages with signal-based observability, you can insert new behavior at any stage boundary by emitting or subscribing to signals. Because agents are assembled from tool registries, you can change agent behavior by modifying their tool sets. Because configuration is deeply merged, you can override any default without touching code.

Specific extension patterns include:

- **Inserting a new agent stage** (e.g., a "Reviewer" between QA and Fixer) by registering it in the `AgentRegistry` and updating the workflow config.
- **Adding a new signal type** for custom observability, then subscribing to it from the TUI or an external consumer.
- **Layering a new config source** (e.g., a remote config server) by implementing the config loading trait and inserting it into the merge chain.
- **Replacing the provider chain** at runtime based on project-level config, enabling per-project model preferences.

---

## Common Pitfalls

1. **Assuming signals are queued reliably.** The SignalBus uses broadcast semantics: if a subscriber is slow, it misses messages. If you need reliable delivery, use the SessionStore to persist the data and query it after the fact.

2. **Forgetting that config is deeply merged, not replaced.** A common mistake is to assume that setting a top-level key in a higher-priority layer replaces the entire subtree. In reality, only the specific keys you set are overridden; everything else inherits from lower layers.

3. **Modifying files outside the worktree.** The isolation guarantee only holds if all tool implementations respect the worktree root. A tool that uses an absolute path from the user's environment will bypass isolation and corrupt the working directory.

4. **Blocking the SignalBus.** Signal handlers run on the tokio runtime. If a handler performs blocking I/O or holds a lock for an extended period, it can back-pressure the entire signal pipeline, causing the TUI to freeze and agents to stall.

5. **Ignoring session partitioning.** Conversation history is partitioned by agent name. If two agents share a conversation key, their messages will be interleaved in the history, confusing the LLM. Always use distinct conversation keys for distinct agents.

---

## Invariants

- **I1: Worktree isolation.** No agent ever modifies files in the user's working directory. All writes go through the worktree.
- **I2: Signal ordering within a session.** Signals emitted by a single agent are ordered by emission time. Cross-agent ordering is not guaranteed.
- **I3: Config merge determinism.** Given the same six layers, the merged config is always identical. There is no randomness in the merge process.
- **I4: Provider failover transparency.** Agents cannot tell which provider in the chain actually served a request (unless they inspect the response metadata).
- **I5: Session continuity.** A resumed session has access to the full conversation history of the original session, including tool invocations and agent outputs.
- **I6: Agent tool isolation.** An agent can only use tools that were explicitly registered for it. There is no dynamic tool discovery at runtime.

---

## Lifecycle Expectations

**Initialization:** The runtime bootstraps in a fixed order: config → SignalBus → SessionStore → providers → tool registries → worktree. Each step depends on the previous one. If any step fails, the runtime exits with a diagnostic error; there is no partial initialization.

**Steady state:** During orchestration, the runtime cycles through agents. Each agent runs a ReAct loop, emitting signals at each iteration. The TUI consumes signals asynchronously. The SessionStore persists data after each agent turn completes.

**Shutdown:** When the orchestrator finishes (or encounters an unrecoverable error), the runtime emits a completion signal, flushes any remaining session data, cleans up the worktree (unless the user requests preservation), and terminates. There is no background daemon mode; xaft runs for the duration of a single task.

**Crash recovery:** Because the SessionStore persists after every turn, a crashed session can be resumed. The user runs `xaft resume <session-id>`, and the runtime re-bootstrap with the same session, continuing from the last completed agent turn.

---

## Examples

### Tracing a task from prompt to output

```text
User runs: xaft "Add error handling to the database module"

1. XaftRuntime::bootstrap() loads config, creates SignalBus + SessionStore
2. Provider chain constructed: [OpenAI → Anthropic → Local]
3. Git worktree created at /tmp/xaft-worktree-abc123
4. Tool registries assembled:
   - Planner: [ReadFile, Grep, ListDir, ...]
   - Coder:   [WriteFile, EditFile, RunCommand, ...]
   - QA:      [ReadFile, Grep, RunCommand, RequestFixTool]
   - Fixer:   [WriteFile, EditFile, RunCommand, ...]
5. HandoffOrchestrator runs:
   a. Planner reads the codebase, produces a plan → emits XaftAgentOutput
   b. Coder receives plan via handoff, implements changes → emits XaftAgentOutput
   c. QA reviews changes, finds an issue → emits RequestFixTool signal
   d. Fixer receives fix request, applies fix → emits Done
6. Orchestrator checks: did planner return a direct answer? No, it returned a plan.
   Did coder produce files? Yes. Final result = coding output.
7. Runtime optionally auto-commits (based on CommitPolicy::OnSuccess)
8. Worktree cleaned up, session persisted, TUI renders final state
```

### Configuration deep merge in action

```toml
# Default (layer 1)
[agents.planner]
temperature = 0.7
max_turns = 20
model = "gpt-4"

# User config (layer 3) — only overrides temperature
[agents.planner]
temperature = 0.3

# Merged result
[agents.planner]
temperature = 0.3  # overridden
max_turns = 20     # inherited from default
model = "gpt-4"    # inherited from default
```

---

## Implementation Guidance

When implementing a new feature, always start by identifying where it fits in the mental model:

- **Does it change the bootstrap sequence?** Then modify `XaftRuntime::bootstrap()` and update this mental model document.
- **Does it add a new signal?** Then define the signal struct, derive the required traits, emit it from the appropriate lifecycle hook, and subscribe to it where needed.
- **Does it change the agent pipeline?** Then modify the `HandoffOrchestrator` configuration and update the orchestration-flow skill document.
- **Does it add a new config key?** Then add it to the config structs, set a default, and verify that deep merge handles it correctly across all six layers.
- **Does it touch the filesystem?** Then ensure it operates within the worktree root and never references the user's working directory directly.

When in doubt, trace the flow: start at the entry point, follow the bootstrap sequence, walk through orchestration, and check signal emission. The mental model is your map; the code is the territory.
