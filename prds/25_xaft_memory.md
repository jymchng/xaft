# xaft-memory

Product-Level Memory System PRD

## Purpose

Integrate long-term memory into xaft as a first-class autonomous coding capability.

This PRD builds on top of `agtrs-memory`.

`agtrs-memory` provides reusable framework primitives.

`xaft-memory` provides:

* coding-agent workflows
* developer memory UX
* project-aware recall
* coding-specific memory tools
* runtime bootstrapping
* TUI integration
* git-aware memory behavior

---

# Ownership Boundary

```text
agtrs-memory
    ↑ reusable framework runtime

xaft-memory
    ↑ coding-agent product layer
```

---

# Goals

1. Allow xaft agents to remember facts across sessions
2. Enable project-scoped architectural memory
3. Prevent repeated repo analysis
4. Provide memory-aware planning
5. Integrate memory into coding workflows
6. Add memory tools to agent toolchains
7. Surface memory in TUI streams
8. Persist knowledge across restarts
9. Support future semantic code intelligence

---

# Crates

```text
crates/xaft-memory
```

---

# Dependencies

```text
xaft-memory
    ├── agtrs-memory
    ├── xaft-runtime
    ├── xaft-config
    ├── xaft-tools
    └── xaft-tui
```

---

# xaft-Specific Concepts

## Project Memory

Store:

* architecture summaries
* module relationships
* coding conventions
* repository structure
* prior fixes
* build/test quirks
* CI failures
* developer preferences

---

## Git-Aware Memory

Memory entries should optionally attach:

```rust
branch
commit_sha
worktree_id
task_id
```

Support invalidation after rebases/history rewrites.

---

# Runtime Integration

## XaftRuntime

Add:

```rust
memory_manager: Arc<MemoryManager>
```

Inject into:

* ToolContext
* AgentContext
* planners
* workflows
* subagents

---

# Agent Tools

## RememberTool

Store:

* facts
* architectural insights
* debugging discoveries
* code conventions

## RecallTool

Search:

* project memory
* prior fixes
* similar bugs
* workflow history

## ForgetTool

Delete stale or incorrect memory.

## SummarizeMemoryTool

Compress older memories into durable summaries.

---

# Planner Integration

Planners MUST query memory before execution.

Examples:

* previous migration attempts
* historical failures
* known architectural patterns
* prior refactors

---

# TUI Integration

Add memory events to stream UI:

```text
[MEMORY] Remembered architecture pattern
[MEMORY] Retrieved prior fix
[MEMORY] Recalled similar issue from last session
```

---

# Session Resume

Memory should persist across:

* crashes
* Ctrl+C
* runtime restarts
* resumed sessions

---

# Config

```toml
[memory]
enabled = true
backend = "sqlite"
auto_remember = true
auto_summarize = true
project_scope_default = true
```

---

# Auto Extraction

Agents should automatically extract:

* architectural summaries
* dependency graphs
* repeated fixes
* important shell commands
* CI resolutions

via post-turn lifecycle hooks.

---

# Lifecycle Integration

Integrate memory into:

```text
on_start
on_finish
on_tool_result
on_error
on_cancel
on_budget_warning
```

---

# Memory Policies

Support:

* TTLs
* relevance decay
* project isolation
* branch isolation
* workspace scoping

---

# TUI UX

Future UI panels:

```text
┌─────────────────────────────┐
│ Project Memory              │
├─────────────────────────────┤
│ • Uses Tokio broadcast      │
│ • CI fails on nightly only  │
│ • Auth service deprecated   │
└─────────────────────────────┘
```

---

# Search Capabilities

Must support:

* semantic search
* substring search
* tag filtering
* branch filtering
* commit filtering
* architectural recall

---

# Workflow Integration

Memory-aware:

* subagents
* workflows
* DAG execution
* retries
* recovery systems

---

# Future Features

* vector search
* code embeddings
* graph memory
* architecture evolution tracking
* autonomous learning
* memory compression agents
* memory ranking models

---

# Testing Requirements

## Unit Tests

* project scoping
* git scoping
* auto extraction
* memory ranking
* planner recall
* TUI events

## Integration Tests

* runtime restart persistence
* multi-agent shared memory
* workflow memory propagation
* tool integration

## End-to-End Tests

* agent remembers repo architecture
* recall after restart
* memory-aware debugging
* long-running autonomous sessions

---

# Acceptance Criteria

1. Memory survives restarts
2. Project scoping works
3. Planner recall works
4. TUI streams memory events
5. Agents use memory autonomously
6. Git-aware metadata works
7. No runtime regressions
8. Full async safety
9. Production-grade persistence
10. Excellent developer UX
