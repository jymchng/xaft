# Workflows

xaft executes tasks through a plan → code → verify → commit pipeline with
multi-agent handoff.

## Standard workflow

1. **Plan** — the Planner classifies the task as informational (direct answer)
   or coding (full workflow), then produces a numbered implementation plan.
2. **Code** — the Coder reads, edits, and verifies changes.
3. **Verify** — QA reviews the result against the plan.
4. **Fix** — the Fixer addresses QA feedback (up to 14 handoffs with cycle
   detection).
5. **Commit** — on success, changes are committed in the session's git
   worktree; on failure the worktree is rolled back.

## Orchestration

- `HandoffOrchestrator` coordinates Planner → Coder → QA → Fixer
- `AgentRegistry` maps agent names to tool sets and prompts
- `HandoffTool` delegates sub-tasks between agents (allowed-target validation)
- `RequestFixTool` escalates QA → Fixer

## Custom workflows and planners

Planners are pluggable. Implement the planner trait and register it via the
agent registry; the runtime resolves the planner by task classification.

## Modes and workflows

Mode selection (Safe/Plan/Yolo) shapes the system prompt and tool filter that
flow into every run request. Plan mode produces a plan without mutating state;
Safe mode blocks writes, commands, and network.

## Related

- [Architecture](architecture.md)
- [Subagents](subagents.md)
