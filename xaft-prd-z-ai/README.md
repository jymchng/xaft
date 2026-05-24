# xaft — Product Requirements Document

> **Next-generation autonomous coding CLI built on the agtrs Rust agent framework**

---

## Executive Summary

xaft is a terminal-native, autonomous coding agent that transforms how developers interact with codebases. Built on the `agtrs` framework — a production-grade Rust agent runtime providing ReAct loops, structured tool dispatch, event-driven architecture, and multi-agent coordination — xaft delivers a fully autonomous coding experience with human-in-the-loop guardrails, real-time streaming output, transactional file editing, and first-class git integration.

Unlike existing AI coding assistants that operate as passive suggestion engines, xaft operates as an **active collaborator**: it plans, executes, verifies, and iterates on code changes within a sandboxed, observable, and reversible workspace. Every action is mediated through agtrs primitives — the `AgentExecutor` ReAct loop, `SignalBus` event propagation, `FileEditor` transactional semantics, `WorkspaceStore` isolation, and `GitRepo`/`WorktreeGuard` branch management — ensuring that autonomous operation never compromises developer control or codebase integrity.

### Key Differentiators

| Capability | xaft | Typical AI Coding Tool |
|---|---|---|
| Execution Model | ReAct loop with lifecycle hooks | Single-shot prompt/response |
| File Editing | Transactional (commit/rollback) | Direct filesystem mutation |
| Git Integration | Branch-per-task, auto-commit, worktree isolation | Manual or none |
| Streaming | Token-by-token + tool execution events | Batch or line-buffered |
| Cost Control | Budget-enforced with real-time tracking | Per-request limits only |
| Multi-Agent | Coordinator + Collaborate team modes | Single agent |
| Planning | OneShot / IterativeRefinement / TreeOfThought | Implicit or none |
| Approval | Configurable gates per-tool, per-action | All-or-nothing |
| MCP | Native Model Context Protocol integration | None or bolt-on |

---

## Product Vision

**Make autonomous coding safe, observable, and reversible.**

xaft should feel like pairing with an experienced developer who:

1. **Plans before acting** — Uses structured planners (OneShot, IterativeRefinement, TreeOfThought) to decompose tasks, never jumps to implementation without understanding scope.
2. **Makes reversible changes** — Every file edit goes through `FileEditor`'s transactional model; every git operation uses branch-per-task with `WorktreeGuard`; nothing is permanent without explicit approval.
3. **Streams intent** — Token-by-token rendering of reasoning, tool calls, and results so the user always knows what's happening and can intervene.
4. **Respects budgets** — Real-time cost tracking via `CostedProvider`/`FallbackProvider` routing; hard stops when budgets are exhausted.
5. **Coordinates when needed** — Spawns sub-agents via `SubagentTool<T>` with typed returns for parallelizable work; uses `TeamMode::Coordinator` or `TeamMode::Collaborate` for multi-agent orchestration.

---

## Goals

### Primary Goals

1. **Autonomous task completion**: Given a natural language instruction, xaft should plan, execute, verify, and deliver a complete code change with >90% first-attempt success rate on well-scoped tasks.

2. **Safety by default**: No file mutation without `FileEditor` transaction; no git operation without `WorktreeGuard`; no shell command without approval gate (unless explicitly auto-approved). Every action emits a `SignalBus` event for audit.

3. **Sub-second streaming**: First token must appear within 500ms of request; tool execution progress must stream in real-time; TUI must render at 60fps with zero perceivable lag.

4. **Cost transparency**: Real-time USD cost tracking per-turn, per-session, and cumulative. Configurable hard budgets at session and daily levels. Automatic model fallback via `FallbackProvider` when primary hits rate limits or cost thresholds.

5. **Extensibility**: Plugin system via MCP (Model Context Protocol) tools; custom `Guardrail` implementations; user-defined tool registrations; configurable lifecycle hooks.

