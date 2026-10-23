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

## Phased and custom workflows

Besides the three fixed orchestrators (`standard`, `dynamic`, `meta`), xaft
runs **phased workflows** — declarative phase graphs with transition tools,
checkpoints and resume. Built in: `code_plan` (plan → execute → review →
summarize), `create_workflow` (author a new workflow), `goal_flow` (clarify a
goal into goals, implement and verify each — `xaft run --goal "..."`),
`parallel_code_plan` (isolated Git-worktree workers), `make_xaft_tool` (author
a persistent custom tool) and `make_book` (write and compile a technical book);
your own live in `.xaft/workflows/` or `~/.config/xaft/workflows/`. See
[Custom workflows](../workflows/04-custom-workflows.md).

```bash
xaft workflows list
xaft workflows run code_plan --intent "add a --verbose flag"
xaft workflows create --intent "a release-checklist workflow"
xaft workflows runs && xaft workflows resume
```

## Modes and workflows

Mode selection shapes the system prompt and tool filter that flow into every
run request. When no `/workflow` selection is set, the workflow bound to the
active mode (`mode_bindings`) runs on submit — `code_plan` for plan mode — and
otherwise `[workflow] default_workflow` or `[workflow] mode` decides.

## Related

- [Architecture](architecture.md)
- [Subagents](subagents.md)
