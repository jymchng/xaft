# Cancellation Design

## Purpose

Cancellation is not optional in xaft—it is a core user expectation. When a user presses Ctrl+C, the system must stop what it's doing, clean up any partial state (git worktrees, session status, file modifications), and exit gracefully. A half-cancelled system is worse than no cancellation: it leaves behind locked worktrees, orphaned sessions, and corrupted state. This document specifies the three-level cancellation architecture that ensures Ctrl+C propagates from the terminal through every concurrent task, that pending approvals are resolved (not left dangling), and that cleanup is guaranteed before exit.

## Mental Model

Think of cancellation as a wave that starts at the terminal and propagates inward through three concentric layers. The outer layer is the `CancellationToken` in the `EventLoop`—when `should_quit` fires (from Ctrl+C or a TUI quit command), it calls `cancel.cancel()`, which every spawned task checks via `cancel.cancelled()`. The middle layer is `ApprovalGate.cancel_all()`—when cancellation arrives, any pending approval requests (waiting in oneshot channels for the user to say yes/no) must be resolved with `false` so the requesting task doesn't hang forever. The inner layer is `AgentError::is_cancelled()`—when an agent detects that its tool call was cancelled, it must return a cancellation error, not a generic failure, so the runtime knows to shut down rather than retry. The wave must reach all three layers before cleanup begins.

## Extension Patterns

When adding a new spawned task, pass a cloned `CancellationToken` and check it in the task's main loop using `tokio::select!{biased; _ = cancel.cancelled() => break, ...}`. When adding a new approval gate interaction, ensure the gate's `cancel_all()` method will resolve the oneshot channel with `false`. When adding a new error type that can result from cancellation, implement `is_cancelled()` on it and ensure the method is called by the agent loop to distinguish cancellation from other errors. When adding a new resource that needs cleanup on cancellation (e.g., a temporary file, a git branch), register the cleanup in the `EventLoop`'s shutdown handler so it runs even if the task was cancelled mid-operation.

## Common Pitfalls

- **Checking `CancellationToken` after `.await` but not before**: If a task does `let result = tool.execute().await; if cancel.is_cancelled() { ... }`, it executes the entire tool call before checking cancellation. Always use `tokio::select!` to race the operation against `cancel.cancelled()`.
- **Forgetting to call `cancel_all()` on the approval gate**: If you cancel the `CancellationToken` but don't call `cancel_all()`, any task waiting on an approval response will hang forever (the oneshot sender was never invoked). Always call both: `cancel.cancel(); approval_gate.cancel_all();`.
- **Treating cancellation as a regular error**: If the agent loop catches a cancellation error and retries, it will enter an infinite loop (the token is still cancelled). Always check `is_cancelled()` and exit the loop immediately.
- **Not cleaning up on cancellation**: If a tool creates a git worktree and is cancelled mid-operation, the worktree remains. The cleanup handler must restore the worktree regardless of how the operation was interrupted.
- **Cancelling without setting session status**: If the session status is left as "running" after cancellation, the user cannot restart the session. Always set the status to "cancelled" or "interrupted" in the cleanup handler.

## Invariants

1. Cancellation must propagate through all three levels: `CancellationToken` → `ApprovalGate.cancel_all()` → `AgentError::is_cancelled()`.
2. `should_quit` (triggered by Ctrl+C or TUI quit) must call `cancel.cancel()` and then `approval_gate.cancel_all()` before awaiting task shutdown.
3. Every spawned task must receive a `CancellationToken` and must exit when `cancel.cancelled()` completes.
4. The agent loop must check `is_cancelled()` after every tool call and must not retry on cancellation.
5. Cleanup must be guaranteed: session status must be set to "cancelled" or "interrupted", git worktrees must be restored, and temporary files must be removed.
6. `ApprovalGate.cancel_all()` must resolve all pending oneshot channels with `false` to prevent hanging tasks.
7. Cancellation must be idempotent: calling `cancel.cancel()` multiple times must be safe.

## Examples

```rust
// Three-level cancellation in EventLoop
impl EventLoop {
    pub async fn run(&mut self) -> Result<(), RuntimeError> {
        loop {
            tokio::select! { biased;
                _ = self.cancel.cancelled() => {
                    tracing::info!("cancellation received, shutting down");
                    self.approval_gate.cancel_all().await;
                    self.cleanup().await?;
                    break;
                }
                event = self.event_receiver.recv() => {
                    if let Some(event) = event {
                        self.handle_event(event).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), RuntimeError> {
        // Set session status
        self.session_store.set_status(self.session_id, SessionStatus::Cancelled).await?;
        // Restore git worktree
        self.git_ops.restore_worktree(&self.workspace_root).await?;
        // Remove temporary files
        if let Some(temp_dir) = &self.temp_dir {
            tokio::fs::remove_dir_all(temp_dir).await.ok();
        }
        tracing::info!("cleanup complete");
        Ok(())
    }
}

// Agent loop with cancellation detection
impl Agent {
    pub async fn run_loop(&mut self, cancel: CancellationToken) -> Result<(), AgtrsError> {
        loop {
            let result = tokio::select! { biased;
                _ = cancel.cancelled() => {
                    tracing::info!("agent loop cancelled");
                    return Err(AgtrsError::Cancelled);
                }
                result = self.step() => result?,
            };

            if result.is_cancelled() {
                tracing::info!("tool call was cancelled, exiting agent loop");
                return Err(AgtrsError::Cancelled);
            }

            if result.is_error && !result.is_cancelled() {
                tracing::warn!(tool = %result.tool_name, "tool failed, may retry");
                // Agent can retry soft errors
            }

            if matches!(result.action, AgentAction::Done) {
                break;
            }
        }
        Ok(())
    }
}

// ApprovalGate cancel_all
impl ApprovalGate for TuiApprovalGate {
    async fn cancel_all(&self) {
        let mut pending = self.pending_approvals.lock().await;
        for (_, response_tx) in pending.drain() {
            // Resolve with false so the requesting task doesn't hang
            let _ = response_tx.send(false);
        }
    }
}

// is_cancelled convention
impl AgtrsError {
    pub fn is_cancelled(&self) -> bool {
        match self {
            AgtrsError::Cancelled => true,
            AgtrsError::ToolFailed { source, .. } => source.is_cancelled(),
            AgtrsError::AgentError { source, .. } => {
                if let Some(cancelled) = source.downcast_ref::<AgtrsError>() {
                    cancelled.is_cancelled()
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}
```
