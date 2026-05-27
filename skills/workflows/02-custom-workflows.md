# Custom Workflows

## Purpose

Custom workflows let you define specialized multi-agent topologies beyond the built-in handoff patterns. While the default xaft configuration provides a planner-coder-fixer-reviewer pipeline, many projects need different agent compositions—a security auditor agent, a documentation writer, a database migration specialist, or a multi-repository coordinator. This document explains how to define new agents with `AgentDefinition`, register them in `AgentRegistry`, configure dynamic workflows with `WorkflowConfig::Dynamic`, and run them with `run_dynamic_handoff()`. By the end, you should be able to compose any multi-agent workflow that xaft's handoff mechanics support.

Custom workflows are the primary extension point for teams that want xaft to understand their domain-specific processes. Rather than hacking the system prompt of a general-purpose agent, you create focused agents with narrow tool sets and clear handoff boundaries, resulting in more reliable behavior and easier debugging.

## Mental Model

Think of a custom workflow as a **directed graph of agents**. Each node is an `AgentDefinition` (who), and each edge is a `can_handoff_to` permission (who can delegate to whom). The `WorkflowConfig::Dynamic` config selects a subgraph of this graph and sets the entry point. `run_dynamic_handoff()` then executes a walk through this graph, with the agents deciding at runtime which edge to follow.

```
AgentRegistry (full graph)
┌──────────┐     ┌──────────┐     ┌──────────┐
│ planner  │────▶│  coder   │────▶│ reviewer │
└──────────┘     └────┬─────┘     └────┬─────┘
                      │                 │
                      ▼                 ▼
                ┌──────────┐     ┌──────────┐
                │  fixer   │◀────│  fixer   │
                └──────────┘     └──────────┘
                      │
                      ▼
                ┌──────────┐
                │ doc-gen  │
                └──────────┘

WorkflowConfig::Dynamic {
    initial_agent: "planner",
    max_handoffs: 10,
    agent_subset: ["planner", "coder", "reviewer", "fixer"],  // excludes doc-gen
}
```

The `agent_subset` acts as a filter: even though `doc-gen` is registered, it's invisible to this workflow. This lets you register many agents globally but activate different subsets for different tasks.

## Extension Patterns

### Defining an Agent with AgentDefinition

`AgentDefinition` is the blueprint for an agent. It specifies everything the runtime needs to create and run an agent instance:

```rust
pub struct AgentDefinition {
    /// Unique name used in handoff targets and logging
    pub name: String,

    /// Function that generates the system prompt given workspace context
    pub system_prompt_fn: Arc<dyn Fn(&WorkspaceContext) -> String + Send + Sync>,

    /// List of tool names this agent can use
    pub tool_set: Vec<String>,

    /// Maximum turns before the agent is force-stopped
    pub max_turns: usize,

    /// Which agents this agent is allowed to hand off to
    pub can_handoff_to: Vec<String>,
}
```

Each field has important design implications:

