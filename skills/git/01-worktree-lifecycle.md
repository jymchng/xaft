# Worktree Lifecycle

## Purpose

The worktree lifecycle is xaft's primary safety mechanism for file system mutations. Before an agent edits any file, xaft creates an isolated git worktree—a lightweight clone of the repository where changes can be made without affecting the main working directory. If the agent's changes are correct, they are committed and merged back. If the agent fails, makes errors, or is cancelled, the worktree is discarded and the repository is restored to its original state. This document explains the complete lifecycle: from `GitRepo::open()` through `begin_worktree()`, the `WorktreeGuard` RAII pattern, commit and rollback semantics, signal emission, and the isolation guarantees that protect user code.

Understanding the worktree lifecycle is essential for tool authors who modify files, for anyone debugging unexpected file states, and for architects reasoning about xaft's safety guarantees in production environments where data integrity is paramount.

## Mental Model

Think of the worktree lifecycle as a **transaction on the filesystem**. Just as a database transaction groups related changes and commits or rolls them back atomically, the worktree groups all file edits in an agent run and either commits them all (on success) or reverts them all (on failure). The `WorktreeGuard` is the RAII guard that enforces this—when it's dropped without being explicitly committed, the worktree is automatically restored.

```
GitRepo::open(working_dir)
       │
       ▼
begin_worktree()
       │
       ▼
WorktreeGuard (RAII)
  ├── Agent reads files from worktree
  ├── Agent writes files to worktree
  ├── Agent executes shell commands in worktree
  │
  ├── Success path:
  │   └── WorktreeGuard::commit(policy)
  │       ├── Stage all changes
  │       ├── Apply CommitPolicy (message, author, etc.)
  │       ├── Create git commit
  │       ├── Emit XaftCommitCreated signal
  │       └── Merge worktree changes back to main
  │
  └── Failure/cancel path:
      └── WorktreeGuard::drop() (implicit)
          ├── Discard all uncommitted changes
          ├── Restore worktree to original state
          └── Remove worktree
```

The key insight is that the `WorktreeGuard` makes rollback the *default* behavior. Committing requires an explicit, successful call to `commit()`. If anything goes wrong—a panic, a cancellation, an error return—the guard's `Drop` impl ensures the worktree is cleaned up and the repository is restored.

## Extension Patterns

### Opening a Repository

The lifecycle starts with `GitRepo::open()`, which validates that the directory is a git repository and loads its metadata:

```rust
let repo = GitRepo::open("/path/to/project").await?;
```

This performs several checks:

1. The directory exists and is accessible.
2. A `.git` directory or file is present (indicating a git repository).
3. The repository is not in a detached HEAD state (unless explicitly allowed).
4. There are no uncommitted changes in the working directory (the worktree will be based on a clean state).

If any check fails, `open()` returns an error before any worktree operations begin.

### Creating a Worktree

`begin_worktree()` creates a new git worktree—a directory that shares the object store with the main repository but has its own working tree and index:

```rust
let guard = repo.begin_worktree().await?;
```

Under the hood, this runs:

```bash
git worktree add /path/to/project/.xaft-worktree-<id> HEAD
```

The worktree directory is created inside `.xaft-worktree-<id>` (a hidden directory within the project root). The `<id>` is a unique identifier (typically a UUID) that prevents collisions when multiple xaft sessions run concurrently on the same repository.

The `WorktreeGuard` holds:

- The worktree path
- A reference to the parent `GitRepo`
- The original HEAD commit hash (for rollback verification)
- A `committed` flag (to detect double-commit or commit-after-drop)

### Agent Edits in the Worktree

Once the worktree is active, all file operations from tools are directed to the worktree path:

```rust
// In WriteFileTool
async fn call(&self, input: WriteFileInput, ctx: &ToolContext) -> ToolResult {
    let safe_path = validate_path(&input.path, ctx.workspace.root())?;
    // ctx.workspace.root() returns the worktree path, not the main repo path
    tokio::fs::write(&safe_path, &input.content).await
        .map_err(|e| ToolResult::Error(e.to_string()))?;
    ToolResult::Ok(json!({ "written": safe_path.display().to_string() }))
}
```

The workspace root is transparently set to the worktree path. Tools don't need to know they're operating in a worktree—they just write to `ctx.workspace.root()` as usual.

### Committing on Success

When the agent successfully completes its task, the `WorktreeGuard::commit()` method is called with a `CommitPolicy`:

