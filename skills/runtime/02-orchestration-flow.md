# Orchestration Flow

## Purpose

This document explains the internals of the xaft orchestrator—the component that drives agents through handoffs, assigns tool sets, manages conversation keys, and determines when a task is complete. Understanding orchestration flow is essential for debugging why a particular agent received certain tools, why a handoff happened (or didn't), and how to customize the workflow for specialized use cases.

The orchestrator is the "brain" of the runtime. While the agent loop handles the mechanics of LLM calls and tool execution, the orchestrator handles the strategy: which agent runs first, which agent runs next, what tools each agent has access to, and how the conversation flows between agents. If the mental model document describes the pipeline, this document describes the pipeline's controller.

---

## Mental Model

Think of the orchestrator as a project manager overseeing a team of specialists. Each specialist (agent) has a specific role, a specific set of tools, and a specific handoff target. The project manager assigns work to the first specialist, waits for them to finish, then passes the work to the next specialist based on the handoff. The project manager does not do any of the work themselves—they coordinate.

In xaft, the `HandoffOrchestrator` plays this role. It is constructed with a list of `NamedAgent` instances, each of which has a name, an agent implementation, and a set of tools. The orchestrator starts the first agent, lets it run its ReAct loop until completion, and then checks if the agent used a `HandoffTool` to designate a successor. If so, the orchestrator starts the successor agent; if not, the workflow is complete.

The key insight is that handoffs are *tool calls*, not method calls. The agent decides to hand off by invoking the `HandoffTool` with a target agent name. The orchestrator intercepts this tool call, extracts the target, and routes control accordingly. This design means that the agent's LLM makes the handoff decision based on the context of the work, enabling intelligent routing that a hardcoded sequence could never achieve.

---

## Architecture Explanation

### `orchestrator::run_workflow()`

The top-level orchestration function. It performs the following steps:

1. **Construct the `HandoffOrchestrator`** with four `NamedAgent` instances (in the standard workflow): Planner, Coder, QA, and Fixer. Each is created with a specific tool set and a specific handoff configuration.

2. **Assign tool sets.** This is the most important architectural decision in orchestration. Tool assignment determines what each agent can do, and incorrect assignment leads to agents that cannot complete their tasks or, worse, agents that perform unsafe operations.

3. **Run the handoff loop.** Starting with the Planner agent, the orchestrator runs each agent's ReAct loop, checks for handoff tool calls, and routes to the next agent. The loop continues until an agent completes without calling the HandoffTool, or until a maximum handoff count is exceeded (a safety limit to prevent infinite handoff cycles).

4. **Post-orchestration analysis.** After the handoff loop completes, the orchestrator examines the final output to determine the result type: did the planner return a direct answer (e.g., a factual question that required no coding), or did the workflow produce a coding result (modified files)?

### NamedAgents and Tool Sets

The four standard agents and their tool assignments:

**Planner** — Receives read-only tools:
- `ReadFile` — Read file contents within the worktree
- `Grep` — Search for patterns across the codebase
- `ListDir` — List directory contents
- `Glob` — Find files matching a pattern
- `HandoffTool` — Hand off to the Coder with the plan

The Planner's tool set is intentionally limited to read operations. The Planner's job is to understand the codebase and produce a plan; it should never modify files. Giving the Planner write tools would violate the separation of concerns and could lead to premature, unplanned modifications.

**Coder** — Receives write tools:
- `WriteFile` — Create or overwrite a file
- `EditFile` — Make targeted edits to an existing file
- `RunCommand` — Execute a shell command in the worktree
- `HandoffTool` — Hand off to the QA agent

The Coder is the only agent that should modify files. It receives the Planner's plan as context and implements the changes. The `RunCommand` tool allows the Coder to run tests, linters, and build tools to verify its changes.

**QA** — Receives read tools plus `RequestFixTool`:
- `ReadFile`, `Grep`, `ListDir`, `Glob` — Same read tools as the Planner
- `RunCommand` — To run tests and verification commands
- `RequestFixTool` — Signal that the Coder's changes need fixes

The QA agent is unique because it has both read tools and a special control flow tool (`RequestFixTool`). The QA agent reviews the Coder's changes, runs tests, and checks for issues. If it finds problems, it uses `RequestFixTool` to trigger the Fixer agent. If everything looks good, it completes without requesting fixes.

**Fixer** — Receives write tools:
- `WriteFile`, `EditFile`, `RunCommand` — Same write tools as the Coder
- `HandoffTool` — Hand off back to QA (for re-verification) or complete

The Fixer is essentially a second Coder with a different system prompt. It receives the QA agent's feedback as context and applies fixes. After fixing, it can hand off back to QA for re-verification, creating a Coder → QA → Fixer → QA loop that continues until QA is satisfied.

### Conversation Keys

Conversation keys are the partitioning mechanism for session history. Each agent's conversation is stored under a unique key, ensuring that messages from different agents are not interleaved. The key format depends on the workflow type:

**Standard workflow:** `"{session_id}::workflow"` — All agents in the standard workflow share a single conversation key. This is because the standard workflow is a linear sequence where each agent picks up where the previous one left off. The conversation history includes all prior agents' messages, giving each new agent full context of what happened before.

**Dynamic workflow:** `"{session_id}::{initial_agent}"` — The conversation key is based on the initial agent's name. This allows different dynamic workflows to have independent conversation histories, even within the same session. When a dynamic workflow hands off, the new agent continues in the same conversation (because the key doesn't change), but it receives a system message indicating that it is now the active agent.

