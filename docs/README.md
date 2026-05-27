# xaft Documentation

> Production-grade Rust coding agent runtime — orchestration, tooling, persistence, and interactive control in a single binary.

**xaft** is a multi-agent coding runtime built on the [`agtrs`](https://github.com/nicholasgasior/agtrs) framework. It orchestrates specialized agents—Planner, Coder, QA, Fixer—through a handoff pipeline, backed by SQLite session persistence, a type-safe signal bus, transactional file editing, and an interactive terminal UI. Every subsystem is designed for reliability: WAL-mode databases survive crashes, approval gates enforce human oversight, and the event loop uses `tokio::select!{biased}` to guarantee cancellation always wins.

This documentation covers everything from your first `xaft run` to the internal mechanics of the `HandoffOrchestrator`. Whether you are onboarding as a user, extending the tool registry, or debugging a signal-routing issue, you will find the relevant detail here.

---

## Getting Started

Guides that take you from zero to productive. These pages assume basic familiarity with the terminal and Rust tooling, but no prior knowledge of xaft internals.

| # | Page | Description |
|---|------|-------------|
| 1 | [Installation](getting-started/01-installation.md) | Binary downloads, building from source, prerequisites, and shell completion setup. |
| 2 | [Quick Start](getting-started/02-quick-start.md) | Your first task in under five minutes — API keys, config, and running a coding job. |
| 3 | [First Task Walkthrough](getting-started/03-first-task.md) | Step-by-step dissection of a real task: prompt entry, planning, tool calls, approval gates, and session replay. |

---

## Architecture

Deep technical reference for every crate, trait, and data flow. These pages are written for contributors and advanced users who need to understand *why* xaft behaves the way it does, not just *what* it does.

| # | Page | Description |
|---|------|-------------|
| 1 | [Architecture Overview](architecture/01-overview.md) | Crate topology, abstraction boundaries, bootstrap sequence, and the big-picture data flow from CLI invocation to agent turn completion. |
| 2 | [Crate Map](architecture/02-crate-map.md) | Crate-by-crate responsibilities, public APIs, re-exports, and inter-crate interaction contracts. |
| 3 | [Dependency Graph](architecture/03-dependency-graph.md) | Full Mermaid dependency graph, coupling analysis, layer boundaries, and direction-of-dependency rules. |

---

## Design Principles

xaft is built on a small number of reinforcing design principles that shape every crate and every API decision. Understanding these principles makes the codebase far more navigable:

1. **Signal-driven architecture.** The `SignalBus` is the central nervous system. Components never call each other directly for cross-cutting concerns—they emit signals. This decouples the TUI from the runtime, the approval gates from the agent loop, and the config hot-reloader from every consumer.

2. **Handoff over hierarchy.** The `HandoffOrchestrator` does not manage a tree of sub-agents. It runs a flat pipeline where each agent produces a `Handoff` decision—continue, delegate, or terminate. This keeps the control flow explicit and debuggable, with a hard cap of 14 handoffs per task to prevent runaway loops.

3. **Transactional side effects.** File edits go through `agtrs-workspace`, which maintains an undo journal. Git operations happen in managed worktrees. Shell commands execute in a sandboxed subprocess. If anything fails mid-flight, xaft can roll back to the last consistent state.

4. **Human-in-the-loop by default.** Every tool call that modifies the filesystem or executes a shell command passes through an `ApprovalGate`. The `TuiApprovalGate` blocks until the user approves, denies, or the 120-second timeout expires. `AutoApproveGate` is available for CI but must be explicitly selected.

5. **Six-layer configuration.** Defaults, global config, project config, session overrides, environment variables, and CLI flags are merged in a strict precedence order via `ConfigLoader`. Deep merge semantics ensure that nested keys are overridden without clobbering sibling fields.

---

## Contributing

Bug reports, feature requests, and pull requests are welcome. Please open an issue before submitting large patches so we can align on approach. The architecture documentation below is the canonical reference for crate boundaries—changes that cross crate boundaries require extra review.

---

## License

xaft is released under the MIT License. The `agtrs` framework crates follow the same license.
