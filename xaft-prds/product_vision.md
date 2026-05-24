# Product Vision

## Vision Statement

> xaft makes autonomous repository-scale coding safe, observable, and reversible — so engineers can delegate mechanical complexity without surrendering control.

## Design Tenets

### 1. Local-First, Cloud-Optional

`xaft` runs entirely on the developer's machine. LLM API calls are outbound-only. No code is transmitted to a central orchestration service. Workspace data never leaves the machine unless the user explicitly enables remote agent features.

Cloud capabilities (remote agent servers, distributed execution) are opt-in extensions, not prerequisites.

### 2. Streaming Is the Default Interface

Every agent interaction is a stream of structured events. The terminal UI renders agent progress in real time — character by character, tool call by tool call. A developer watching `xaft` work should feel like they are pair-programming with a fast, methodical colleague, not waiting for a black box to finish.

### 3. Git Is the Ground Truth

`xaft` never modifies files without git tracking. Every agent edit:
- Happens in an isolated git worktree
- Is associated with a diff reviewable before application
- Is committed with a generated commit message
- Is reversible with `xaft rollback`

The workspace is never left in an untracked state.

### 4. Approval Gates Are Mandatory for Destructive Operations

`xaft` maintains a risk classification for every tool call:
- `RiskLevel::Low` — file reads, searches, informational queries: silent auto-approve
- `RiskLevel::Medium` — file writes, test runs: log + auto-approve (configurable)
- `RiskLevel::High` — deletions, external API calls, `git push`, system commands: pause + human approval

Approval dialogs appear in the TUI with full context: the tool name, input arguments, estimated side effects, and a diff preview where relevant.

### 5. Composable Over Monolithic

`xaft` is not a vertically integrated product. It is an orchestration layer over composable primitives. Tool implementations, memory backends, planner strategies, and provider routing logic are all independently replaceable.

A team building domain-specific coding agents for their Rust monorepo should be able to add custom tools, custom planners, and custom memory stores without forking `xaft`.

### 6. Observable by Default

Every agent action emits typed events to the `SignalBus`. The TUI subscribes to these events to render dashboards. Log exporters subscribe to write structured JSON. Metrics collectors subscribe to push to Prometheus or OpenTelemetry. None of these consumers affect agent execution.

### 7. Cost Discipline

Every LLM call has an associated cost estimate. `xaft` tracks cumulative cost per session, per task, and per agent. Budget limits are enforced as hard stops. Cheap models handle planning, summarization, and classification; capable models handle complex reasoning. Model routing is automatic but auditable.

---

## User Mental Model

A `xaft` session has three conceptual layers the user can observe:

```
┌─────────────────────────────────────────────────────┐
│  INTENT LAYER    "Migrate all usages of serde_json  │
│                   to the new error API"              │
├─────────────────────────────────────────────────────┤
│  PLAN LAYER      Step 1: Index affected files        │
│                  Step 2: Generate migration patch    │
│                  Step 3: Run tests                   │
│                  Step 4: Fix failures                │
│                  Step 5: Commit                      │
├─────────────────────────────────────────────────────┤
│  EXECUTION LAYER Tool: search_codebase               │
│                  Tool: read_file src/error.rs        │
│                  Tool: write_file src/error.rs       │
│                  Tool: cargo test --workspace        │
└─────────────────────────────────────────────────────┘
```

The TUI renders all three layers simultaneously. The engineer can zoom into any layer to inspect details, pause execution, or override decisions.

---

## Non-Negotiable Properties

These properties must hold for every release:

1. **Reversibility**: Every file change can be reverted with a single command.
2. **Auditability**: Every tool call is logged with timestamp, input, output, and cost.
3. **Determinism**: Given the same plan and deterministic tool responses, `xaft` produces the same sequence of actions.
4. **Isolation**: Agent edits never modify the working tree directly; always via worktrees.
5. **Cancellation**: Any operation can be cancelled with `Ctrl-C` and the workspace left clean.

---

## Anti-Vision (What xaft Is Not)

- **Not an IDE plugin**: `xaft` is a CLI with a TUI. Editor integrations are a future layer.
- **Not a code generation API**: `xaft` is an end-user tool. The API (`xaft serve`) is an optional remote access layer.
- **Not autonomous by default**: Auto-approve is opt-in. Default posture is supervised.
- **Not a replacement for code review**: `xaft` generates PRs for human review; it does not approve them.
- **Not cloud-dependent**: Core functionality works offline with a local model provider.

---

*Previous: [Executive Summary](01_executive_summary.md) | Next: [Runtime Architecture →](architecture/01_runtime_architecture.md)*