# Custom workflows

xaft can run any **phased workflow**: a declarative graph of phases, each one an
agent turn with its own prompt, tool capabilities, turn budget and transition
rules. This is the xaft counterpart of agenthicc's `WorkflowPlugin` + generic
`WorkflowRunner`. The built-in workflows are themselves phased workflows, so
everything below is also how they work.

## Built-in workflows

| Workflow | Phases | What it does |
|---|---|---|
| `code_plan` | plan → execute → review → summarize | plan (you approve), implement, review with tests; bound to **plan** mode |
| `create_workflow` | design → generate → validate → summarize | author a new custom workflow (see below) |
| `goal_flow` | clarify → decide_goals → implement_goal ⇄ verify_goal → summarize | clarify the intent into concrete goals, implement and verify each one, then summarise; `xaft run --goal "..."` starts it |
| `parallel_code_plan` | plan → decompose → dispatch → collect → integrate → verify → summarize | plan once, run each task in its own Git worktree and sub-agent, merge the branches back; runs are listed by `xaft worktrees` |
| `make_xaft_tool` (alias `make_agenthicc_tool`) | analyze → generate → validate → finalize | author, validate, smoke-test and publish a persistent custom tool (`.xaft/tools/<name>.toml`, see [Tools](../guides/tools.md)) |
| `make_book` | toc → research → assets → chapter × N → layout_review → front_matter → back_matter → final_layout_review → compile | write a technical book chapter by chapter and compile it to EPUB, HTML and (with pandoc) PDF |

Plus the three fixed orchestrators `standard`, `dynamic` and `meta`.
`xaft workflows show <name> --toml` prints any built-in's full definition.

## Running workflows

```bash
xaft workflows list                          # every workflow: builtin, user, project
xaft workflows show code_plan                # phases, edges, transition tools
xaft workflows show code_plan --toml         # the definition, as a starting point
xaft workflows run code_plan --intent "add a --verbose flag"
xaft workflows runs                          # checkpointed runs in this directory
xaft workflows resume [RUN_ID]               # continue a failed/interrupted run
xaft workflows reset RUN_ID                  # discard a run's checkpoint
xaft workflows validate path/to/flow.toml    # check a definition without running it
xaft workflows create --intent "a release-checklist workflow"
```

In the TUI:

| Command | Effect |
|---|---|
| `/workflows` | list workflows (`/workflows runs` lists runs, `/workflows reload` re-reads the directories) |
| `/workflow <name>` | run every following task with that workflow |
| `/workflow resume [run-id]` | arm a checkpointed run; your next message continues it — type `continue`, or guidance that is passed to the resumed phase |
| `/workflow reset` | clear the selection; `/workflow reset <run-id>` discards that run |

A mode can also choose the workflow: the workflow bound to the active mode with
`mode_bindings` runs on submit when no `/workflow` selection is set. The
built-in `code_plan` is bound to **plan** mode, so a task submitted in plan mode
is planned (read-only), approved by you, then implemented, reviewed and
summarised — agenthicc's Plan-mode behaviour.

## Where workflows come from

Later sources shadow earlier ones of the same name:

1. **built-in** — `standard`, `dynamic`, `meta`, `code_plan`, `create_workflow`,
   `goal_flow`, `parallel_code_plan`, `make_xaft_tool`, `make_book`
2. **user** — `~/.config/xaft/workflows/<name>.toml` or `<name>/workflow.toml`
3. **project** — `.xaft/workflows/<name>.toml` or `<name>/workflow.toml`
4. **config** — `[workflows.<name>]` tables in `xaft.toml`

Entries whose name starts with `_` or `.` (drafts, backups) are never loaded.
A definition that fails validation is not selectable; `xaft workflows list`
and `/workflows reload` report why.

A `[workflows.<name>]` table that declares neither `phases` nor `mode`
*configures* an existing phased workflow instead of replacing it:

```toml
[workflows.code_plan]
execute_model = "claude-haiku-4-5"   # per-phase model: <phase>_model
max_total_phase_runs = 30
```

## Writing a workflow

