# xaft — Autonomous Coding CLI

**Product Requirements Document**
**Version:** 0.1.0-draft
**Status:** Internal Engineering Specification
**Authors:** Platform Engineering
**Last Updated:** 2026

---

## What is xaft?

`xaft` is a next-generation autonomous coding CLI built on the `agtrs` Rust agentic framework. It is designed to operate as a full development partner — capable of understanding large codebases, decomposing multi-step engineering tasks, executing tools safely, generating patches, running tests, and collaborating through rich terminal interfaces.

`xaft` is not a wrapper around an LLM. It is a **production-grade autonomous engineering system** built from typed Rust primitives: agents, tools, planners, task runners, streaming engines, memory systems, and orchestrators.

---

## Documentation Index

### Core Documents

| File | Description |
|---|---|
| [SUMMARY.md](SUMMARY.md) | Full table of contents |
| [01_executive_summary.md](01_executive_summary.md) | Vision, goals, and positioning |
| [02_product_vision.md](02_product_vision.md) | Product philosophy and design tenets |

### Architecture

| File | Description |
|---|---|
| [architecture/01_runtime_architecture.md](architecture/01_runtime_architecture.md) | Core runtime model and crate layout |
| [architecture/02_agent_lifecycle.md](architecture/02_agent_lifecycle.md) | Agent construction, execution, teardown |
| [architecture/03_event_bus.md](architecture/03_event_bus.md) | SignalBus, AgentMessageBus, event sourcing |
| [architecture/04_workspace_model.md](architecture/04_workspace_model.md) | Filesystem, git, worktree architecture |
| [architecture/05_streaming_engine.md](architecture/05_streaming_engine.md) | Streaming execution model |
| [architecture/06_concurrency_model.md](architecture/06_concurrency_model.md) | Tokio, cancellation, parallel execution |
| [architecture/07_state_machines.md](architecture/07_state_machines.md) | Task, agent, session state machines |
| [architecture/08_crate_organization.md](architecture/08_crate_organization.md) | Crate layout and new crate proposals |

### Terminal UI

| File | Description |
|---|---|
| [tui/01_tui_architecture.md](tui/01_tui_architecture.md) | Ratatui rendering architecture |
| [tui/02_layout_engine.md](tui/02_layout_engine.md) | Pane system, layout, keyboard routing |
| [tui/03_diff_viewer.md](tui/03_diff_viewer.md) | Patch/diff viewer widget |
| [tui/04_streaming_panes.md](tui/04_streaming_panes.md) | Live streaming output panes |
| [tui/05_approval_dialogs.md](tui/05_approval_dialogs.md) | Human-in-the-loop approval UX |
| [tui/06_dashboards.md](tui/06_dashboards.md) | Cost, token, agent activity dashboards |

### Orchestration

| File | Description |
|---|---|
| [orchestration/01_multi_agent_coordination.md](orchestration/01_multi_agent_coordination.md) | Multi-agent coordination model |
| [orchestration/02_planning_system.md](orchestration/02_planning_system.md) | Planning strategies and intent decomposition |
| [orchestration/03_task_graph.md](orchestration/03_task_graph.md) | Task DAG execution engine |
| [orchestration/04_agent_handoffs.md](orchestration/04_agent_handoffs.md) | Handoff protocol and context passing |

### Tools

| File | Description |
|---|---|
| [tools/01_tool_calling_system.md](tools/01_tool_calling_system.md) | Tool registry, hooks, lifecycle |
| [tools/02_sandbox_execution.md](tools/02_sandbox_execution.md) | Safe shell execution model |
| [tools/03_git_integration.md](tools/03_git_integration.md) | Git-native operations |
| [tools/04_patch_diff_engine.md](tools/04_patch_diff_engine.md) | Patch generation and application |

### Memory

| File | Description |
|---|---|
| [memory/01_conversation_memory.md](memory/01_conversation_memory.md) | Short-term and long-term memory |
| [memory/02_context_window.md](memory/02_context_window.md) | Context window management |
| [memory/03_persistence_layer.md](memory/03_persistence_layer.md) | Storage backends |
| [memory/04_session_recovery.md](memory/04_session_recovery.md) | Checkpoint and resume |

### Providers

