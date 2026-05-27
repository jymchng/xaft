# Modifying Orchestration

## Purpose

The orchestration layer is the brain of the xaft runtime—it decides which agent runs, when it hands off to another agent, and how context flows between them. Modifying this layer is the highest-impact change you can make: a bug here can cause infinite handoff loops, lost conversation context, or budget overruns. This document explains how the `HandoffOrchestrator` works, what constraints it operates under, and what you must understand before modifying it. Read this document before changing any code in the orchestration module.

## Mental Model

Think of the `HandoffOrchestrator` as a relay race coordinator. Each agent is a runner. The orchestrator gives the baton (conversation context) to the first runner (the planner), who decides which runner should go next (the editor or the reviewer). The baton is passed via the conversation store—the outgoing agent's messages become the incoming agent's context. The `prompt_fn` is the coach's pep talk: it generates a system prompt for each agent based on the current state (which agent just finished, what the goal is, how many handoffs remain). The `max_handoffs` limit is the race budget: if the baton is passed too many times, the race is stopped (to prevent infinite loops). The `post-orchestration parsing` is the finish line: the orchestrator reads the final agent's output for specific markers ("APPROVED", `EditSummary`) to determine the outcome.

## Extension Patterns

When adding a new agent to the orchestration, update the `prompt_fn` to recognize the new agent's role and generate an appropriate system prompt. When modifying handoff logic, ensure the new logic respects `max_handoffs` and does not create cycles (Agent A → Agent B → Agent A → ...). When adding a new post-orchestration marker (e.g., "NEEDS_REVIEW"), update the parsing logic in the orchestrator's completion handler. When modifying the conversation store integration, ensure that `agent_store` and `conv_store` are shared across all agents in the workflow (each agent reads from the same conversation history). When adding a new workflow, configure `WorkflowConfig` with the agent sequence, handoff rules, and conversation key format.

## Common Pitfalls

- **Exceeding `max_handoffs` without understanding budget implications**: Each handoff involves a full LLM call (the receiving agent reads the conversation and generates a response), which costs tokens and time. If `max_handoffs` is set too high, a degenerate workflow can burn through the cost limit. If it's set too low, legitimate multi-step workflows are cut short. Choose the limit based on the expected workflow complexity.
- **Not sharing `agent_store` and `conv_store` across agents**: If each agent has its own conversation store, handoffs lose context—the receiving agent doesn't know what the sending agent did. The stores must be shared (via `Arc`) so that every agent in the workflow reads from and writes to the same history.
- **Duplicate conversation keys**: The conversation key identifies a unique conversation in the store. If two agents in the same workflow use the same key but different contexts, one will overwrite the other's messages. Keys must be unique per session + workflow (e.g., `session_123:workflow_plan-and-edit`).
- **Relying on "APPROVED" text without defensive parsing**: The orchestrator looks for "APPROVED" in the final agent's output to determine success. If the agent's prompt doesn't instruct it to output "APPROVED" on success, or if the output format changes, the parsing will fail silently. Always add defensive checks and fallback behavior.
- **Ignoring `EditSummary` conventions**: The `EditSummary` is a structured output that the editor agent produces to describe what it changed. If the post-orchestration parsing expects a specific format but the editor produces a different one, the summary is lost. Define the `EditSummary` format explicitly in the agent's system prompt.

## Invariants

1. The `HandoffOrchestrator` must respect `max_handoffs`. If the handoff count reaches the limit, the workflow must terminate with a clear error, not loop indefinitely.
2. `agent_store` and `conv_store` must be shared across all agents in the same workflow. Each agent must read the full conversation history before generating its response.
3. Conversation keys must be unique per session + workflow. The format is `<session_id>:<workflow_name>`.
4. `prompt_fn` must generate a complete system prompt for each agent. It must not return an empty string or rely on default prompts without explicit configuration.
5. Post-orchestration parsing must handle both "APPROVED" and `EditSummary` formats. If parsing fails, the orchestrator must treat the outcome as uncertain (not as approved).
6. Handoff cycles must be detected and prevented. If Agent A hands off to Agent B which hands off to Agent A, the orchestrator must detect the cycle and terminate.

## Examples