6. **Multi-agent coordination**: Support for `Chain` (sequential pipeline), `Workflow` (DAG-based parallel execution), and `TeamMode` (Coordinator/Collaborate) for complex tasks requiring decomposition.

### Secondary Goals

7. **Session persistence**: Resume interrupted sessions via `ConversationStore` and `MemoryStore`; full replay capability.
8. **Index-powered search**: Fast semantic code search via `xaft-index` crate (tree-sitter + embeddings).
9. **TUI excellence**: Ratatui-based terminal UI with syntax-highlighted diffs, cost gauges, and agent thought visualization.
10. **CI/CD integration**: Headless mode for automated pipelines; structured JSON output; exit codes reflecting task success/failure.

---

## Non-Goals

1. **IDE integration** — xaft is a CLI-first tool. IDE extensions are out of scope for v1 (but the architecture should not preclude them).
2. **General-purpose chatbot** — xaft is a coding agent, not a conversational AI. Small talk, creative writing, etc. are explicitly not supported.
3. **Code generation without verification** — xaft must always verify its changes (compile, test, lint) before presenting results. "Fire and forget" code generation is a non-goal.
4. **Multi-repo orchestration** — v1 focuses on single-repo workflows. Cross-repo coordination is deferred to v2.
5. **Custom LLM training** — xaft uses existing LLM providers; fine-tuning is out of scope.
6. **Visual/diff-based UI** — Web UI is out of scope; xaft is terminal-only for v1.
7. **Real-time collaboration** — Multi-user sessions are not supported; xaft is a single-user tool.

---

## How to Navigate This PRD

```
xaft-prd/
├── README.md                          ← You are here
├── SUMMARY.md                         ← Table of contents with descriptions
└── architecture/
    ├── 01_runtime_architecture.md     ← How xaft boots and runs
    ├── 02_agent_lifecycle.md          ← Agent instantiation through shutdown
    ├── 03_event_bus.md                ← SignalBus event catalog and flow
    ├── 04_workspace_model.md          ← File editing, transactions, git
    ├── 05_streaming_engine.md         ← Real-time output and backpressure
    ├── 06_tool_system.md              ← Tool trait, registration, hooks
    ├── 07_state_machines.md           ← All state machines and transitions
    └── 08_crate_organization.md       ← Rust crate layout and dependencies
```

### Reading Order

| Reader Profile | Recommended Path |
|---|---|
| **Engineering leadership** | README → SUMMARY → 01_runtime_architecture → 08_crate_organization |
| **Backend/agent engineer** | 01_runtime_architecture → 02_agent_lifecycle → 07_state_machines → 06_tool_system |
| **TUI/frontend engineer** | 05_streaming_engine → 03_event_bus → 01_runtime_architecture |
| **DevOps/infra engineer** | 08_crate_organization → 04_workspace_model → 07_state_machines |
| **Product/stakeholder** | README → SUMMARY → skip architecture details |

---

