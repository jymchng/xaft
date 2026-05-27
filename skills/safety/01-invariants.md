# Safety Invariants

## Purpose

Safety invariants are the rules that must never be broken—violating any one of them can result in data loss, security breaches, or corrupted state that is unrecoverable. Unlike conventions (which are best practices), invariants are hard constraints that every code path must uphold, even under error conditions, cancellation, or concurrent access. This document enumerates the invariants that the xaft runtime depends on, explains why each exists, and specifies what must happen if an invariant is at risk of being violated. When reviewing code, treat every invariant as a check point: does this change preserve the invariant?

## Mental Model

Think of invariants as the load-bearing walls of the system. You can rearrange the furniture (refactor internal logic), repaint the walls (change log formats), or add new rooms (new features), but you must never remove a load-bearing wall without replacing it with an equivalent structure. Each invariant exists because a previous failure (or near-miss) demonstrated that the system could not operate correctly without it. For example, path traversal protection exists because the LLM might generate a `read_file` call with `../../../etc/passwd`, and without the check, the tool would happily read it. Git worktree isolation exists because modifying HEAD directly would corrupt the user's repository. Cost accumulation accuracy exists because exceeding a cost limit without stopping is a financial liability.

## Extension Patterns

When adding a new file tool, you must apply path traversal protection by resolving the path against `workspace_root` and verifying that the resolved path starts with the workspace root. When adding a new git operation, you must ensure it operates on the worktree (not HEAD) and that the worktree is restored on error or cancellation. When adding a new cost-generating operation (e.g., a new provider), you must subscribe the cost tracker before the first LLM call and verify that the accumulated cost is checked before each subsequent call. When adding a new session state transition, you must verify that the transition is valid (e.g., "running" → "completed" is valid, "completed" → "running" is not). When adding a new dangerous operation, you must ensure it goes through the approval gate and that the gate requires explicit opt-in (not auto-approve by default). When adding a new config override, you must ensure that `null` values preserve the base (never clear).

## Common Pitfalls

- **Assuming the LLM will never generate malicious tool calls**: The LLM is an untrusted input source. It may generate `read_file("../../../etc/passwd")` or `bash_exec("rm -rf /")`. Every tool must validate its inputs against the workspace boundary.
- **Modifying HEAD directly in git operations**: Even "harmless" operations like `git checkout` on HEAD can leave the user's repository in a detached HEAD state. Always use worktrees.
- **Subscribing to the cost tracker after the first LLM call**: If the first LLM call costs $5 and the limit is $3, the cost limit is already exceeded before the tracker starts counting. Subscribe before any calls.
- **Auto-approving dangerous operations for convenience**: A bash execution tool that auto-approves `rm` commands is a security hole. Dangerous operations must require explicit opt-in, even if it's annoying.
- **Clearing config values with `null`**: If a project config sets `cost_limit: null` intending to "not override," but the merge treats null as "clear," the cost limit disappears and the system has no spending cap.

## Invariants

1. **Path traversal protection on ALL file tools**: Every file tool must resolve the target path against `workspace_root` using `canonicalize` or `Path::starts_with`. If the resolved path escapes the workspace root, the tool must return a soft error and must not access the file.

2. **Git worktree isolation**: All git operations must operate on a worktree, never on HEAD directly. The worktree must be created before the operation and restored (cleaned up) on completion, error, or cancellation. Modifying HEAD is only allowed in explicitly user-initiated operations (e.g., `git commit` with approval).

3. **Cost accumulation must be accurate**: The cost tracker must be subscribed before the first LLM call. The accumulated cost must be checked before each subsequent call. If the cost limit is exceeded, the operation must be aborted before the call is made, not after.

4. **Session status transitions must be valid**: The session state machine is: `Created → Running → (Completed | Failed | Cancelled)`. Transitions out of terminal states (`Completed`, `Failed`, `Cancelled`) are forbidden. The session status must always reflect the actual state of the session.

5. **Approval gates must never auto-approve dangerous operations without explicit opt-in**: Operations that modify the filesystem (`write_file`, `bash_exec`), modify git state (`git_commit`), or make network requests (`web_fetch`) must require approval unless the user has explicitly opted in via guardrail config. Auto-approve is only safe for read-only operations (`read_file`, `list_directory`).

6. **Null config values must preserve base (never clear)**: In the deep merge of configuration layers, `Value::Null` in the override must preserve the base value. There is no mechanism to "clear" a config value—this is intentional, as clearing would break the layering model.

## Examples

```rust
// Invariant 1: Path traversal protection
impl ReadFileTool {
    fn validate_path(&self, path: &Path) -> Result<PathBuf, AgtrsError> {
        let resolved = self.workspace_root.join(path).canonicalize()
            .map_err(|_| AgtrsError::PathNotFound { path: path.to_path_buf() })?;
        if !resolved.starts_with(&self.workspace_root) {
            return Err(AgtrsError::PathTraversalBlocked {
                attempted: path.to_path_buf(),
                boundary: self.workspace_root.clone(),
            });
        }
        Ok(resolved)
    }
}

// Invariant 2: Git worktree isolation
impl GitOps {
    async fn with_worktree<F, T>(&self, f: F) -> Result<T, GitOpsError>
    where
        F: FnOnce(&Path) -> Result<T, GitOpsError>,
    {
        let worktree = self.create_worktree()?;
        let result = tokio::select! {
            r = std::future::ready(f(worktree.path())) => r,
            _ = self.cancel.cancelled() => {
                self.restore_worktree(&worktree)?;
                return Err(GitOpsError::Cancelled);
            }
        };
        self.restore_worktree(&worktree)?;
        result
    }
}

// Invariant 3: Cost accumulation before first call
impl CostedProvider {
    pub fn new(inner: Box<dyn LlmProvider>, cost_tracker: Arc<CostTracker>) -> Self {
        // Subscribe IMMEDIATELY - before any calls can be made
        cost_tracker.subscribe();
        Self { inner, cost_tracker }
    }

    async fn complete(&self, request: Request) -> Result<Response, ProviderError> {
        // Check cost limit BEFORE the call
        if self.cost_tracker.is_over_limit() {
            return Err(ProviderError::CostLimitExceeded);
        }
        let response = self.inner.complete(request).await?;
        self.cost_tracker.record(response.usage.clone()).await;
        Ok(response)
    }
}

// Invariant 4: Session status transitions
impl Session {
    pub fn transition(&mut self, new_status: SessionStatus) -> Result<(), SessionError> {
        match (&self.status, &new_status) {
            (SessionStatus::Created, SessionStatus::Running) => {},
            (SessionStatus::Running, SessionStatus::Completed) => {},
            (SessionStatus::Running, SessionStatus::Failed) => {},
            (SessionStatus::Running, SessionStatus::Cancelled) => {},
            (current, _) => {
                return Err(SessionError::InvalidTransition {
                    from: *current,
                    to: new_status,
                });
            }
        }
        self.status = new_status;
        Ok(())
    }
}

// Invariant 5: Approval gate for dangerous operations
impl ToolRegistry {
    fn requires_approval(&self, tool_name: &str) -> bool {
        match tool_name {
            "read_file" | "list_directory" => self.guardrail.auto_approve_reads,
            "write_file" | "bash_exec" | "git_commit" => false, // NEVER auto-approve
            _ => false,
        }
    }
}
```