The choice of conversation key has significant implications. A shared key means every agent sees the full history of every previous agent, which provides maximum context but also increases token usage. A per-agent key means each agent starts with a clean history, which saves tokens but requires the handoff mechanism to pass relevant context explicitly.

### Post-Orchestration: Direct Answer vs. Coding Result

After the orchestrator completes, `run_workflow()` examines the final output to determine what to return to the caller:

1. **Direct answer.** If the Planner determines that the user's question can be answered without coding (e.g., "What does the `process_data` function do?"), it returns a text response instead of handing off to the Coder. The orchestrator detects this by checking whether the Planner used the HandoffTool. If it didn't, the Planner's response is the final output, and the result is a `DirectAnswer` containing the text.

2. **Coding result.** If the workflow progressed through the Coder (and optionally QA and Fixer), the result includes the modified files, the diff, and any test output. The orchestrator collects this information from the session store and returns a `CodingResult` containing the file changes, the test results, and the final agent's summary.

This distinction matters for the caller (the CLI or the testing harness): a direct answer is displayed as text, while a coding result is displayed as a diff summary with optional review options.

### Handoff Mechanics

When an agent calls the `HandoffTool`, the following sequence occurs:

1. The agent's ReAct loop intercepts the tool call (it is a tool call like any other from the LLM's perspective).
2. The `HandoffTool::execute()` method returns a `HandoffResult` containing the target agent name and a context message (the handoff reason, as generated by the LLM).
3. The orchestrator receives the `HandoffResult` from the agent loop.
4. The orchestrator looks up the target agent by name in its `NamedAgent` list.
5. The orchestrator starts the target agent, passing the context message as the initial user message.
6. The target agent begins its ReAct loop with the handoff context as its starting point.

This sequence is synchronous from the orchestrator's perspective: it waits for each agent to complete before starting the next one. There is no parallel execution of agents, because agents modify the same worktree and their outputs depend on each other.

---

## Extension Patterns

### Adding a new agent to the standard workflow

To add a new agent (e.g., a "Reviewer" between QA and Fixer):

1. Create the agent using `AgentBuilder` with the appropriate name, role, and tool set.
2. Wrap it in a `NamedAgent` with the correct handoff target.
3. Insert it into the `NamedAgent` list in `run_workflow()`.
4. Update the QA agent's `HandoffTool` target to point to the Reviewer instead of the Fixer.
5. Update the documentation (including this file).

### Creating a custom workflow

To create a workflow with a different agent topology:

1. Define the agents and their tool sets using `AgentBuilder`.
2. Create a `WorkflowConfig::Dynamic` configuration that specifies the initial agent.
3. In each agent's `HandoffTool`, specify the target agent name based on the desired routing logic.
4. The orchestrator will follow the handoff chain at runtime, enabling conditional routing.

### Changing conversation key strategy

To use per-agent conversation keys in a standard workflow:

1. Modify the orchestrator to construct conversation keys using the agent name rather than the fixed "workflow" string.
2. Add a system message to each handoff that summarizes the previous agent's output, since the new agent won't see the previous conversation history.
3. Test that token usage decreases and that agents still have sufficient context to perform their tasks.

---

## Common Pitfalls

1. **Giving the Planner write tools.** This is the most common mistake when customizing tool sets. The Planner must only read; giving it write access breaks the plan-then-execute model and can lead to inconsistent states where files are modified before a plan is finalized.

2. **Forgetting to add `HandoffTool` to an agent's tool set.** If an agent needs to hand off but doesn't have the `HandoffTool`, it will simply complete without handing off, causing the orchestrator to treat the workflow as finished. This is particularly insidious because the agent won't error—it will just stop early.

3. **Creating circular handoff chains.** If Agent A hands off to Agent B and Agent B hands off back to Agent A, the orchestrator will loop forever (or until the maximum handoff count is reached). Always design handoff chains with a clear termination condition.

4. **Using the wrong conversation key format.** If you use `"{session_id}::workflow"` for a dynamic workflow, all agents will share a single conversation history, which may not be what you want. Conversely, if you use per-agent keys for a standard workflow, agents will lack context from previous stages.

5. **Ignoring the `RequestFixTool` response.** The QA agent's `RequestFixTool` is not a `HandoffTool`; it is a separate mechanism that signals the orchestrator to start the Fixer. If you modify the orchestration logic and forget to handle `RequestFixTool` responses, the Fixer will never be triggered.