```rust
/// HandoffOrchestrator: manages agent handoffs within a workflow.
pub struct HandoffOrchestrator {
    agents: HashMap<String, Box<dyn Agent>>,
    agent_store: Arc<dyn AgentStore>,
    conv_store: Arc<dyn ConversationStore>,
    prompt_fn: Box<dyn Fn(&str, &OrchestrationState) -> String + Send + Sync>,
    max_handoffs: u32,
    workflow_name: String,
}

impl HandoffOrchestrator {
    pub async fn run(&self, session_id: &str, initial_agent: &str) -> Result<OrchestrationOutcome, AgtrsError> {
        let mut current_agent = initial_agent.to_string();
        let mut handoff_count = 0u32;
        let conv_key = format!("{session_id}:{}", self.workflow_name);

        loop {
            // Check handoff budget
            if handoff_count >= self.max_handoffs {
                tracing::warn!(
                    handoff_count,
                    max = self.max_handoffs,
                    "max handoffs exceeded, terminating workflow"
                );
                return Ok(OrchestrationOutcome::MaxHandoffsExceeded { handoff_count });
            }

            // Generate system prompt via prompt_fn
            let state = OrchestrationState {
                current_agent: current_agent.clone(),
                handoff_count,
                workflow_name: self.workflow_name.clone(),
            };
            let system_prompt = (self.prompt_fn)(&current_agent, &state);

            // Load shared conversation history
            let conversation = self.conv_store.get(&conv_key).await.unwrap_or_default();

            // Get the agent
            let agent = self.agents.get(&current_agent)
                .ok_or_else(|| AgtrsError::AgentNotFound { agent: current_agent.clone() })?;

            // Execute agent step
            let action = agent.step(conversation, &system_prompt).await?;

            match action {
                AgentAction::Handoff { agent: next_agent } => {
                    tracing::info!(
                        from = %current_agent,
                        to = %next_agent,
                        handoff = handoff_count + 1,
                        "agent handoff"
                    );
                    current_agent = next_agent;
                    handoff_count += 1;
                }
                AgentAction::Done => {
                    // Post-orchestration parsing
                    let outcome = self.parse_outcome(&conversation)?;
                    return Ok(outcome);
                }
                AgentAction::Error(e) => {
                    return Err(AgtrsError::AgentError {
                        agent: current_agent,
                        source: e.into(),
                    });
                }
            }
        }
    }

    fn parse_outcome(&self, conversation: &Conversation) -> Result<OrchestrationOutcome, AgtrsError> {
        let last_message = conversation.last_assistant_message()
            .ok_or(AgtrsError::OrchestrationError("no final message".into()))?;

        // Look for "APPROVED" marker
        if last_message.contains("APPROVED") {
            // Parse EditSummary if present
            let edit_summary = self.parse_edit_summary(last_message);
            return Ok(OrchestrationOutcome::Approved { edit_summary });
        }

        // Defensive: if no clear marker, treat as uncertain
        Ok(OrchestrationOutcome::Uncertain {
            message: last_message.clone(),
        })
    }

    fn parse_edit_summary(&self, message: &str) -> Option<EditSummary> {
        // Look for structured EditSummary block
        // Format: ```edit-summary\n{"files": [...], "description": "..."}\n```
        let start = message.find("```edit-summary")?;
        let end = message[start..].find("```")?;
        let json = &message[start + 15..start + end];
        serde_json::from_str(json).ok()
    }
}

/// Workflow configuration
#[derive(Debug, Deserialize)]
pub struct WorkflowConfig {
    pub name: String,
    pub initial_agent: String,
    pub max_handoffs: u32,           // default: 10
    pub agent_sequence: Vec<String>, // ordered list of agent names
}

/// Conversation key format: <session_id>:<workflow_name>
fn conv_key(session_id: &str, workflow_name: &str) -> String {
    format!("{session_id}:{workflow_name}")
}

/// Example prompt_fn that generates context-specific prompts
fn planner_prompt_fn(agent_name: &str, state: &OrchestrationState) -> String {
    match agent_name {
        "planner" => format!(
            "You are the planner agent. Analyze the task and decide which specialist agent should handle it. \
             Available agents: editor, reviewer. Handoffs used: {}/{}. \
             Output 'HANDOFF: editor' or 'HANDOFF: reviewer' to delegate, or 'DONE' if the task is complete.",
            state.handoff_count, state.max_handoffs
        ),
        "editor" => format!(
            "You are the editor agent. Make the requested changes and output 'APPROVED' when done. \
             Include an edit-summary block with the list of changed files. \
             Handoffs used: {}/{}.",
            state.handoff_count, state.max_handoffs
        ),
        "reviewer" => format!(
            "You are the reviewer agent. Review the changes made by the editor. \
             Output 'APPROVED' if the changes are correct, or 'HANDOFF: editor' if revisions are needed. \
             Handoffs used: {}/{}.",
            state.handoff_count, state.max_handoffs
        ),
        _ => "You are a general-purpose agent. Complete the task and output 'DONE' when finished.".to_string(),
    }
}
```