```toml
# .xaft/workflows/triage.toml
description = "Reproduce → Fix → Verify"
mode = "phased"
mode_bindings = []            # e.g. ["auto"] to make it the default in auto mode

[[phases]]
name = "reproduce"
agent_type = "explorer"       # read-only by default
max_turns = 15
next = "fix"
extra_tools = ["workflow_state"]
system_prompt = """
Reproduce the reported bug. Record the failing command with set_state("repro", ...)."""

[[phases.transitions]]
tool = "bug_reproduced"
description = "The bug is reproduced; hand the evidence to the fix phase."
params = [{ name = "evidence", description = "Command and output that show the bug." }]

[[phases.transitions]]
tool = "cannot_reproduce"
description = "The bug does not reproduce; end the workflow with an explanation."
outcome = "exit"
params = [{ name = "reason", description = "What was tried." }]

[[phases]]
name = "fix"
agent_type = "executor"
mode_override = "auto"
max_turns = 40
next = "verify"
require_explicit_completion = true   # loop until mark_execute_complete(summary)
prompt = """
Fix the bug. Evidence: {data.reproduce.evidence}
Reproduction command: {state.repro}"""

[[phases]]
name = "verify"
agent_type = "reviewer"
mode_override = "auto"
allowed_capabilities = ["read", "search", "git_read", "execute"]
max_iterations = 5
on_reject = "fix"
require_explicit_review = true       # approve_review / reject_review
```

`xaft workflows show --toml code_plan` prints a complete built-in to copy, and
`describe_workflow_schema` (available to the `create_workflow` agent) documents
every field. The essentials:

| Field | Meaning |
|---|---|
| `agent_type` | `auto`, `planner`, `executor`, `reviewer`, `explorer`, `verifier`, `custom` or `human` (a human phase asks you to approve; headless runs auto-approve) |
| `system_prompt` / `prompt` | the phase's role prompt and task message; `*_file` variants read a file inside a package directory |
| `mode_override` | `plan`/`review`/`safe` give read-only tools; `auto`/`yolo` give all tools. A phase without one follows the session mode |
| `allowed_capabilities`, `tools` | narrow the tool set: `read search write execute git_read git_write network memory`, then an exact name allowlist |
| `extra_tools` | `goal_list`, `workflow_state`, `artifacts`, `ask_user`, `workflow_authoring`, `parallel_workers`, `book`, `custom_tool_authoring` |
| `next`, `on_reject`, `on_error`, `parallel_with` | the graph |
| `require_plan_finalization` / `require_explicit_completion` / `require_explicit_review` | built-in transition tools; the phase gets continuation turns until one is called |
| `[[phases.transitions]]` | custom transition tools with `params`, `next`/`end`, `outcome` (`approved`, `rejected`, `exit`), `require_user_approval`, `guard`, `effect` |
| `max_turns`, `max_iterations`, `max_total_phase_runs` | budgets; an unbounded `next` cycle is a validation error |
| `max_attempts` | turns one entry of the phase may take to call a required transition (agenthicc's per-phase `_MAX_ATTEMPTS`; default: `max_iterations` when positive, else 10) |
| `skip_when` + `skip_to` | a guard checked as the phase is entered: when it holds, the phase is skipped for `skip_to` — how a per-item loop leaves for its summary (`skip_when = "goals_complete"`) |
| `model_key` | the workflow param that sets this phase's model besides `<phase>_model` (goal_flow's `implement_model` for `implement_goal`) |
| `require_successful_commands` | fail the phase if a shell command it ran failed (a later successful re-run of the same command clears it) |

Prompt placeholders: `{intent}`, `{context}` (the workflow context block),
`{phase}`, `{workflow}`, `{iteration}`, `{previous_output}`,
`{output.<phase>}`, `{data.<phase>.<arg>}` (a transition argument),
`{state.<key>}`, `{goals}`, `{next_goal}`, `{artifacts}`, `{working_dir}`,
`{run_id}`, `{goal.<field>}` (the active goal: `text`, `position`, `count`,
`attempts`, `summary`, `evidence`, `files`, `id`) and `{goals.list}` /
`{goals.dispositions}` / `{goals.files}`.

