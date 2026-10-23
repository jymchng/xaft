# Custom workflows

xaft can run any **phased workflow**: a declarative graph of phases, each one an
agent turn with its own prompt, tool capabilities, turn budget and transition
rules. This is the xaft counterpart of agenthicc's `WorkflowPlugin` + generic
`WorkflowRunner`. Every built-in phased workflow — `code_plan`,
`create_workflow`, `copy_website`, `reconstruct_site`, `site_imitate`,
`ui_ux_pro_max` — is itself a phased workflow, so everything below is also how
they work (`xaft workflows show <name> --toml` prints any of them).

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

1. **built-in** — `standard`, `dynamic`, `meta`, and the phased workflows listed
   under [Built-in phased workflows](#built-in-phased-workflows)
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

## Built-in phased workflows

| Workflow | Phases | Ported from agenthicc | Parameters (`[workflows.<name>]`) |
|---|---|---|---|
| `code_plan` | plan → execute → review ↔ execute → summarize; bound to **plan** mode | `workflows/code_plan/` | `<phase>_model` |
| `create_workflow` | design → generate → validate ↔ generate → summarize | `workflows/create_workflow/` | `<phase>_model` |
| `copy_website` | extract_target → site_study → design_spec → scaffold → implement_layout → implement_pages → implement_data → responsive_pass → parity_verify (a rejection returns to implement_layout, at most five times) → final_report | `workflows/copy_website/` | `study_model`, `build_model`, `verify_model` |
| `site_imitate` | analyze → plan → scaffold → (build → verify) once per planned component → final_verify; intent `URL \| new purpose` | `workflows/site_imitate/`, `workflows/name_that_ui.py` | `<phase>_model` |
| `ui_ux_pro_max` | product_discovery, then the `broad`, `targeted` or `accessibility_audit` route through research, design-system synthesis and review, stack adaptation, experience architecture, implementation, audit ↔ remediation, acceptance and delivery | `workflows/ui_ux_pro_max/` | `<phase>_model`, `max_design_review_attempts`, `max_remediation_cycles`, `max_phase_turns` |
| `reconstruct_site` | the 41-phase authoritative plan: research, a research gate, implementation with one `page` run per route, validation with controlled re-entry, the production infrastructure phases, final validation | `workflows/reconstruct_site/` | `profile` (`static`, `application`, `production`, `custom`), `custom_phases`, `max_reentries`, `viewports`, `phase_models`, `<phase>_model` and agenthicc's knob aliases |

The three website workflows (`copy_website`, `site_imitate`,
`reconstruct_site`) use the browser tools (`cloakbrowser_*`, or `playwright_*`
with `tools.browser_backend = "playwright"`) when the session registers them,
and say so — instead of inventing observations — when it does not. Those three
and `ui_ux_pro_max` are routed by Rust effects that port each agenthicc
runner's bookkeeping (component cursors, route profiles, review and
remediation budgets, research-gate baselines, re-entry budgets); each TOML
file's header documents the port and every place it deliberately differs from
agenthicc.

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
| `extra_tools` | `goal_list`, `workflow_state`, `artifacts`, `ask_user`, `workflow_authoring`, `ui_catalog` (the NameThatUI component dictionary), `ui_ux_pro_max`, `reconstruct` |
| `next`, `on_reject`, `on_error`, `parallel_with` | the graph |
| `require_plan_finalization` / `require_explicit_completion` / `require_explicit_review` | built-in transition tools; the phase gets continuation turns until one is called |
| `[[phases.transitions]]` | custom transition tools with `params` (each `type = "string"` by default, or `boolean`, `string_list`, `object_list`), `next`/`end`, `outcome` (`approved`, `rejected`, `exit`), `require_user_approval`, `guard`, `effect` |
| `max_turns`, `max_iterations`, `max_total_phase_runs` | budgets; an unbounded `next` cycle is a validation error |
| `max_attempts` | continuation turns one phase run gets while its transition is still missing (agenthicc's `_MAX_ATTEMPTS`); unlike `max_iterations` it does not cap how often the phase is entered, so a per-item loop phase keeps a small attempt budget |
| `model_param`, `max_turns_param` | read the phase's model / turn budget from a named workflow param — several phases can share one agenthicc knob (`copy_website`'s `study_model`) |
| `stable_system_prompt` (top level) | a workflow contract placed first in every phase's system prompt, unchanged between phases (agenthicc's `stable_system_prompt`) |
| `require_successful_commands` | fail the phase if a shell command it ran failed (a later successful re-run of the same command clears it) |

Prompt placeholders: `{intent}`, `{intent_head}` / `{intent_tail}` (the intent
before / after its first `|`, for structured intents such as `URL | purpose`),
`{context}` (the workflow context block),
`{phase}`, `{workflow}`, `{iteration}`, `{previous_output}`,
`{output.<phase>}`, `{data.<phase>.<arg>}` (a transition argument),
`{state.<key>}`, `{goals}`, `{next_goal}`, `{artifacts}`, `{working_dir}`.

**A phase changes only when a transition tool succeeds** — prose such as "done"
never advances a workflow. A phase with no transition tools runs one turn and
follows `next`.

Guards are preconditions (`goals_complete`, `draft_valid`, `mobile_verified`,
`implementation_plan_approved`, `infra_artifacts_present`, …); a refused guard
is returned to the agent as an error and the phase continues. Effects run when
the call is accepted: `publish_workflow_draft` publishes a `create_workflow`
draft, and the effects named after the site workflows (`copy_website`,
`site_imitate`, `ui_ux_pro_max`, `reconstruct_site`) validate the call's
arguments, keep that workflow's state, and may choose the edge — the next
component, a route profile's next phase, a validated re-entry target. A
routing effect only picks phases the graph declares, so the validator's
reachability checks still hold.

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

Besides the `workflow_authoring` tools (`describe_workflow_schema`,
`list_tool_capabilities`, `validate_workflow_draft`, …), the authoring agent
has xaft's read-only self-inspection tools — `list_xaft_docs`,
`read_xaft_doc`, `search_xaft_docs`, `inspect_xaft_source` and
`search_xaft_source` (agenthicc's `*_agenthicc_*` tools, over xaft's own
corpus; see [Tools](../guides/tools.md#self-introspection-tools)). They read
the documentation and source the binary was built from, so the agent can check
how the phased engine really behaves (`inspect_xaft_source('xaft_runtime::phased')`)
instead of guessing. They are `read`/`search` capability tools, so a
`mode_override = "plan"` phase keeps them.

## Checkpoints and resume

Every run of a phased workflow is checkpointed to
`.xaft/workflow-runs/<run-id>.json` after each phase transition: the intent,
every phase output, the iteration counters, the workflow's state (goals,
artifacts, drafts) and the edge it chose. A run that fails, is cancelled or is
interrupted resumes at exactly that edge — completed phases are not repeated,
and the resumed phase is told it is resuming (agenthicc's `[WORKFLOW RESUME]`
prompt).
