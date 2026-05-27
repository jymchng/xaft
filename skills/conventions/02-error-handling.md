# Error Handling Conventions

## Purpose

Error handling in xaft must balance three goals: correctness (the right error reaches the right layer), observability (every error is traceable to its source), and user experience (the agent recovers when possible and reports clearly when not). A single `anyhow` blob at the top of the stack would sacrifice all three. Instead, xaft uses a layered error architecture where each crate defines its own typed errors via `thiserror`, the tool/agent layer unifies them into `AgtrsError`, and the runtime maps `RuntimeError` variants to exit codes. Soft errors (a tool that failed but the agent can retry) use `ToolResult` with `is_error: true`, while hard errors (a cancelled operation, a corrupted session) propagate as `Err`. This distinction is critical: the agent loop inspects `ToolResult.is_error` to decide whether to retry, but it never catches `Err` from a hard failure—that bubbles to the runtime.

## Mental Model

Think of errors as a funnel. At the wide end, each crate has its own error enum (e.g., `xaft-git-ops::GitError`, `xaft-session-store::SessionError`). These are domain-specific and carry context (which branch, which session ID). At the middle, `AgtrsError` unifies tool and agent errors with named variants like `AgtrsError::ToolFailed { name, source }` and `AgtrsError::AgentError { agent, source }`. At the narrow end, `RuntimeError` maps to exit codes: `RuntimeError::ConfigError → exit 2`, `RuntimeError::ProviderUnavailable → exit 3`, `RuntimeError::Cancelled → exit 130`. The funnel ensures that low-level details are preserved for debugging but the runtime only sees what it needs to act on.

## Extension Patterns

When adding a new crate, define a `thiserror` error enum at the crate root. Name it `<CrateName>Error` (e.g., `GitOpsError`, `SessionStoreError`). Each variant should have a descriptive name and carry the relevant context as fields. When integrating the crate into the tool/agent layer, add a variant to `AgtrsError` that wraps the crate error with `#[source]`. When a new error condition needs a distinct exit code, add a variant to `RuntimeError` and update the `ExitCode` mapping in the runtime's `main()`. For soft errors—situations where a tool fails but the agent should try a different approach—return `ToolResult { output: error_message, is_error: true }` rather than `Err`. For hard errors—situations where the operation cannot meaningfully continue—return `Err` and let it propagate.

## Common Pitfalls

- **Using `anyhow` in library crates**: `anyhow` erases type information and makes it impossible to match on specific error variants downstream. Reserve `anyhow` for the binary crate's `main()` where you need a final error report, never in library code.
- **Wrapping all errors in `AgtrsError::Internal`**: A catch-all variant defeats the purpose of typed errors. If you find yourself reaching for `Internal`, either add a new named variant or check whether the error should be a soft `ToolResult` instead.
- **Ignoring `is_cancelled()`**: After an awaited operation, always check `is_cancelled()` on the error. A cancelled tool call should not be retried—the agent loop must exit gracefully. Missing this check leads to zombie retries after the user pressed Ctrl+C.
- **Losing context in error chains**: When converting from a crate error to `AgtrsError`, always use `#[source]` and include context fields (tool name, agent name, path). An `AgtrsError::ToolFailed` without a tool name is useless for debugging.
- **Returning `Err` for retryable failures**: If a file write fails because of a permissions error and the agent could try a different path, return `ToolResult { is_error: true }` so the agent loop can retry. Returning `Err` would abort the entire session.

## Invariants

1. Every library crate must define its own `thiserror` error enum. Never use `anyhow` in library code.
2. `AgtrsError` must have a named variant for each crate-level error it wraps. No catch-all `Internal` variant.
3. `RuntimeError` must map every variant to a distinct exit code. No two variants may share an exit code.
4. `ToolResult` with `is_error: true` indicates a soft error (agent may retry). `Err` indicates a hard error (agent must abort).
5. After every `.await` on a tool call or agent step, check `is_cancelled()` on the result before proceeding.
6. Error conversion must preserve source context via `#[source]` and must include identifying fields (tool name, path, session ID).

## Examples

```rust
// Crate-level error (xaft-git-ops)
#[derive(Debug, thiserror::Error)]
pub enum GitOpsError {
    #[error("worktree creation failed at {path}: {source}")]
    WorktreeFailed {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
    #[error("branch {branch} not found in repository")]
    BranchNotFound { branch: String },
}

// Unified tool/agent error
#[derive(Debug, thiserror::Error)]
pub enum AgtrsError {
    #[error("tool {name} failed: {source}")]
    ToolFailed {
        name: String,
        #[source]
        source: GitOpsError,
    },
    #[error("agent {agent} encountered an error: {source}")]
    AgentError {
        agent: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

// Runtime error with exit code mapping
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("configuration error: {0}")]
    ConfigError(String),    // → exit 2
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String), // → exit 3
    #[error("operation cancelled")]
    Cancelled,              // → exit 130
}

// Soft error via ToolResult
fn write_file(path: &Path, content: &str) -> ToolResult {
    match fs::write(path, content) {
        Ok(()) => ToolResult { output: "File written".into(), is_error: false },
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            ToolResult { output: format!("Permission denied: {e}"), is_error: true }
            // Agent can retry with a different path
        }
        Err(e) => return Err(AgtrsError::ToolFailed {
            name: "write_file".into(),
            source: e.into(),
        }),
    }
}

// Cancellation detection
impl AgtrsError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, AgtrsError::Cancelled)
            || matches!(self, AgtrsError::ToolFailed { source, .. } if source.is_cancelled())
    }
}
```