## Quick Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           xaft CLI Entry Point                         │
│                        (xaft-cli: clap + tracing)                       │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                         XaftRuntime                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────────────────┐  │
│  │ SessionMgr   │  │ ConfigLoader │  │ ProviderRouter                │  │
│  │ (Conversation│  │ (xaft.toml + │  │ (CostedProvider →             │  │
│  │  Store)      │  │  env + CLI)  │  │  FallbackProvider chain)      │  │
│  └──────┬───────┘  └──────┬───────┘  └──────────────┬────────────────┘  │
│         │                 │                          │                   │
│         ▼                 ▼                          ▼                   │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                    AgentExecutor (ReAct Loop)                     │   │
│  │  ┌─────────┐  ┌───────────┐  ┌──────────┐  ┌────────────────┐   │   │
│  │  │ Planner │→│ LLM Call  │→│ Tool     │→│ Lifecycle      │   │   │
│  │  │         │  │ (stream)  │  │ Dispatch │  │ Hooks          │   │   │
│  │  └─────────┘  └───────────┘  └──────────┘  └────────────────┘   │   │
│  └────────────────────────────┬─────────────────────────────────────┘   │
│                               │                                         │
│         ┌─────────────────────┼─────────────────────┐                   │
│         ▼                     ▼                     ▼                   │
│  ┌─────────────┐  ┌──────────────────┐  ┌──────────────────────┐        │
│  │ SignalBus   │  │ WorkspaceStore   │  │ GitRepo              │        │
│  │ (30+ events)│  │ + FileEditor     │  │ + WorktreeGuard      │        │
│  │ → TUI       │  │ (transactional)  │  │ (branch/commit/      │        │
│  │ → CostTrack │  │                  │  │  restore)            │        │
│  └─────────────┘  └──────────────────┘  └──────────────────────┘        │
└─────────────────────────────────────────────────────────────────────────┘
                               │
                ┌──────────────┼──────────────┐
                ▼              ▼              ▼
        ┌──────────┐  ┌──────────────┐  ┌──────────┐
        │ TUI      │  │ Headless/JSON│  │ SSE/Axum │
        │ (ratatui)│  │ (CI/CD)      │  │ (remote) │
        └──────────┘  └──────────────┘  └──────────┘
```

### Core Data Flow

```
User Prompt → XaftRuntime::run()
    → Session::load_or_create()
    → Planner::plan(prompt)
    → AgentExecutor::run_stream(agent, prompt)
        → loop {
            before_llm_call()      → SignalBus::emit(BeforeLlmCall)
            llm.stream()           → SignalBus::emit(StreamToken)
            after_llm_call()       → SignalBus::emit(AfterLlmCall)
            tool.dispatch()        → SignalBus::emit(ToolExecuting)
            tool.result()          → SignalBus::emit(ToolResult)
            on_turn_complete()     → SignalBus::emit(TurnComplete)
          } until done or cancelled
    → GitRepo::commit_if_clean()
    → Session::persist()
    → SignalBus::emit(SessionComplete)