```rust
pub struct CommitPolicy {
    /// The commit message. If None, a default message is generated.
    pub message: Option<String>,
    /// The author name and email. Defaults to "xaft <xaft@local>".
    pub author: Option<String>,
    /// Whether to amend the previous commit instead of creating a new one.
    pub amend: bool,
    /// Whether to push the commit to the remote after creating it.
    pub push: bool,
}

// Commit with a custom message
guard.commit(CommitPolicy {
    message: Some("feat: add JWT authentication middleware".into()),
    author: Some("xaft <xaft@local>".into()),
    amend: false,
    push: false,
}).await?;
```

The commit process:

1. **Stage all changes**: `git add -A` in the worktree.
2. **Check for changes**: If there are no staged changes, skip the commit (no-op).
3. **Create the commit**: `git commit -m <message> --author=<author>` in the worktree.
4. **Emit `XaftCommitCreated` signal**: Notifies the TUI and event bridge.
5. **Merge back**: The committed changes are merged from the worktree branch into the main branch.

```rust
impl WorktreeGuard {
    pub async fn commit(self, policy: CommitPolicy) -> Result<()> {
        assert!(!self.committed, "WorktreeGuard already committed");

        // Stage all changes
        self.repo.git_cmd(&["add", "-A"], Some(&self.worktree_path)).await?;

        // Check if there are changes to commit
        let status = self.repo.git_cmd(&["status", "--porcelain"], Some(&self.worktree_path)).await?;
        if status.trim().is_empty() {
            tracing::info!("No changes to commit");
            return Ok(());
        }

        // Create commit
        let message = policy.message.unwrap_or_else(|| {
            format!("xaft: automated changes ({})", chrono::Local::now().format("%Y-%m-%d %H:%M"))
        });
        let author = policy.author.unwrap_or_else(|| "xaft <xaft@local>".into());

        self.repo.git_cmd(
            &["commit", "-m", &message, "--author", &author],
            Some(&self.worktree_path),
        ).await?;

        // Emit signal
        let hash = self.repo.git_cmd(
            &["rev-parse", "HEAD"],
            Some(&self.worktree_path),
        ).await?;
        let files_changed: Vec<String> = status.lines()
            .map(|line| line[3..].trim().to_string())
            .collect();

        self.signal_bus.try_emit_signal(XaftCommitCreated {
            commit_hash: hash.trim().to_string(),
            message,
            files_changed,
        }).await;

        // Merge back to main
        self.merge_back().await?;

        Ok(())
    }
}
```

### Restoring on Failure/Cancel

The `WorktreeGuard`'s `Drop` implementation handles cleanup when the guard is dropped without committing:

```rust
impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if !self.committed {
            // Rollback: discard worktree changes
            let worktree_path = self.worktree_path.clone();
            let repo_path = self.repo.path().clone();

            // Best-effort cleanup; don't panic in Drop
            if let Err(e) = std::thread::scope(|s| {
                s.spawn(|| {
                    // Use blocking git commands since we're in Drop
                    let _ = std::process::Command::new("git")
                        .args(&["worktree", "remove", "--force", &worktree_path])
                        .current_dir(&repo_path)
                        .output();

                    // Verify the main working directory is clean
                    let _ = std::process::Command::new("git")
                        .args(&["checkout", "HEAD", "--", "."])
                        .current_dir(&repo_path)
                        .output();
                });
            }) {}

            tracing::warn!(
                "Worktree rolled back (uncommitted changes discarded): {}",
                worktree_path.display()
            );
        }
    }
}
```

The rollback is best-effort because `Drop` cannot be async and should not panic. If the git commands fail during cleanup, a warning is logged and the worktree directory may need manual cleanup.

### Handling Cancellation

When the cancellation token fires during a worktree operation:

```rust
tokio::select! {
    result = guard.commit(policy) => {
        result?;
    }
    _ = cancellation_token.cancelled() => {
        tracing::info!("Cancellation received; dropping worktree guard for rollback");
        drop(guard); // Triggers Drop → rollback
        return Ok(());
    }
}
```

The guard is dropped without calling `commit()`, so the `Drop` implementation performs the rollback.

## Common Pitfalls

1. **Forgetting to commit the WorktreeGuard.** If the agent finishes successfully but the code path doesn't call `guard.commit()`, the guard is dropped and all changes are rolled back. Always ensure the success path commits.

