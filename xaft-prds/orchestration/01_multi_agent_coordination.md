# Multi-Agent Coordination

## Coordination Patterns in xaft

`xaft` uses three coordination patterns from `agtrs-runtime`, selected automatically based on task structure.

### Pattern 1: Sequential Handoff (default)

For tasks where each step depends on the previous result:

```
PlannerAgent → intent_to_plan
CodeAgent    → implement step 1
CodeAgent    → implement step 2
FixerAgent   → fix failures
ReviewAgent  → review diff
CodeAgent    → incorporate review
```

Implemented via `HandoffOrchestrator`:

```rust
let orchestrator = HandoffOrchestratorBuilder::new()
    .add_agent("planner",  Arc::new(PlannerAgent::new(llm.clone())))
    .add_agent("code",     Arc::new(CodeAgent::new(llm.clone(), workspace.clone())))
    .add_agent("fixer",    Arc::new(FixerAgent::new(llm.clone(), shell.clone())))
    .add_agent("reviewer", Arc::new(ReviewAgent::new(cheap_llm.clone())))
    .with_store(Arc::new(HandoffAgentStore::default()))
    .with_coordinator_prompt(xaft_handoff_prompt())
    .build();
```

### Pattern 2: Parallel Subagent Delegation

For tasks with independent subtasks (no file conflicts):

```
CodeAgent (orchestrator)
├── SubagentTool("migrate_auth")    → CodeAgent instance A (worktree A)
├── SubagentTool("add_logging")     → CodeAgent instance B (worktree B)
└── SubagentTool("update_tests")    → CodeAgent instance C (worktree C)
                                       ↓ all complete
                               merge_worktrees(A, B, C)
```

### Pattern 3: Team Coordinator

For complex tasks where a coordinator routes subtasks to specialists:

```rust
#[team(name = "engineering_team", mode = "coordinator", max_rounds = 10)]
#[injectable]
pub struct EngineeringTeam {
    code_agent:   Inject<CodeAgent>,
    fixer_agent:  Inject<FixerAgent>,
    index_agent:  Inject<IndexAgent>,
    review_agent: Inject<ReviewAgent>,
}
```

## AgentMessageBus Coordination

Agents communicate via typed messages without going through the orchestrator:

```rust
// CodeAgent sends review request to ReviewAgent mid-execution
pub async fn request_inline_review(
    &self,
    diff: &str,
    ctx: &ToolContext,
) -> Result<ReviewResult, AgtrsError> {
    let bus = ctx.message_bus().expect("bus required");
    let cid = Uuid::new_v4().to_string();
    let mut rx = bus.subscribe("code_agent").await;

    bus.send(AgentMessage::builder("code_agent", AgentMessageType::Query)
        .to("review_agent")
        .correlation_id(cid.clone())
        .payload(serde_json::json!({"diff": diff, "context": "auth_migration"}))
        .build()).await?;

    let response = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(msg) = rx.recv().await {
                if msg.correlation_id.as_deref() == Some(&cid) {
                    return msg;
                }
            }
        }
    }).await.map_err(|_| AgtrsError::msg("review timeout"))?;

    serde_json::from_value(response.payload.unwrap_or_default())
        .map_err(|e| AgtrsError::Other(e.to_string()))
}
```

## Conflict-Aware Parallelism

Before spawning parallel agents, `PlanExecutor` performs static analysis:

```rust
pub fn batch_non_conflicting(steps: &[PlanStep]) -> Vec<Vec<PlanStep>> {
    let mut batches: Vec<Vec<PlanStep>> = Vec::new();
    let mut current_batch: Vec<PlanStep> = Vec::new();
    let mut used_files: HashSet<String> = HashSet::new();

    for step in steps {
        let step_files: HashSet<String> = step.target_files.iter().cloned().collect();

        if step_files.is_disjoint(&used_files) && !step.depends_on.iter().any(|dep| {
            current_batch.iter().any(|s| s.id == *dep)
        }) {
            // Can run in parallel with current batch
            current_batch.push(step.clone());
            used_files.extend(step_files);
        } else {
            // Flush current batch, start new one
            if !current_batch.is_empty() {
                batches.push(current_batch.drain(..).collect());
                used_files.clear();
            }
            current_batch.push(step.clone());
            used_files.extend(step_files);
        }
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}
```

## Agent Communication Protocol

```rust
/// Standard xaft inter-agent message format
pub struct XaftAgentMessage {
    pub from: String,
    pub to: Option<String>,
    pub message_type: XaftMessageType,
    pub correlation_id: Option<String>,
    pub session_id: Uuid,
    pub task_id: Option<Uuid>,
    pub step_id: Option<String>,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

pub enum XaftMessageType {
    ReviewRequest,
    ReviewResponse,
    FixRequest,
    FixResponse,
    StatusUpdate,
    PlanRevisionRequest,
    ApprovalRequest,
    ApprovalResponse,
    Custom(String),
}
```

## References

- agtrs: `agtrs-runtime/src/team.rs`, `agtrs-runtime/src/messaging.rs`
- agtrs: `agtrs-runtime/src/subagent.rs`
- Next: [Planning System →](02_planning_system.md)