| File | Description |
|---|---|
| [providers/01_model_provider_abstraction.md](providers/01_model_provider_abstraction.md) | LlmProvider trait and implementations |
| [providers/02_cost_routing.md](providers/02_cost_routing.md) | Cost tracking and budget enforcement |
| [providers/03_provider_routing.md](providers/03_provider_routing.md) | Semantic routing and model selection |

### Safety

| File | Description |
|---|---|
| [safety/01_approval_safety.md](safety/01_approval_safety.md) | Approval gates and guardrails |
| [safety/02_sandboxing.md](safety/02_sandboxing.md) | Sandbox architecture |
| [safety/03_security.md](safety/03_security.md) | Security model and threat surface |

### Plugins & Interop

| File | Description |
|---|---|
| [plugins/01_plugin_system.md](plugins/01_plugin_system.md) | Plugin architecture |
| [plugins/02_mcp_compatibility.md](plugins/02_mcp_compatibility.md) | MCP protocol integration |

### API & Distribution

| File | Description |
|---|---|
| [api/01_axum_remote_api.md](api/01_axum_remote_api.md) | Remote agent HTTP/SSE API |
| [api/02_distributed_runtime.md](api/02_distributed_runtime.md) | Distributed execution model |

### Observability

| File | Description |
|---|---|
| [observability/01_telemetry.md](observability/01_telemetry.md) | Tracing, metrics, spans |
| [observability/02_event_bus.md](observability/02_event_bus.md) | Event bus architecture |

### UX

| File | Description |
|---|---|
| [ux/01_cli_ux_design.md](ux/01_cli_ux_design.md) | CLI command surface and interaction model |
| [ux/02_ux_philosophy.md](ux/02_ux_philosophy.md) | Core UX principles |

### Testing & Quality

| File | Description |
|---|---|
| [testing/01_testing_strategy.md](testing/01_testing_strategy.md) | Test architecture and patterns |
| [testing/02_benchmarking.md](testing/02_benchmarking.md) | Performance benchmarking strategy |

### Configuration & Deployment

| File | Description |
|---|---|
| [config/01_configuration_system.md](config/01_configuration_system.md) | Config file format and resolution |
| [config/02_deployment_packaging.md](config/02_deployment_packaging.md) | Packaging, installation, distribution |

### Example Flows

| File | Description |
|---|---|
| [flows/01_e2e_flows.md](flows/01_e2e_flows.md) | End-to-end workflow examples |
| [flows/02_collaboration_flows.md](flows/02_collaboration_flows.md) | Multi-agent collaboration examples |
| [flows/03_streaming_flows.md](flows/03_streaming_flows.md) | Streaming interaction examples |

### Competitive & Strategy

| File | Description |
|---|---|
| [competitive/01_competitive_analysis.md](competitive/01_competitive_analysis.md) | Comparison with Claude Code, Aider, Codex CLI |
| [competitive/02_goals_non_goals.md](competitive/02_goals_non_goals.md) | Goals and explicit non-goals |

### Roadmap

| File | Description |
|---|---|
| [roadmap/01_future_roadmap.md](roadmap/01_future_roadmap.md) | Phased roadmap and milestones |
| [roadmap/02_open_questions.md](roadmap/02_open_questions.md) | Open research and design questions |

### Appendix

| File | Description |
|---|---|
| [appendix/01_traits_interfaces.md](appendix/01_traits_interfaces.md) | Core trait surface reference |
| [appendix/02_scalability.md](appendix/02_scalability.md) | Scalability analysis |
| [appendix/03_performance.md](appendix/03_performance.md) | Performance constraints and targets |

---

## Design Tenets

1. **Rust-native** — No Python wrappers. No dynamic scripting. Pure Rust from transport to TUI.
2. **Async-first** — Every I/O operation is non-blocking. Tokio is the only runtime.
3. **Trait-driven** — All subsystems expose typed traits. Implementations are swappable.
4. **Streaming-first** — Output reaches the terminal before the agent run completes.
5. **Git-native** — Every file modification is tracked, diffable, and reversible.
6. **Safe by default** — Destructive operations require explicit approval.
7. **Observable** — Every agent action emits typed signals consumable by TUI, metrics, and logs.
8. **Composable** — Tools, agents, planners, and memory are independently extensible.

---

*This PRD is a living document. All sections are intended for implementation by senior Rust engineers.*