2. **Committing and then dropping.** The `committed` flag prevents double-commit, but if you commit and then accidentally drop (instead of consuming with `commit(self)`), the Drop impl will try to roll back a committed worktree. The assert in `commit()` catches this in debug builds.

3. **Running multiple xaft sessions on the same repo without worktree isolation.** Each session creates its own worktree with a unique ID, so concurrent sessions are safe. But if you bypass the worktree system and edit files directly in the main working directory, changes from one session will be visible to another.

4. **Not handling "no changes to commit" gracefully.** If the agent runs but doesn't modify any files (e.g., it only read files and produced text output), `commit()` is a no-op. Don't treat this as an error.

5. **Worktree directories accumulating after crashes.** If the process crashes (OOM, segfault) before `Drop` runs, the `.xaft-worktree-*` directories remain. Implement a cleanup routine that removes stale worktree directories on startup.

6. **Assuming worktree commits are on the main branch.** Worktree commits are on a separate branch. They must be merged back to the main branch via `merge_back()`. If `merge_back()` fails (e.g., due to conflicts), the changes are in the worktree but not in the main working directory.

7. **Large binary files in the worktree.** Git worktrees share the object store, so large binary files don't duplicate disk usage. However, committing large files in the worktree will add them to the shared object store, increasing repository size.

## Invariants

- **Worktree creation is atomic.** Either the worktree is fully created and usable, or it doesn't exist at all. There is no partial state.
- **The main working directory is never modified directly.** All agent edits go through the worktree. The main directory is only modified during `merge_back()`.
- **Rollback is the default.** The `WorktreeGuard` rolls back on drop unless explicitly committed. This is the RAII guarantee.
- **`XaftCommitCreated` is emitted exactly once per commit.** No emission on rollback, no double emission on commit.
- **The worktree path is unique per session.** No two concurrent xaft sessions share the same worktree.
- **`merge_back()` is called after commit.** Changes are not considered fully applied until they're merged back to the main branch.
- **Cleanup is best-effort in `Drop`.** If git commands fail during cleanup, a warning is logged. The system does not panic in `Drop`.

## Examples

### Full Agent Run with Worktree Protection

```rust
async fn run_agent_with_worktree(
    repo: &GitRepo,
    agent: &XaftAgent,
    goal: &str,
) -> Result<AgentRunOutcome> {
    // 1. Create worktree
    let guard = repo.begin_worktree().await?;

    // 2. Set workspace root to worktree path
    let workspace = Workspace::from_root(guard.worktree_path());
    let ctx = AgentContext::new(workspace, /* ... */);

    // 3. Run agent
    let outcome = tokio::select! {
        result = agent.run(&ctx) => result,
        _ = ctx.cancellation_token.cancelled() => {
            AgentRunOutcome::Cancelled
        }
    };

    // 4. Commit or rollback based on outcome
    match &outcome {
        AgentRunOutcome::Completed { .. } => {
            guard.commit(CommitPolicy {
                message: Some(format!("xaft: {}", goal)),
                author: None,
                amend: false,
                push: false,
            }).await?;
        }
        AgentRunOutcome::Cancelled | AgentRunOutcome::Error(_) | AgentRunOutcome::BudgetExhausted { .. } => {
            // Guard is dropped → automatic rollback
            tracing::info!("Agent outcome: {:?}; rolling back worktree", outcome);
        }
    }

    Ok(outcome)
}
```

### Startup Cleanup for Stale Worktrees

```rust
async fn cleanup_stale_worktrees(repo: &GitRepo) -> Result<()> {
    let entries = std::fs::read_dir(repo.path())?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with(".xaft-worktree-") {
            tracing::warn!("Removing stale worktree: {}", name_str);
            repo.git_cmd(&["worktree", "remove", "--force", &name_str], None).await?;
        }
    }
    Ok(())
}
```

### CommitPolicy Variants

```rust
// Simple auto-generated commit
let policy = CommitPolicy::default();

// Custom message for traceability
let policy = CommitPolicy {
    message: Some("fix: resolve null pointer in auth middleware".into()),
    ..Default::default()
};

// Amend the previous commit (for iterative refinement)
let policy = CommitPolicy {
    message: Some("fix: resolve null pointer in auth middleware (v2)".into()),
    amend: true,
    ..Default::default()
};

// Push after commit (for CI-triggered workflows)
let policy = CommitPolicy {
    message: Some("deploy: update production config".into()),
    push: true,
    ..Default::default()
};
```