- **`name`**: Must be unique within the registry. Used in `HandoffTool` targets and conversation key generation.
- **`system_prompt_fn`**: A closure rather than a static string so the prompt can adapt to the workspace (e.g., include the project's language, framework, or coding conventions).
- **`tool_set`**: A whitelist of tool names. The agent can only use tools in this list. Tools not listed are invisible to the model, reducing confusion and preventing unauthorized actions.
- **`max_turns`**: A safety valve. Even if the agent loops, it will be stopped after this many turns. Choose values based on the agent's expected workload: a planner might need 5 turns, a coder might need 30.
- **`can_handoff_to`**: The outgoing edges of the agent's subgraph. An empty list means the agent is a terminal node (it cannot delegate).

### Registering Agents in AgentRegistry

The `AgentRegistry` is a simple name-to-definition map. Register agents before starting the workflow:

```rust
let mut registry = AgentRegistry::new();

registry.register(AgentDefinition {
    name: "security_auditor".into(),
    system_prompt_fn: Arc::new(|ctx| format!(
        "You are a security auditing agent. Analyze the codebase at {} for \
         vulnerabilities. Focus on: SQL injection, XSS, CSRF, insecure \
         deserialization, and path traversal. Report findings with severity \
         and remediation steps.",
        ctx.root.display()
    )),
    tool_set: vec![
        "read_file".into(), "list_files".into(), "shell".into(),
        "handoff".into(), "request_fix".into(),
    ],
    max_turns: 15,
    can_handoff_to: vec!["coder".into(), "fixer".into()],
})?;

registry.register(AgentDefinition {
    name: "doc_writer".into(),
    system_prompt_fn: Arc::new(|_| 
        "You are a documentation agent. Write clear, concise documentation \
         for the code you are given. Use markdown format. Include usage \
         examples and parameter descriptions.".into()
    ),
    tool_set: vec![
        "read_file".into(), "write_file".into(), "handoff".into(),
    ],
    max_turns: 10,
    can_handoff_to: vec!["reviewer".into()],
})?;
```

### Configuring a Dynamic Workflow

`WorkflowConfig::Dynamic` specifies which agents participate in a workflow and where it starts:

```rust
pub enum WorkflowConfig {
    Static { /* built-in pipeline */ },
    Dynamic {
        initial_agent: String,
        max_handoffs: usize,
        agent_subset: Vec<String>,
    },
}
```

- **`initial_agent`**: The first agent to run. Must be in `agent_subset`.
- **`max_handoffs`**: The handoff budget for this workflow. Overrides the default of 14.
- **`agent_subset`**: Which registered agents are visible in this workflow. Agents not in this list cannot be handed off to, even if they appear in `can_handoff_to`.

```rust
let config = WorkflowConfig::Dynamic {
    initial_agent: "security_auditor".into(),
    max_handoffs: 10,
    agent_subset: vec![
        "security_auditor".into(),
        "coder".into(),
        "fixer".into(),
    ],
};
```

In this configuration, even if the coder's `can_handoff_to` includes "reviewer", the handoff will fail because "reviewer" is not in the `agent_subset`. This double-filtering (source permission + subset membership) provides fine-grained access control.

### Running the Workflow

```rust
let outcome = run_dynamic_handoff(
    &registry,
    &config,
    &workspace,
    &message_store,
    "Audit the authentication module for security vulnerabilities".into(),
).await?;

match outcome {
    AgentRunOutcome::Completed { final_agent, summary } => {
        println!("Workflow completed at agent '{}' with summary: {}", 
                 final_agent, summary);
    }
    AgentRunOutcome::BudgetExhausted { handoff_count } => {
        eprintln!("Workflow stopped: handoff budget ({}) exhausted", handoff_count);
    }
    AgentRunOutcome::Cancelled => {
        eprintln!("Workflow cancelled by user");
    }
    AgentRunOutcome::Error(e) => {
        eprintln!("Workflow error: {}", e);
    }
}
```

## Common Pitfalls

1. **Agents with overlapping responsibilities.** If two agents have nearly identical system prompts and tool sets, the model will be confused about which one to hand off to. Give each agent a distinct, well-defined role.

2. **Overly permissive `can_handoff_to` lists.** If every agent can hand off to every other agent, you lose the structure that makes multi-agent workflows valuable. Restrict handoffs to logical delegation paths.

3. **Forgetting to include "handoff" in `tool_set`.** If an agent's tool set doesn't include the `handoff` tool, it literally cannot delegate. This is useful for terminal agents but is a common oversight for intermediate agents.

4. **`initial_agent` not in `agent_subset`.** This is a configuration error that will be caught at startup, but it's easy to miss during development when you're rapidly changing the subset.

5. **`max_turns` too low for the agent's task.** A coder agent with `max_turns: 3` will be cut off before it can read the file, write the code, and run the tests. Estimate the minimum turns needed and add a safety margin.

6. **System prompt functions that return the same string regardless of context.** If your `system_prompt_fn` ignores the `WorkspaceContext` parameter, you might as well use a static string. The closure form exists specifically for context-aware prompting.

7. **Not handling the `BudgetExhausted` outcome.** When the handoff budget runs out, the workflow stops mid-stream. Your application must handle this gracefully—perhaps by reporting partial results or asking the user if they want to continue with a higher budget.

## Invariants

- **Agent names are globally unique within the registry.** Registering a duplicate name returns an error.
- **`agent_subset` must contain `initial_agent`.** This is validated at configuration time.
- **Handoff targets must be in both `can_handoff_to` and `agent_subset`.** A handoff fails if either check fails.
- **`max_handoffs` is an absolute limit per workflow run.** It cannot be increased mid-run.
- **Each agent in the workflow gets its own conversation key.** Message histories are never shared between agents, even if they use the same underlying LLM model.
- **The `handoff` tool must be in an agent's `tool_set` for it to delegate.** Without it, the agent cannot call `HandoffTool`.

## Examples

### Full Custom Workflow: Security Audit Pipeline

```rust
// 1. Define agents
let registry = AgentRegistry::from_iter(vec![
    AgentDefinition {
        name: "security_scanner".into(),
        system_prompt_fn: Arc::new(|ctx| format!(
            "Scan the project at {} for known vulnerability patterns. \
             Use grep and file reading to identify suspicious code. \
             Hand off to the remediation agent with a prioritized list.",
            ctx.root.display()
        )),
        tool_set: vec!["read_file".into(), "shell".into(), "handoff".into()],
        max_turns: 10,
        can_handoff_to: vec!["remediator".into()],
    },
    AgentDefinition {
        name: "remediator".into(),
        system_prompt_fn: Arc::new(|_| 
            "You fix security vulnerabilities. Apply the recommended fixes \
             and run tests to verify nothing is broken.".into()
        ),
        tool_set: vec![
            "read_file".into(), "write_file".into(), "shell".into(),
            "handoff".into(), "request_fix".into(),
        ],
        max_turns: 20,
        can_handoff_to: vec!["verifier".into(), "fixer".into()],
    },
    AgentDefinition {
        name: "verifier".into(),
        system_prompt_fn: Arc::new(|_| 
            "You verify that security fixes are correct and complete. \
             Re-scan the fixed code and confirm the vulnerability is resolved.".into()
        ),
        tool_set: vec!["read_file".into(), "shell".into(), "handoff".into()],
        max_turns: 5,
        can_handoff_to: vec!["remediator".into()],  // back for re-fix if needed
    },
]);

// 2. Configure workflow
let config = WorkflowConfig::Dynamic {
    initial_agent: "security_scanner".into(),
    max_handoffs: 8,
    agent_subset: vec![
        "security_scanner".into(),
        "remediator".into(),
        "verifier".into(),
        "fixer".into(),  // built-in fixer available as fallback
    ],
};

// 3. Run
let result = run_dynamic_handoff(
    &registry, &config, &workspace, &message_store,
    "Scan authentication code for OWASP Top 10 vulnerabilities".into(),
).await?;
```

### Minimal Two-Agent Workflow

For simpler needs, a two-agent setup is often sufficient:

```rust
let config = WorkflowConfig::Dynamic {
    initial_agent: "architect".into(),
    max_handoffs: 4,  // architect→coder→architect→coder at most
    agent_subset: vec!["architect".into(), "coder".into()],
};
```

This creates a tight loop where the architect designs and the coder implements, with at most two round-trips before the budget is exhausted. It's ideal for small, well-scoped tasks.
