# Code-Plan Structure

When xaft's Planner produces a plan, it is represented as a structured
code-plan so the Coder can execute it step by step and QA can verify it.

## Plan shape

```json
{
  "goal": "Add error handling to all public functions in src/api/",
  "steps": [
    {
      "action": "edit",
      "path": "src/api/mod.rs",
      "reason": "Wrap public fns in Result",
      "tool": "edit_file"
    }
  ]
}
```

Each step carries the action, the reason, and the tool that would be used.
Plan mode prompts the agent to produce exactly this shape and to end with
"Switch to Auto mode (Shift+Tab) to execute this plan."

## Execution

- The Coder walks the steps in order, calling the planned tools.
- QA verifies the diff against the plan; the Fixer addresses gaps.
- On success the session's git worktree is committed; on failure it is rolled
  back.

## Custom plans

Planners are pluggable (see [Workflows](../guides/workflows.md)); a custom
planner returns the same step shape, so the Coder/QA pipeline is unchanged.

## Related

- [Workflows](../guides/workflows.md)
- [Modes](../guides/modes.md)
