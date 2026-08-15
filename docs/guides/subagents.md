# Subagents

xaft supports typed subagents for parallel exploration and delegated work.

## Explore pool

The runtime maintains a pool of explorer agents that can be dispatched in
parallel to gather context (files, searches, git history) before the planner
decides on an approach. Results are merged into the session context.

## Named agents

The built-in named agents are:

| Agent | Role |
|---|---|
| `planner` | Classifies the task and produces the implementation plan |
| `coder` | Reads, edits, and verifies code changes |
| `qa` | Reviews changes against the plan |
| `fixer` | Addresses QA feedback |

Each agent has its own conversation key in the durable store
(`<session>::workflow::<agent>`), allowing per-agent replay and resume.

## Handoff

Agents can hand off sub-tasks to each other via `HandoffTool`; handoffs are
audited and cycle-detected. See [Workflows](workflows.md).

## Related

- [Workflows](workflows.md)
- [Memory](memory.md)
