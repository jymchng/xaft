# xaft

**A next-generation autonomous Rust-native coding agent runtime.**

<p align="center">
  <img src="assets/xaft-logo.png" alt="xaft logo" width="180" />
</p>

xaft plans, executes, verifies, and delivers code changes — with full
transactional safety, real-time observability, and multi-agent orchestration.
Give it a task in plain English. It reads your codebase, formulates a plan,
edits files, runs tests, and commits the result. Every mutation is reversible.
Every action is observable. Every agent is accountable.

```bash
xaft run "Add error handling to all public functions in src/api/"
```

## What it does

| Capability | Description |
|---|---|
| **Autonomous execution** | Plan → Code → Verify → Commit pipeline with multi-agent handoff (Planner → Coder → QA → Fixer) |
| **Transactional safety** | Git worktree isolation per session, fuzzy-anchor file edits, path-traversal protection, approval gates |
| **Multi-agent orchestration** | `HandoffOrchestrator`, dynamic `AgentRegistry`, inter-agent delegation |
| **Real-time observability** | SignalBus event system, live TUI dashboard, structured tracing, cost tracking |
| **Extensibility** | Trait-based tool system, plugin architecture, MCP integration, custom planners |
| **Reversibility** | Git worktree per session, auto-commit on success, rollback on failure |

## Repository layout

```text
crates/
├── xaft-tui        # Conversational streaming terminal renderer
├── xaft-runtime    # Core runtime, event loop, provider factory, orchestration
├── xaft-cli        # Command-line entry point + slash-command dispatch
├── xaft-tools      # Filesystem, git, and shell tools + dynamic tool factory
├── xaft-agent      # Agent loop, streaming, signals
├── xaft-agents     # Planner/Coder/QA/Fixer named agents + registry
├── xaft-config     # Typed configuration, TOML loading, hot-reload watcher
├── xaft-session    # Durable session + conversation persistence
├── xaft-memory     # Recall/remember/summarize/forget memory tools
├── xaft-skills     # Loadable agent knowledge files (SKILL.md / .xaft/skills)
└── xaft-tui        # (see above)
```

## Quick links

- [Quickstart](guides/quickstart.md) — install, configure a provider, run your first task
- [Architecture](guides/architecture.md) — event-driven runtime, three tasks, single render loop
- [Configuration](guides/configuration.md) — `xaft.toml`, env overrides, hot reload
- [Modes](guides/modes.md) — Safe → Plan → Yolo cycle, aliases, `/mode`
- [TUI](guides/tui.md) — triggers, paste, approvals, resume, telemetry
- [Workflows](guides/workflows.md) — standard workflow, dynamic handoff, custom planners
- [Subagents](guides/subagents.md) — typed subagents and the explore pool
- [Memory](guides/memory.md) — session/project/global tiers, tools
- [MCP](guides/mcp.md) — connecting MCP servers
- [Security](guides/security.md) — approval gates, sandboxing, path guards
- [Testing](guides/testing.md) — test strategy, mocking providers
- [Tools](guides/tools.md) — built-in fs/git/shell tools, custom tools
- [Reference](reference/cli.md) — CLI, kernel, storage, repository state, code-plan

## Contributing

See [Contributing](contributing.md) for the guide — branch workflow, crate
layout, and testing conventions.
