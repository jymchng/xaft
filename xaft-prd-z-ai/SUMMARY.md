# xaft PRD — Table of Contents

> Complete navigation index for the xaft Product Requirements Document suite.
> Each entry includes the file path, a detailed description, key sections, and estimated reading time.

---

## Root Documents

### [README.md](./README.md)
**Executive summary and product vision**

The root document establishing xaft's identity, goals, non-goals, design principles, and a quick-glance architecture diagram. Defines target users, success metrics, and version scope (v0.1 → v1.0). This is the entry point for all stakeholders.

| Section | Content |
|---|---|
| Executive Summary | Product definition, key differentiators table vs. typical AI coding tools |
| Product Vision | "Make autonomous coding safe, observable, and reversible" — five pillars |
| Goals / Non-Goals | 6 primary goals, 4 secondary goals, 7 explicit non-goals |
| Navigation Guide | Directory tree, reading paths by reader profile |
| Quick Architecture | ASCII overview diagram, core data flow, agtrs primitives mapping table |
| Design Principles | Composition, fail-safe, observe-everything, minimize-latency, idempotent |
| Target Users | 5 personas with primary use cases |
| Success Metrics | 6 measurable targets with measurement methods |
| Version Scope | v0.1 (MVP) through v1.0 roadmap |

**Estimated reading time:** 15 minutes

---

## Architecture Documents

### [01 — Runtime Architecture](./architecture/01_runtime_architecture.md)
**How xaft boots, initializes, and runs the main event loop**

Deep dive into the `XaftRuntime` struct — the top-level orchestrator that composes all agtrs primitives. Covers the full boot sequence from CLI argument parsing through agent execution to shutdown. Includes the `XaftRuntime` struct definition, session lifecycle, provider routing (`CostedProvider` → `FallbackProvider`), planner selection, `TeamMode` coordination, and the critical CLI → agent → streaming output pipeline.

| Section | Content |
|---|---|
| Boot Sequence | CLI args → config loading → provider init → runtime construction |
| XaftRuntime Struct | Full Rust pseudo-code definition with all fields |
| Session Lifecycle | Create → Load → Run → Suspend → Resume → Complete |
| Provider Routing | CostedProvider wrapping, FallbackProvider chain construction |
| Planner Selection | Heuristic-based planner choice (OneShot vs Iterative vs TreeOfThought) |
| Team Mode | Coordinator and Collaborate mode initialization |
| Main Event Loop | The outer loop consuming StreamEvents and dispatching to TUI/headless |
| Runtime Pipeline | Full ASCII diagram of the data flow |
| Error Handling | Panic boundaries, graceful degradation, cancellation propagation |
| Shutdown Sequence | Signal handling, in-flight request draining, state persistence |

**Estimated reading time:** 25 minutes

---

### [02 — Agent Lifecycle](./architecture/02_agent_lifecycle.md)
**Full agent lifecycle: instantiation through every turn to shutdown**

Documents how xaft wraps the agtrs `Agent` trait, implements custom lifecycle hooks for git auto-commit, terminal streaming, and plan-mode integration. Covers the complete turn cycle: `on_start` → `before_llm_call` → LLM invocation → `after_llm_call` → tool dispatch → `on_tool_result` → `on_turn_complete` → `on_finish`. Details the `PlanModeAgent` integration, `MemoryStore`/`ConversationStore`/`Scratchpad` usage, and how sub-agents are spawned via `SubagentTool<T>`.

| Section | Content |
|---|---|
| Agent Trait Wrapper | How xaft implements Agent with custom hooks |
| Lifecycle Phases | All 8 lifecycle methods with pre/post conditions |
| Turn Cycle | Detailed per-turn flow with event emissions |
| PlanModeAgent | How planning integrates into the agent lifecycle |
| Memory Architecture | ConversationStore, MemoryStore, Scratchpad composition |
| Sub-Agent Spawning | SubagentTool<T> with typed returns |
| Lifecycle Hooks | Git auto-commit hook, terminal streaming hook, cost tracking hook |
| Agent Configuration | System prompts, tool registrations, guardrails per agent |
| Error Recovery | How agents handle LLM failures, tool errors, and timeouts |
| Agent Shutdown | Graceful termination with state persistence |

**Estimated reading time:** 20 minutes

---

### [03 — Event Bus](./architecture/03_event_bus.md)
**SignalBus deep dive: all 30+ event types and their flows**

Complete catalog of every `SignalBus` event type used by xaft, organized by subsystem. Documents sync vs. async delivery semantics, event propagation order, and how the TUI, cost tracker, git lifecycle manager, and debugging subsystem consume events. Includes event flow diagrams for critical paths (LLM call, tool execution, file edit, git operation). Provides a guide for adding custom events.