Transition parameters are strings unless they declare a JSON `type`
(`boolean`, `integer`, `array` — with `items = "string" | "object" | "any"` —
or `object`), as agenthicc's typed transition tools do:

```toml
[[phases.transitions]]
tool = "verify_goal"
effect = "goal_verdict"
params = [
  { name = "satisfied", type = "boolean", description = "True when the goal is satisfied." },
  { name = "evidence", description = "Checks and results." },
]
```

Guards: `goals_complete`, `goals_pending`, `has_artifacts`, `plan_approved`,
`draft_valid`, `chapters_complete`, `tool_draft_valid`. Effects are the
deterministic steps of the built-ins (`publish_workflow_draft`,
`finalize_goals`, `goal_implemented`, `goal_verdict`, `record_tool_plan`,
`record_tool_draft`, `publish_custom_tool`, `book_*`); `xaft workflows
validate` lists the known names in its errors.

Some tool groups contribute **group transitions**: tools that end the phase
themselves, the way agenthicc's custom runners do (`dispatch_parallel_workers`
launches the workers and ends the phase in one call). `parallel_workers`
offers the tools of its orchestration's current stage; `goal_list`'s
`postpone_phase` / `skip_phase` / `delete_phase` end the phase only when they
control the **active** goal, leaving through the phase's `on_reject` edge.

**A phase changes only when a transition tool succeeds** — prose such as "done"
never advances a workflow. A phase with no transition tools runs one turn and
follows `next`.

## Script phases — arbitrary logic

agenthicc workflows can run any Python in their runner. In xaft, any language
does it: a phase with `agent_type = "script"` runs its `command` (`sh -c`, in
the working directory, no model involved) and the command picks the edge.

```toml
[[phases]]
name = "test"
agent_type = "script"
command = "cargo test --workspace 2>&1 | tail -20"
timeout_s = 900
next = "summarize"
on_reject = "fix"          # a non-zero exit goes back to the fix phase
max_iterations = 5
```

The command is never templated (no injection of user text into a shell
line); it reads the run from environment variables — `XAFT_WORKFLOW`,
`XAFT_RUN_ID`, `XAFT_PHASE`, `XAFT_ITERATION`, `XAFT_INTENT`,
`XAFT_WORKING_DIR`, `XAFT_PREVIOUS_OUTPUT` and `XAFT_RUN_FILE` (the run's
checkpoint JSON: every phase output and the workflow state). If its stdout, or
the last line of it, is a JSON object, these keys decide the outcome:

| key | effect |
|---|---|
| `approved` | `true`/`false` overrides the exit code |
| `summary` | the phase output later phases see (`{output.<phase>}`) |
| `next` | a phase name, or `"end"` |
| `exit` | `true` ends the run early (status `exited`) |
| `state` | an object merged into the workflow state (`{state.<key>}` in prompts) |

Without JSON, exit code 0 is approved and anything else is rejected; a rejected
phase with no `on_reject` fails (so `on_error` applies, or the run stops and can
be resumed). With a user attached, the command is shown for approval before it
runs.

## Authoring with `create_workflow`

`xaft workflows create --intent "..."` (or `/workflow create_workflow`) runs a
four-phase authoring workflow: **design** (read-only; asks you questions and
gets your approval), **generate** (writes the TOML under
`.xaft/workflows/_drafts/`), **validate** (deterministic validation plus a
review against the design; a rejection returns to generate) and **summarize**.
Approving the draft publishes it atomically to `.xaft/workflows/`, backing up
any previous version under `.xaft/workflows/_backups/`; the new workflow is
immediately listed and runnable.

## Checkpoints and resume

Every run of a phased workflow is checkpointed to
`.xaft/workflow-runs/<run-id>.json` after each phase transition: the intent,
every phase output, the iteration counters, the workflow's state (goals,
artifacts, drafts) and the edge it chose. A run that fails, is cancelled or is
interrupted resumes at exactly that edge — completed phases are not repeated,
and the resumed phase is told it is resuming (agenthicc's `[WORKFLOW RESUME]`
prompt).