6. **Assuming handoff context is always sufficient.** The handoff context message is generated by the LLM, which may omit important details. If you find that downstream agents are making mistakes because they lack context, consider injecting additional structured data (e.g., the plan, the diff, the test output) into the handoff message.

---

## Invariants

- **I1: Sequential execution.** Agents execute one at a time, in handoff order. There is no parallel agent execution.
- **I2: Tool set immutability.** An agent's tool set is determined at construction time and cannot change during execution.
- **I3: Maximum handoff limit.** The orchestrator will stop after a configurable number of handoffs (default: 10) to prevent infinite loops.
- **I4: Worktree consistency.** Between agent handoffs, the worktree state is consistent. No agent starts while another agent's tool execution is in progress.
- **I5: Session persistence per turn.** After each agent turn completes, the conversation history and tool results are persisted to the session store before the next agent starts.

---

## Lifecycle Expectations

**Initialization:** The `HandoffOrchestrator` is constructed with all agents and their tool sets before execution begins. This means all agents exist in memory for the duration of the workflow, even if they are never used (e.g., the Fixer is not needed if QA finds no issues).

**Execution:** The orchestrator runs a loop: start agent → wait for completion → check for handoff → start next agent (or finish). Each agent's execution time is unpredictable, depending on the complexity of the task and the LLM's response latency. The orchestrator does not impose per-agent timeouts; that is the agent's responsibility (via `max_turns` and `cost_limit`).

**Termination:** The orchestrator terminates when:
- An agent completes without calling the HandoffTool (normal termination).
- The maximum handoff count is exceeded (safety termination).
- An agent encounters an unrecoverable error (error termination).

In all cases, the orchestrator collects whatever output is available and returns it to `run_workflow()` for post-processing.

---

## Examples

### Standard workflow: Planner → Coder → QA → (Fixer)

```text
User prompt: "Add input validation to the User struct"

1. Planner starts with read tools
   → Reads src/models/user.rs
   → Reads src/handlers/user_handler.rs
   → Produces plan: "Add validation to User::new(), update handler to check errors"
   → Calls HandoffTool(target="coder", context="Plan: add validation...")

2. Coder starts with write tools
   → Receives plan as context
   → Edits src/models/user.rs (adds validation)
   → Edits src/handlers/user_handler.rs (adds error checking)
   → Runs `cargo test` → 3 tests pass
   → Calls HandoffTool(target="qa", context="Implementation complete, tests passing")

3. QA starts with read tools + RequestFixTool
   → Reads modified files
   → Runs `cargo test` → 3 tests pass
   → Runs `cargo clippy` → 1 warning: unused import
   → Calls RequestFixTool(issue="Remove unused import in user.rs line 5")

4. Fixer starts with write tools
   → Removes unused import
   → Runs `cargo clippy` → 0 warnings
   → Completes without handoff (workflow finished)

5. Orchestrator: coding result with 2 modified files, all tests passing
```

### Direct answer: Planner answers without coding

```text
User prompt: "What does the User struct's validate method do?"

1. Planner starts with read tools
   → Reads src/models/user.rs
   → Determines the question can be answered without code changes
   → Returns: "The validate method checks that the email field contains '@'
      and that the age field is between 0 and 150. It returns a Result
      with specific error messages for each validation failure."
   → Does NOT call HandoffTool

2. Orchestrator: direct answer, no coding result
```

### Dynamic workflow with conditional routing

```text
Config:
  initial_agent = "triage"
  triage → (if bug) → "bug_fixer" | (if feature) → "feature_coder"

1. Triage agent reads the prompt and codebase
   → Determines this is a bug report
   → Calls HandoffTool(target="bug_fixer", context="Bug: null pointer in parse()")

2. Bug fixer implements the fix
   → Runs tests → passes
   → Completes without handoff

3. Orchestrator: coding result
```

---

## Implementation Guidance

When modifying orchestration logic, always consider the following:

1. **Tool set changes require careful analysis.** Adding a tool to an agent's set changes the LLM's available actions. Always test that the new tool does not confuse the LLM or cause it to deviate from its role (e.g., giving the Planner a write tool may cause it to start coding instead of planning).

2. **Handoff chains must terminate.** Before adding a new handoff target, trace the possible paths through the agent graph and verify that every path eventually reaches an agent that completes without handoff. If you find a cycle, add a termination condition (e.g., a maximum retry count within the agent's system prompt).

3. **Conversation key changes affect session resumption.** If you change the key format, existing sessions will not resume correctly because the stored conversation history uses the old keys. Either maintain backward compatibility or write a migration that re-keys existing sessions.

4. **Post-orchestration analysis must handle all result types.** When adding a new agent or workflow, ensure that `run_workflow()` correctly categorizes the result (direct answer vs. coding result). An uncategorized result will cause a runtime error.

5. **Test with the `for_testing()` entry point.** Mock the LLM responses to exercise specific orchestration paths (e.g., Planner → Coder → QA → Fixer → QA → complete) without needing actual LLM calls. Verify that handoffs happen in the expected order and that tool sets are correctly assigned at each stage.
