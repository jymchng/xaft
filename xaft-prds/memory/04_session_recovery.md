# Session Recovery

## Recovery Scenarios

| Scenario | Detection | Recovery action |
|---|---|---|
| Process crash | `xaft resume` command | Load checkpoint, recreate worktree, continue from last step |
| Ctrl-C | Signal handler | Save checkpoint, clean suspend |
| Budget exceeded | `BudgetExceeded` error | Save checkpoint, report remaining steps |
| Step failure (recoverable) | Test failure | Run FixerAgent, retry step |
| Step failure (unrecoverable) | Max retries exceeded | Save checkpoint, await user |
| Provider timeout | `LlmCallFailed` | Retry with exponential backoff (max 3 attempts) |

## Resume Command

```bash
$ xaft resume ses-abc123

Resuming session ses-abc123...
  Intent: "migrate auth to JWT"
  Last checkpoint: Step 3/7 (Edit src/auth.rs)
  Worktree: xaft-wt-abc123 (branch xaft/abc123)
  Modified files: src/auth.rs (staged)
  Cost so far: $0.042

Continue from step 3? [Y/n]
```

## Recovery Implementation

```rust
pub async fn resume_session(session_id: Uuid, config: &XaftConfig) -> Result<(), XaftError> {
    // 1. Load session snapshot
    let store = SqliteSessionStore::open(&config.session_db_path).await?;
    let snapshot = store.load_session(session_id).await?
        .ok_or_else(|| XaftError::Session(format!("session {session_id} not found")))?;

    // 2. Verify worktree still exists
    let repo = GitRepo::open(&config.project_root)?;
    let worktree_exists = repo.worktree_exists(&snapshot.worktree_branch.as_deref().unwrap_or("")).await;

    if !worktree_exists {
        // Worktree was removed — recreate from base
        tracing::info!("recreating worktree from base");
        let wt = repo.create_worktree_from_branch(
            &snapshot.worktree_branch.as_deref().unwrap_or("main"),
            "main",
        ).await?;
        // Re-apply committed changes from worktree branch
        repo.checkout_branch(&snapshot.worktree_branch.unwrap()).await?;
    }

    // 3. Load checkpoint
    let checkpoint = store.load_checkpoint(snapshot.current_task_id.unwrap()).await?
        .ok_or_else(|| XaftError::Session("no checkpoint found".into()))?;

    // 4. Reconstruct session
    let session = XaftSession::from_snapshot(snapshot, config).await?;

    // 5. Resume execution from checkpoint
    let plan_executor = PlanExecutor::new(Arc::clone(&session));
    plan_executor.resume_from_checkpoint(checkpoint).await?;

    Ok(())
}
```

## Exponential Backoff for LLM Retries

```rust
pub async fn llm_call_with_retry<F, T>(
    f: F,
    max_retries: u32,
    cancel_token: &CancellationToken,
) -> Result<T, AgtrsError>
where
    F: Fn() -> BoxFuture<'static, Result<T, AgtrsError>>,
{
    let mut attempt = 0;
    loop {
        tokio::select! {
            result = f() => {
                match result {
                    Ok(v) => return Ok(v),
                    Err(AgtrsError::LlmCallFailed(_)) if attempt < max_retries => {
                        attempt += 1;
                        let delay = Duration::from_millis(1000 * 2u64.pow(attempt));
                        tracing::warn!("LLM call failed, retrying in {}ms (attempt {}/{})", delay.as_millis(), attempt, max_retries);
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            _ = cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled { reason: "cancelled during retry".into() });
            }
        }
    }
}
```

## References

- agtrs: `agtrs-runtime/src/task.rs` (TaskState::Suspended, TaskRunner::resume)
- agtrs: `agtrs-store/src/backends/sqlite.rs`