| Section | Content |
|---|---|
| SignalBus Architecture | How SignalBus is constructed, event registration, subscriber model |
| Event Catalog | Full table of 30+ events with type, payload, delivery mode, consumers |
| Sync vs Async Events | Which events are synchronous (must complete before proceeding) |
| Event Flow Diagrams | ASCII diagrams for LLM call, tool execution, file edit, git op |
| TUI Event Consumption | How TUI subscribes to and renders events |
| Cost Tracking Events | BudgetEvent, CostIncrement, BudgetExhausted flows |
| Git Lifecycle Events | BranchCreated, CommitRequested, WorktreeOpened, etc. |
| Debugging Events | Internal diagnostics, trace-level events |
| Custom Events | Guide for extending the event system |
| Event Ordering | Guarantees about event delivery order within and across subsystems |

**Estimated reading time:** 20 minutes

---

### [04 — Workspace Model](./architecture/04_workspace_model.md)
**File editing, transactions, git integration, and workspace isolation**

Comprehensive documentation of xaft's workspace model: `WorkspaceStore` trait, `FileEditor` transactional semantics, `InMemoryWorkspaceStore` vs `OnDiskWorkspaceStore`, path sanitization, and integration with `GitRepo`/`WorktreeGuard`. Covers the complete `read_with_lines` → `replace_block` → `apply_diff` → `multi_edit` → `commit`/`rollback` flow with detailed examples.

| Section | Content |
|---|---|
| WorkspaceStore Trait | Full trait definition with all methods |
| Store Implementations | InMemoryWorkspaceStore, OnDiskWorkspaceStore trade-offs |
| FileEditor Transactional Model | Dirty tracking, commit, rollback semantics |
| Editing Primitives | replace_block, apply_diff, multi_edit with examples |
| Path Sanitization | Security model preventing directory traversal |
| Git Integration | GitRepo trait, WorktreeGuard lifecycle |
| Branch-per-Task | How xaft creates isolated git branches for each task |
| Auto-commit Strategy | When and how xaft commits changes automatically |
| Workspace Snapshots | Point-in-time workspace state for undo/redo |
| Concurrent Access | How multiple agents share a workspace safely |

**Estimated reading time:** 25 minutes

---

### [05 — Streaming Engine](./architecture/05_streaming_engine.md)
**Real-time output, backpressure, token rendering, and SSE bridging**

Deep dive into the `StreamEvent` enum, `AgentExecutor::run_stream`, SSE bridge for Axum, TUI consumption patterns, backpressure handling, token-by-token rendering, tool execution streaming, and how streaming interacts with approval gates. This is the critical path for user experience.

| Section | Content |
|---|---|
| StreamEvent Enum | All variants with payload definitions |
| AgentExecutor::run_stream | How the ReAct loop produces StreamEvents |
| SSE Bridge | Axum handler converting StreamEvents to SSE |
| TUI Consumption | How ratatui renders streaming events at 60fps |
| Backpressure Handling | Flow control when consumer is slower than producer |
| Token-by-Token Rendering | Incremental text rendering without full redraws |
| Tool Execution Streaming | How tool progress (stdout, partial results) streams |
| Approval Gate Interaction | How streaming pauses for human approval |
| Cancellation During Stream | How CancellationToken interrupts streaming |
| Streaming Performance | Benchmarks, optimization strategies, zero-copy paths |

**Estimated reading time:** 20 minutes

---

### [06 — Tool System](./architecture/06_tool_system.md)
**Tool trait, registration, hooks, caching, schema generation, and the #[tool] macro**

Complete documentation of xaft's tool system: the `Tool` trait, `ErasedTool` type-erased dispatch, `ToolContext`, tool hooks (`before`/`after`), `HookedTool` wrapper, cache system, `requires_confirmation` flag, tool schema generation via `schemars`, and the `#[tool]` procedural macro. Catalogs all built-in tools (workspace, git, shell, search, MCP) with their schemas.

| Section | Content |
|---|---|
| Tool Trait | Full trait definition, associated types, required methods |
| ErasedTool | Type erasure for heterogeneous tool collections |
| ToolContext | Execution context (workspace, git, bus, cancellation token) |
| Tool Hooks | before_tool/after_tool hooks with HookedTool wrapper |
| Cache System | Tool result caching with TTL and invalidation |
| requires_confirmation | Per-tool approval gate configuration |
| Schema Generation | schemars integration, JSON Schema output |
| #[tool] Macro | Procedural macro for declarative tool definition |
| Built-in Tools | Full catalog: ReadFile, WriteFile, EditFile, Git, Shell, Search, MCP |
| MCP Integration | How external MCP tools are registered and dispatched |
| Guardrail Integration | How Guardrail trait filters tool inputs/outputs |

