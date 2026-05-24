# Agent Handoffs

## Handoff Protocol

When one agent transfers control to another, the `HandoffContext` carries sufficient state for the receiving agent to continue without repeating work.

```rust
pub struct XaftHandoffContext {
    /// Previous agent name
    pub from_agent: String,
    /// Receiving agent name
    pub to_agent: String,
    /// Human-readable summary of work done so far
    pub summary: String,
    /// Files modified so far (in worktree)
    pub modified_files: Vec<PathBuf>,
    /// Current git diff (condensed)
    pub current_diff_summary: String,
    /// Any structured artifacts from the previous agent
    pub artifacts: HashMap<String, serde_json::Value>,
    /// Reason for handoff
    pub reason: HandoffReason,
}

pub enum HandoffReason {
    StepComplete,           // normal transition
    TestFailure { error: String },  // CodeAgent → FixerAgent
    ReviewRequested,        // CodeAgent → ReviewAgent
    OutOfContext,           // agent hit token limit
    CostLimit,              // agent hit cost limit
}
```

## CodeAgent → FixerAgent Handoff

```rust
// In PlanExecutor, after CodeAgent step:
let test_result = shell.run("cargo test --workspace 2>&1", None).await?;

if test_result.exit_code != 0 {
    let handoff = XaftHandoffContext {
        from_agent: "code".into(),
        to_agent: "fixer".into(),
        summary: code_response.content.clone(),
        modified_files: workspace.list_modified().await?,
        current_diff_summary: git.diff_summary(worktree).await?,
        artifacts: HashMap::from([
            ("test_error".into(), serde_json::json!(test_result.stderr)),
            ("test_stdout".into(), serde_json::json!(test_result.stdout)),
        ]),
        reason: HandoffReason::TestFailure { error: test_result.stderr.clone() },
    };

    handoff_store.set_active_agent(&session_id, "fixer").await;
    handoff_store.set_pending_context(&session_id, &handoff).await;

    // Run FixerAgent with context
    let mut fixer_ctx = build_agent_context("fixer", &session);
    inject_handoff_context(&mut fixer_ctx, &handoff);
    AgentExecutor::run(&fixer_agent, Message::user(
        format!("Fix these test failures:\n```\n{}\n```", test_result.stderr)
    ), &mut fixer_ctx).await?;
}
```

## Context Window Preservation

When handing off between agents, the conversation history is condensed:

```rust
fn build_handoff_system_message(handoff: &XaftHandoffContext) -> Message {
    Message::system(format!(
        r#"You are continuing work started by the {} agent.

## Summary of Work Done
{}

## Modified Files
{}

## Current Diff (condensed)
```diff
{}
```

## Your Task
{}
"#,
        handoff.from_agent,
        handoff.summary,
        handoff.modified_files.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n"),
        handoff.current_diff_summary,
        match &handoff.reason {
            HandoffReason::TestFailure { error } => format!("Fix these errors:\n```\n{error}\n```"),
            HandoffReason::ReviewRequested => "Review the above changes for correctness.".into(),
            _ => "Continue the task.".into(),
        }
    ))
}
```

## References

- agtrs: `agtrs-runtime/src/team.rs` (HandoffOrchestrator, HandoffAgentStore)
- agtrs guide: `guides/14-team-and-handoff.md`