```

### agtrs Primitives Used by xaft

| agtrs Primitive | xaft Usage | Location in PRD |
|---|---|---|
| `Agent` trait | Core agent abstraction with custom hooks | 02_agent_lifecycle.md |
| `AgentExecutor` | ReAct loop with streaming | 01_runtime_architecture.md |
| `Tool` trait + `ErasedTool` | All tool implementations | 06_tool_system.md |
| `SignalBus` | Event-driven TUI, cost tracking, git lifecycle | 03_event_bus.md |
| `LlmProvider` trait | Provider abstraction | 01_runtime_architecture.md |
| `CostedProvider` | Cost tracking per-request | 01_runtime_architecture.md |
| `FallbackProvider` | Model fallback chain | 01_runtime_architecture.md |
| `SubagentTool<T>` | Typed sub-agent spawning | 06_tool_system.md |
| `TaskRunner` | State machine for task execution | 07_state_machines.md |
| `OneShotPlanner` | Single-pass planning | 01_runtime_architecture.md |
| `IterativeRefinementPlanner` | Multi-pass planning with refinement | 01_runtime_architecture.md |
| `TreeOfThoughtPlanner` | Branching exploration planning | 01_runtime_architecture.md |
| `FileEditor` | Transactional file editing | 04_workspace_model.md |
| `WorkspaceStore` | File state management | 04_workspace_model.md |
| `GitRepo` / `WorktreeGuard` | Git operations with isolation | 04_workspace_model.md |
| `AgentMessageBus` | Inter-agent communication | 03_event_bus.md |
| `TeamMode` | Coordinator / Collaborate modes | 01_runtime_architecture.md |
| `Chain` | Sequential agent pipeline | 01_runtime_architecture.md |
| `Workflow` | DAG-based parallel execution | 01_runtime_architecture.md |
| `Guardrail` trait | Safety and policy enforcement | 06_tool_system.md |
| `MemoryStore` | Long-term memory persistence | 02_agent_lifecycle.md |
| `ConversationStore` | Conversation history | 02_agent_lifecycle.md |
| `Scratchpad` | Working memory within a turn | 02_agent_lifecycle.md |
| `StructuredLlm<T>` | Typed LLM responses | 06_tool_system.md |
| Approval gates | Human-in-the-loop confirmation | 07_state_machines.md |
| Cancellation tokens | Graceful shutdown | 07_state_machines.md |
| Streaming (`StreamEvent`) | Real-time output | 05_streaming_engine.md |
| Cost tracking | Budget enforcement | 01_runtime_architecture.md |
| User budgets | Session/daily spending limits | 01_runtime_architecture.md |

---

## Design Principles

1. **Composition over configuration** — xaft composes agtrs primitives rather than wrapping them in abstraction layers. The `XaftRuntime` struct holds owned instances of `AgentExecutor`, `SignalBus`, `WorkspaceStore`, etc., and orchestrates them through well-defined interfaces.

2. **Fail-safe defaults** — Every destructive operation defaults to requiring approval. Auto-approval is opt-in, never opt-out. The `FileEditor` transactional model ensures partial edits never corrupt files.

3. **Observe everything** — Every state transition, tool call, LLM request, and cost increment emits a `SignalBus` event. The TUI, logging system, and audit trail all consume the same event stream.

4. **Minimize latency** — Streaming starts before the first full LLM response. Tool execution progress streams intermediate output. The TUI renders incrementally without blocking the event loop.

5. **Respect the user's context** — xaft reads `.xaftignore`, respects `.gitignore`, uses the project's existing toolchain (cargo, npm, etc.), and never modifies files outside the workspace root without explicit instruction.

6. **Idempotent operations** — Where possible, tools should be idempotent. Re-running a file edit or git operation should produce the same result. This enables safe retry after interruption.

---

## Target Users

| Persona | Primary Use Case | Key Feature |
|---|---|---|
| Individual developer | "Fix this bug" / "Add this feature" | Autonomous plan→execute→verify loop |
| Tech lead | "Refactor this module" / "Migrate to v2" | Multi-agent coordination, git isolation |
| DevOps engineer | "Update all configs" / "Fix CI pipeline" | Headless mode, structured JSON output |
| Open-source maintainer | "Triage issues" / "Review PRs" | SubagentTool for parallel analysis |
| Security engineer | "Audit dependencies" / "Fix vulnerabilities" | Guardrails, approval gates, audit trail |

---

## Success Metrics

| Metric | Target | Measurement |
|---|---|---|
| First-attempt task completion rate | >90% on well-scoped tasks | Automated benchmark suite |
| Time to first token | <500ms | P99 latency from prompt to first StreamEvent |
| TUI render latency | <16ms per frame (60fps) | Frame time profiling |
| Session resume reliability | 100% | Crash recovery testing |
| Cost predictability | ±10% of pre-session estimate | Budget vs. actual tracking |
| Git cleanliness | 0 uncommitted changes after clean exit | Git status verification |

---

## Version Scope

### v0.1 (MVP)

- Single-agent ReAct loop with streaming TUI
- File editing via FileEditor with commit/rollback
- Git branch-per-task with WorktreeGuard
- Cost tracking and budget enforcement
- Shell tool with approval gates
- Basic planner (OneShot + IterativeRefinement)

### v0.2

- Multi-agent: Coordinator + Collaborate team modes
- TreeOfThought planner
- MCP tool integration
- Session persistence and resume
- Semantic code search (xaft-index)

### v0.3

- Workflow (DAG) support
- Custom guardrails plugin system
- SSE/Axum remote access
- CI/CD headless mode with structured output
- Diff-based TUI rendering

### v1.0

- Production-ready stability
- Full documentation and examples
- Performance optimization and profiling
- Comprehensive test coverage (>95%)
- Security audit

---

*This PRD is a living document. All architecture files contain implementation-level detail sufficient for a Rust engineer to begin coding immediately.*