**Estimated reading time:** 25 minutes

---

### [07 — State Machines](./architecture/07_state_machines.md)
**All state machines: TaskState, WorktreeState, FileEditor, AgentSession, Approval**

Every state machine in xaft documented with ASCII state diagrams, transition conditions, and invariants. Covers `TaskState` (Received → Planned → Running → Completed/Failed/Cancelled/Suspended/AwaitingApproval), `WorktreeState` (Open → Committed/Restored), `FileEditor` dirty tracking, `AgentSession` state, and the Approval workflow.

| Section | Content |
|---|---|
| TaskState | 8 states, all transitions, guard conditions |
| WorktreeState | Git worktree lifecycle management |
| FileEditor State | Dirty/clean tracking, transaction state |
| AgentSession State | Session lifecycle from creation to termination |
| Approval Workflow | Multi-state approval with timeout and escalation |
| State Machine Composition | How state machines interact and constrain each other |
| Persistence | How state is serialized for crash recovery |
| Invariants | Formal invariants that must hold at each state |
| Error Transitions | How errors cause state transitions |
| Concurrency | Thread-safety guarantees for concurrent state access |

**Estimated reading time:** 20 minutes

---

### [08 — Crate Organization](./architecture/08_crate_organization.md)
**Rust crate layout, dependency graph, feature flags, and subsystem mapping**

How xaft extends the agtrs workspace with new crates: `xaft-cli`, `xaft-tui`, `xaft-index`, `xaft-shell`, `xaft-mcp`, `xaft-config`, `xaft-session`. Includes full dependency graph, feature flag design, and how each crate maps to a subsystem.

| Section | Content |
|---|---|
| Crate Overview | All crates with responsibilities and LOC estimates |
| Dependency Graph | ASCII diagram of inter-crate dependencies |
| Feature Flags | Feature-gated functionality and why |
| agtrs Integration | How xaft crates depend on agtrs crates |
| xaft-cli | Clap argument parsing, entry point, runtime boot |
| xaft-tui | Ratatui-based terminal UI, event consumption |
| xaft-index | Tree-sitter parsing, embedding-based semantic search |
| xaft-shell | Shell command execution, sandboxing, output streaming |
| xaft-mcp | Model Context Protocol client, tool registration |
| xaft-config | Configuration loading, validation, defaults |
| xaft-session | Session persistence, resume, replay |
| Build Profiles | Dev, release, and benchmark profiles |
| Testing Strategy | Integration tests spanning crate boundaries |

**Estimated reading time:** 20 minutes

---

## Quick Reference: File Sizes and Detail Level

| File | Est. Lines | Detail Level | Contains Code |
|---|---|---|---|
| README.md | ~250 | Strategic | Architecture diagram only |
| SUMMARY.md | ~200 | Navigation | None |
| 01_runtime_architecture.md | ~400 | Implementation | Full struct definitions |
| 02_agent_lifecycle.md | ~350 | Implementation | Lifecycle method signatures |
| 03_event_bus.md | ~350 | Reference | Event type definitions |
| 04_workspace_model.md | ~400 | Implementation | Trait definitions, examples |
| 05_streaming_engine.md | ~350 | Implementation | Stream handling code |
| 06_tool_system.md | ~400 | Implementation | Tool trait, macro examples |
| 07_state_machines.md | ~350 | Reference | State diagrams, invariants |
| 08_crate_organization.md | ~300 | Structural | Cargo.toml snippets |

---

## Cross-Reference Matrix

Topics that appear across multiple files:

| Topic | Primary | Secondary References |
|---|---|---|
| AgentExecutor ReAct loop | 01_runtime | 02_lifecycle, 05_streaming |
| SignalBus events | 03_event_bus | 01_runtime, 02_lifecycle, 05_streaming |
| FileEditor transactions | 04_workspace | 06_tool_system, 07_state_machines |
| StreamEvent | 05_streaming | 01_runtime, 03_event_bus |
| Tool dispatch | 06_tool_system | 02_lifecycle, 07_state_machines |
| TaskState | 07_state_machines | 01_runtime, 02_lifecycle |
| GitRepo / WorktreeGuard | 04_workspace | 01_runtime, 07_state_machines |
| Cost tracking | 01_runtime | 03_event_bus, 05_streaming |
| Crate boundaries | 08_crate_org | All files |
| Approval gates | 07_state_machines | 05_streaming, 06_tool_system |

---

*Total estimated reading time for complete PRD: ~3.5 hours*
