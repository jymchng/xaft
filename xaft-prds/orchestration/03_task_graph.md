# Task Graph Execution

## DAG Execution Model

`xaft` models task execution as a directed acyclic graph (DAG) where nodes are `PlanStep`s and edges represent `depends_on` relationships.

```
        [step-1: index]
              │
    ┌─────────┴───────────┐
    │                     │
[step-2: edit auth]  [step-3: edit api]    ← parallel (no shared files)
    │                     │
    └─────────┬───────────┘
              │
        [step-4: run tests]
              │
    ┌─────────┴───────────┐
    │                     │
[success: commit]   [failure: fixer]
```

## DAG Scheduler

```rust
pub struct DagScheduler {
    steps: HashMap<String, XaftPlanStep>,
    completed: HashSet<String>,
    in_flight: HashSet<String>,
}

impl DagScheduler {
    /// Returns steps whose dependencies are all completed.
    pub fn ready_steps(&self) -> Vec<&XaftPlanStep> {
        self.steps.values()
            .filter(|step| {
                !self.completed.contains(&step.base.id)
                && !self.in_flight.contains(&step.base.id)
                && step.base.depends_on.iter().all(|dep| self.completed.contains(dep))
            })
            .collect()
    }

    pub fn mark_in_flight(&mut self, step_id: &str) {
        self.in_flight.insert(step_id.to_string());
    }

    pub fn mark_complete(&mut self, step_id: &str) {
        self.in_flight.remove(step_id);
        self.completed.insert(step_id.to_string());
    }

    pub fn is_complete(&self) -> bool {
        self.completed.len() == self.steps.len()
    }
}
```

## Execution Loop

```rust
pub async fn execute_dag(
    scheduler: &mut DagScheduler,
    session: &XaftSession,
    ui_tx: mpsc::Sender<UiEvent>,
) -> Result<(), XaftError> {
    loop {
        if scheduler.is_complete() { break; }

        let ready = scheduler.ready_steps();
        if ready.is_empty() {
            // Wait for in-flight steps to complete
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        // Partition by parallelizability
        let (parallel, sequential): (Vec<_>, Vec<_>) = ready.iter()
            .partition(|s| s.parallelizable);

        if parallel.len() > 1 {
            // Execute non-conflicting steps in parallel
            let handles: Vec<_> = parallel.iter().map(|step| {
                scheduler.mark_in_flight(&step.base.id);
                let step = (*step).clone();
                let session = Arc::clone(session);
                let tx = ui_tx.clone();
                tokio::spawn(async move {
                    execute_step(&step, &session, tx).await
                })
            }).collect();

            for (handle, step) in handles.into_iter().zip(parallel.iter()) {
                match handle.await? {
                    Ok(_) => scheduler.mark_complete(&step.base.id),
                    Err(e) => return Err(e),
                }
            }
        } else {
            // Execute first ready step sequentially
            let step = ready[0];
            scheduler.mark_in_flight(&step.base.id);
            execute_step(step, session, ui_tx.clone()).await?;
            scheduler.mark_complete(&step.base.id);
        }
    }

    Ok(())
}
```

## Checkpoint Integration

After every step completion (or per `CheckpointPolicy`):

```rust
async fn save_step_checkpoint(
    session: &XaftSession,
    step: &XaftPlanStep,
    result: &StepResult,
) -> Result<(), XaftError> {
    let checkpoint = Checkpoint {
        checkpoint_id: Uuid::new_v4(),
        task_id: session.current_task_id(),
        session_id: session.session_id,
        step_index: step.base.sequence,
        completed_steps: session.completed_steps().await,
        worktree_path: session.active_worktree_path().await,
        worktree_branch: session.active_branch().await,
        conversation_snapshot: session.conversation_snapshot().await,
        context_state: session.context_state_snapshot().await,
        saved_at: Utc::now(),
    };

    session.task_runner.save_checkpoint(checkpoint).await?;
    session.signal_bus.emit(CheckpointSaved {
        task_id: session.current_task_id(),
        step: step.base.sequence,
    }).await;

    Ok(())
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs`
- agtrs: `agtrs-graph/src/validate.rs`
- Next: [Agent Handoffs →](04_agent_handoffs.md)
