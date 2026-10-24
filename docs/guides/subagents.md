# Subagents

xaft supports typed subagents for parallel exploration and delegated work.

## `spawn_subagents`

Every agent turn — each agent of the standard, dynamic and meta pipelines and
each phase of a phased workflow — can fan work out to isolated workers with
the `spawn_subagents` tool:

```json
{
  "tasks": [
    {"type": "explorer", "task": "Find where sessions are persisted"},
    {"type": "tester", "task": "Run the session tests", "context": "Use cargo test -p xaft-session"}
  ],
  "max_concurrent": 4,
  "timeout_s": 3600
}
```

- `type` is one of the nine roles below (`research` is accepted for
  `researcher`); `task` is the worker's first message; the optional `context`
  is appended to its system prompt.
- At most `max_concurrent` workers (default 4) run at once; each has
  `timeout_s` (default one hour) of wall-clock time.
- The result is one JSON object: `ok`, `pool_id`, `total`, `succeeded`,
  `failed`, `error` and `results` — the workers' final responses, labelled
  and in task order:

  ```text
  === explorer #1 (✓ 4.2s) ===
  Sessions are written by …

  === tester #1 (✗ timed out after 3600s) ===
  [failed: timed out after 3600s]
  ```

A worker is a fresh agent: it never sees the parent's conversation, only its
task, and its tool calls stay private to it — the parent receives only its
final response. A worker that ends with no text gets one more turn to answer;
failing that, its result lists the tool calls it made.

The TUI shows each worker as it starts and finishes:
`↳ [spawn] explorer #1 (explorer): Find where sessions are persisted` and
`✓ [spawn done] explorer #1 · 4.2s`.

## Roles

| Role | Turns | Tools (the most it may have) |
|---|---|---|
| `explorer` | 15 | read, list, search, grep, git log/show/blame/grep |
| `planner` | 10 | read, list, search, grep |
| `implementer` | 20 | read, list, search, `write_file`, `edit_file`, `patch_file`, `append_to_file`, `run_python_expr` |
| `executor` | 24 | the implementer's, plus `bash_exec`, `run_command`, `run_python`, `run_tests` |
| `tester` | 20 | read, list, search, write/edit/patch, `run_tests`, `bash_exec`, `run_python_expr` |
| `reviewer` | 10 | read, list, search, grep, `git_diff`, `git_log`, `run_python_expr` |
| `documenter` | 15 | read, list, search, write/edit/patch |
| `verifier` | 12 | read, search, grep, `run_tests`, `run_python_expr`, `git_diff`, `git_log` |
| `researcher` | 8 | read, list, search |

A role's list is a ceiling: a worker gets the tools its role allows **and**
the spawning turn could call. A worker spawned by the planner (read-only
tools) has no write tool, so an `implementer` there fails at once with
`implementer has no write-capable tools after session filtering`. An
`implementer` that finishes without one successful write fails too, whatever
its final text says.

## Modes and approvals

Workers inherit the spawning turn's policy:

- **Plan** (and Review) mode does not offer `spawn_subagents` at all.
- **Safe** mode asks before each spawn, and every side-effecting call a
  worker makes asks again, through the same approval dialog as the parent's.
- A **headless** run in Safe mode denies those approvals unless started with
  `-y`/`--auto-approve` or `--dangerously-skip-permissions`.

Each worker's system prompt states the inherited policy and repeats the
mode's own instructions.

## Asking the parent, messaging peers

Workers can talk to the parent and to each other through bounded messages.
Every worker knows its own id (`task-0`, `task-1`, …) and its peers'.

| Tool | Who | Does |
|---|---|---|
| `ask_parent` | worker | ask the parent a question |
| `send_parent_message` | worker | report a status or finding to the parent |
| `poll_agent_messages` | worker | read queued messages |
| `send_peer_message` | worker | message another worker (or `pool` for all) |
| `ask_peer` / `answer_peer` | worker | ask a peer / answer a peer's question |
| `answer_subagent` | parent | answer a worker's question |
| `send_subagent_message` | parent | send a worker an instruction |
| `collect_subagent_results` | parent | wait for a pool and get its results |

When a worker asks the parent, `spawn_subagents` returns early with
`"status": "awaiting_clarification"`, the `pool_id` and the open questions,
and the pool keeps running. The parent answers with `answer_subagent`; the
worker continues with the answer, and `collect_subagent_results` returns the
pool's results. Queued messages reach a worker at the start of its next turn,
marked as untrusted context: a message can never change a worker's tools or
policy. Limits: 16 KiB per message, 256 queued messages per recipient, 16 open
questions per pool, 300 seconds before a message or question expires.

## The journal and resuming

Every worker's result, every complete pool and every message is written to
`.xaft/subagents/<session>.jsonl` in the workspace before it is reported, so
work finished before a cancellation is not lost. When a session asks again
for the same set of tasks under the same policy, the journaled results are
returned (`"pool_id": "cached"`) instead of running the workers again; a pool
with any failed worker is never reused.

## Explore pool

The standard pipeline's planner also has `explore_repository`, which runs up
to eight read-only file explorers in parallel and returns one summary per
file. Each fan-out is recorded as a worker orchestration that `/workers` lists.

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

## Differences from agenthicc

- The role tool lists use xaft's tool names; a role that may `write_file` may
  also `edit_file`, and one that may list directories may also use `glob` and
  `tree`.
- A worker is told its own id and its peers' ids, so `send_peer_message` and
  `ask_peer` can address them.
- An answer that arrives before the worker has finished the turn in which it
  asked is still delivered (agenthicc only followed up questions that were
  still pending).
- The TUI shows per-worker start and finish lines rather than a pool header
  and footer, and a journal cache hit is not announced in the transcript.
- There is no plugin API for custom roles or aggregators.

## Related

- [Workflows](workflows.md)
- [Modes](modes.md)
- [MCP](mcp.md)
- [Memory](memory.md)
