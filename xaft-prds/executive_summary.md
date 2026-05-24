# Executive Summary

## The Problem

Modern software engineering teams are bottlenecked not by computing power or tooling, but by the cognitive overhead of maintaining, extending, and refactoring large codebases. Code review cycles, context-switching between tools, and the mechanical labor of implementing well-understood patterns consume engineering time that could be spent on architectural decisions and novel problem-solving.

Existing AI coding tools — GitHub Copilot, Claude Code, Cursor, Aider — address autocomplete and single-file editing well. None of them are designed from the ground up as **autonomous, multi-step, repository-scale engineering systems**. They lack:

- Structured task planning with dependency graphs
- Resumable long-running workflows with checkpointing
- Parallel agent execution across repository subsystems
- Native git worktree isolation for agent edits
- Production-grade sandboxed command execution
- Rich terminal UIs with real-time streaming visualization
- Multi-provider cost routing with budget enforcement
- Type-safe plugin systems for domain-specific tooling

## The Solution: xaft

`xaft` is an **autonomous coding CLI** built on the `agtrs` Rust agentic framework. It occupies the gap between:

- Single-turn LLM completions (Copilot, simple chat)
- Full cloud-based agents (Devin, SWE-agent)

`xaft` runs **locally**, **autonomously**, and **safely** — with the engineer remaining in control through a rich terminal interface and explicit approval gates.

### Core Capabilities

| Capability | Description |
|---|---|
| **Repository-scale planning** | Decompose multi-file refactors into structured plan graphs |
| **Parallel agent execution** | Spawn isolated agents per subsystem in git worktrees |
| **Streaming TUI** | Live diff viewing, agent activity, cost tracking in Ratatui |
| **Git-native operations** | Worktree isolation, commit generation, PR creation |
| **Safe sandboxing** | Allowlist-based shell execution with replay audit logs |
| **Resumable workflows** | Checkpoint every task step; resume after crash or suspension |
| **Multi-provider routing** | Route tasks to cheap/capable models based on complexity |
| **Plugin system** | Add domain tools via MCP or native Rust plugins |

## Strategic Position

`xaft` targets:

1. **Senior engineers** who want autonomous help with well-understood but mechanical tasks (dependency upgrades, API migrations, test generation, refactors)
2. **Platform teams** who need auditable, policy-enforced automated code modification pipelines
3. **Monorepo maintainers** who need repository-scale analysis and cross-cutting changes
4. **AI-forward engineering teams** who want to build domain-specific coding agents on a production foundation

## Why Rust

Building `xaft` in Rust is not a stylistic choice — it is an architectural requirement:

- **Fearless concurrency**: Multiple agents editing in parallel worktrees with zero data races
- **Deterministic performance**: Frame-perfect TUI rendering without GC pauses
- **Compile-time safety**: Type-checked tool schemas, structured outputs, and event types
- **Systems-level terminal control**: Ratatui operates at the byte level of terminal rendering
- **Production reliability**: Memory safety without a runtime; no OOM crashes in long-running sessions

## Investment Thesis

The `agtrs` framework provides a production-grade foundation that would take 18+ months to build from scratch:

- `AgentExecutor` ReAct loop with cancellation and deadline support
- `SubagentTool` for isolated context-window delegation
- `TaskRunner` with checkpointing and state machine management
- `SignalBus` for typed observability events
- `AgentMessageBus` for inter-agent P2P communication
- `WorkspaceEditor` with atomic file operations
- `ShellExecutor` with policy-based sandboxing
- `GitRepo` + `WorktreeManager` for isolated editing

`xaft` builds the **user-facing product** on top of this proven infrastructure.

## Success Metrics

| Metric | Target (1 month) |
|---|---|
| Repository indexing latency | < 5s for 100K LOC repo |
| Streaming time-to-first-token | < 500ms |
| Task success rate (benchmark) | > 75% on SWE-bench Lite |
| TUI frame rate | Stable 30fps during streaming |
| Session recovery time | < 2s from checkpoint |
| Cost per average task | < $0.50 |

## Summary

`xaft` is the autonomous coding CLI that senior engineers will trust with their production codebases — because it is auditable, reversible, observable, and built with the same systems-level discipline as the codebase it modifies.

---

*Next: [Product Vision →](02_product_vision.md)*