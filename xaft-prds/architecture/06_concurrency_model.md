# Concurrency Model

## Runtime Foundation

`xaft` uses Tokio as its sole async runtime with `features = ["full"]`. The `#[tokio::main]` macro starts a multi-threaded Tokio runtime on all available cores.

```rust
#[tokio::main]
async fn main() -> Result<(), XaftError> {
    let args = Cli::parse();
    let config = XaftConfig::load(&args)?;
    init_tracing(&config);
    xaft_core::run(args, config).await
}
```

**No mixing of async runtimes.** All async code in `xaft` and `agtrs` assumes Tokio.

## Task Taxonomy

| Task type | Mechanism | Cancellation |
|---|---|---|
| Agent execution | `tokio::spawn` + `JoinSet` | `CancellationToken::child_token()` |
| TUI render loop | `tokio::spawn` | Dropped on `JoinSet::abort_all()` |
| Keyboard reader | `tokio::spawn` | Dropped on `JoinSet::abort_all()` |
| Shell command | `tokio::process::Command` + `spawn()` | `child.kill()` on cancel |
| LLM HTTP call | `reqwest` async + `select!` | `cancel_token.cancelled()` races HTTP future |
| File watcher | `tokio::spawn` + `notify` async | Dropped on join |
| SignalBus consumer | `tokio::spawn` (one per subscription) | Broadcast channel closed on bus drop |
| Audit log writer | `tokio::spawn` | Flushed before exit |

## Structured Concurrency with JoinSet

```rust
pub async fn run_session(session: Arc<XaftSession>, intent: Intent) -> Result<(), XaftError> {
    let mut tasks = JoinSet::new();
    let root_token = session.root_cancel.clone();

    // Spawn background tasks
    tasks.spawn(run_tui_render_loop(Arc::clone(&session)));
    tasks.spawn(run_keyboard_reader(Arc::clone(&session)));
    tasks.spawn(run_audit_log_writer(Arc::clone(&session)));
    tasks.spawn(run_metrics_emitter(Arc::clone(&session)));

    // Main orchestration task
    let result = run_orchestration(Arc::clone(&session), intent).await;

    // Signal all tasks to stop
    root_token.cancel();

    // Wait for all tasks with timeout
    let shutdown_deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(shutdown_deadline);

    loop {
        tokio::select! {
            Some(join_result) = tasks.join_next() => {
                if let Err(e) = join_result {
                    tracing::error!("background task panicked: {e:?}");
                }
                if tasks.is_empty() { break; }
            }
            _ = &mut shutdown_deadline => {
                tracing::warn!("shutdown timeout — aborting remaining tasks");
                tasks.abort_all();
                break;
            }
        }
    }

    result
}
```

## CancellationToken Hierarchy

```
root_token                    ← fires on Ctrl-C or fatal error
├── orchestration_token       ← fires when orchestration completes/fails
│   ├── plan_executor_token   ← per-plan execution
│   │   ├── step_0_token      ← per-step agent run
│   │   │   └── tool_*_token  ← per-tool call (propagated into ToolContext)
│   │   └── step_1_token
│   └── subagent_token        ← subagent isolated runs
├── tui_token                 ← killed after orchestration completes
└── audit_token               ← killed after audit flush
```

Cancellation fires **downward** only. A parent token firing cancels all children.

## Parallel Agent Execution

When the plan contains independent steps, `PlanExecutor` runs them concurrently in separate worktrees:

```rust
pub async fn execute_parallel_steps(
    steps: &[PlanStep],
    session: &XaftSession,
) -> Result<Vec<StepResult>, XaftError> {
    // Conflict analysis: group into non-conflicting batches
    let batches = plan_executor::batch_non_conflicting(steps);

    let mut all_results = Vec::new();

    for batch in batches {
        // All steps in a batch can run concurrently
        let mut handles = JoinSet::new();

        for step in batch {
            let session = Arc::clone(session);
            let step = step.clone();
            handles.spawn(async move {
                // Create isolated worktree for this step
                let wt = session.worktree_manager
                    .create_for_task(step.task_id, "main").await?;

                let result = execute_single_step(&step, &wt, &session).await;

                // Cleanup worktree
                session.worktree_manager.remove(&wt, result.is_ok()).await?;

                result
            });
        }

        // Collect batch results
        while let Some(result) = handles.join_next().await {
            all_results.push(result??);
        }
    }

    Ok(all_results)
}
```

## Mutex / Lock Strategy

| Resource | Lock type | Rationale |
|---|---|---|
| `AppState` (TUI) | `tokio::sync::Mutex` | Held briefly; writers and reader on same executor |
| `RepoIndex.checksums` | `tokio::sync::RwLock` | Many readers (index query), few writers (file change) |
| `CostTracker.total` | `AtomicF64` | Hot path; no async needed |
| `active_worktree` | `tokio::sync::RwLock` | Read frequently, written on task start/end |
| `ToolContext.state` | `HashMap` (no lock) | Per-call, not shared |
| `ConversationStore` | `tokio::sync::Mutex` | Single writer per session |

**Rule**: Never hold any lock across an `.await` point. Clone data out, drop the lock, then await.

## Parallel Tool Calls Within an Agent

When `AgentConfig::parallel_tool_calls = true`, the executor runs all tool calls in a single turn concurrently:

```rust
// From agtrs-runtime/src/executor.rs (simplified)
let tool_futures: Vec<_> = tool_calls.iter().map(|call| {
    let tool = ctx.get_tool(&call.name).cloned();
    let input = call.input.clone();
    let tool_ctx = ToolContext::new(&call.tool_use_id)
        .with_cancel_token(run_token.clone())
        .with_turn(iteration);

    async move {
        let tool = tool.ok_or_else(|| AgtrsError::msg("tool not found"))?;
        tool.call(input, &tool_ctx).await
    }
}).collect();

let results = futures::future::join_all(tool_futures).await;
```

`CodeAgent` enables parallel tool calls because `read_file` calls on different files are independent.

## Spawn_blocking Policy

All operations that call synchronous OS APIs or are CPU-bound use `spawn_blocking`:

```rust
// Tree-sitter parsing (CPU-bound)
let symbols = tokio::task::spawn_blocking(move || {
    parse_file_sync(&content, &language)
}).await?;

// File hashing (CPU-bound for large files)
let hash = tokio::task::spawn_blocking(move || {
    sha256_file_sync(&path)
}).await?;

// Diff application (uses Myers algorithm, can be expensive)
let patch_result = tokio::task::spawn_blocking(move || {
    apply_patch_sync(&original, &diff)
}).await?;
```

**Policy**: Any operation exceeding ~100µs of CPU time should use `spawn_blocking`.

## References

- agtrs: `agtrs-runtime/src/executor.rs` (parallel tool handling)
- agtrs tests: `agtrs-runtime/tests/executor_parallel.rs`
- agtrs tests: `agtrs-runtime/tests/cancellation.rs`
- Next: [State Machines →](07_state_machines.md)