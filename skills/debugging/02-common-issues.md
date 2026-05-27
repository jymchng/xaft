# Common Issues and Diagnoses

## Purpose

Even with perfect tracing, diagnosing issues in a concurrent, multi-layered runtime requires knowing where to look and what to look for. This document catalogs the most common error messages and symptoms that xaft users and developers encounter, explains the root cause for each, and provides the diagnostic steps to confirm and fix the issue. Think of this as the runtime's troubleshooting manual: when something goes wrong, start here.

## Mental Model

Think of common issues as a diagnostic decision tree. The error message is the root, and each branch leads to a root cause and a fix. The tree is organized by subsystem: provider errors (API key, network, rate limits), git errors (worktree, branch, uncommitted changes), session errors (storage, status, data directory), cancellation errors (token propagation, approval gates), and cost errors (limit exceeded, tracker subscription). For each error, there is a "check first" step (the most likely cause) and a "check next" step (the less likely but possible cause). The goal is to minimize time-to-fix by prioritizing the most common root causes.

## Extension Patterns

When a new error message is added to the codebase, add a corresponding entry to this document with the error text, root cause, and diagnostic steps. When a user reports an issue that is not covered here, diagnose it using the tracing setup guide, then add the finding to this document so future developers can find it. When an error message is ambiguous (could mean multiple things), improve the error message in the code to include more context (e.g., which config key was missing, which path was not found), then update this document to reflect the improved message.

## Common Pitfalls

- **Jumping to code changes before diagnosing**: Many issues that look like bugs are actually configuration problems (wrong API key, missing git init, incorrect data directory). Always diagnose first using the steps in this document before modifying code.
- **Ignoring the debug log file**: The error message shown in the TUI is a summary. The full context (which provider, which tool, which session) is in the debug log. Always check the log file for the complete picture.
- **Assuming the issue is in the code you just changed**: The runtime is highly concurrent—a failure in one component can manifest in another. A provider timeout can look like an agent hang; a git error can look like a tool failure. Trace the span hierarchy to find the actual source.
- **Not checking the config file**: Many "provider not found" and "session not found" errors are caused by typos or misconfigurations in `.xaft.toml`. Always verify the config file matches the expected values before investigating code paths.

## Invariants

1. Every error message that a user can see must be documented in this file with a root cause and fix.
2. The diagnostic steps must be ordered from most likely to least likely root cause.
3. Each entry must reference the relevant tracing span or log line to look for.
4. When a fix requires a code change, the entry must reference the relevant source file or module.

## Issue Catalog

### "provider not found"

**Symptom**: The runtime fails to start or an agent fails with `ProviderError::ProviderNotFound`.

**Root cause**: The `provider` key in the config file does not match any variant in `ProviderType`, or the agent preset references a provider that is not configured.

**Diagnostic steps**:
1. Check `.xaft.toml` for the `[provider]` section. Verify that `provider_type` is one of: `anthropic`, `openai`, `gemini` (lowercase, snake_case).
2. Check the agent preset's `provider` key. If the preset says `provider = "claude"` but the config says `provider_type = "anthropic"`, they won't match.
3. Check `RUST_LOG=xaft_providers=debug` output for the `ProviderFactory::build()` call and the resolved `ProviderType`.

**Fix**: Align the config `provider_type` with the agent preset's `provider` key, or add the missing provider section to the config.

---

### "git worktree failed"

**Symptom**: A tool that uses git (edit, commit, branch) fails with `GitOpsError::WorktreeFailed`.

**Root cause**: The repository is not initialized, or there are uncommitted changes that prevent worktree creation.

**Diagnostic steps**:
1. Run `git status` in the workspace root. If it says "not a git repository," initialize one with `git init`.
2. Check for uncommitted changes: `git diff --stat`. If there are uncommitted changes, the worktree creation will fail because git cannot create a worktree from a dirty index.
3. Check `RUST_LOG=xaft_git_ops=debug` output for the worktree creation path and the git error message.
4. Verify that the `.git` directory exists and is not corrupted: `git fsck`.

**Fix**: Initialize git if needed (`git init`), commit or stash uncommitted changes, and retry. If the `.git` directory is corrupted, restore from backup or reinitialize.

---

### "session not found"

**Symptom**: The runtime fails to load a session with `SessionError::SessionNotFound`.

**Root cause**: The `data_dir` path in the config does not point to the directory where the session was created, or the session SQLite file was deleted.

**Diagnostic steps**:
1. Check the `data_dir` path in `.xaft.toml`. If it's relative, it's resolved from the current working directory, which may differ between runs.
2. Verify the SQLite file exists: `ls -la <data_dir>/sessions/<session_id>.db`.
3. Check `RUST_LOG=xaft_session_store=debug` for the resolved data directory path and the file open attempt.
4. If the file doesn't exist, check if it was created in a different directory (perhaps a previous run used a different working directory).

**Fix**: Use an absolute path for `data_dir` in the config. If the session was created with a relative path, find the SQLite file and either move it to the correct directory or update the config.

---

### "tool call cancelled"

**Symptom**: A tool call returns `AgtrsError::Cancelled` unexpectedly, or the agent loop exits with "cancelled" even though the user didn't press Ctrl+C.

**Root cause**: The `CancellationToken` is being triggered prematurely—either by a parent task that cancelled the token, or by a shutdown handler that called `cancel.cancel()` too early.

**Diagnostic steps**:
1. Check `RUST_LOG=agtrs_runtime=debug` for the `CancellationToken::cancel()` call. Look for the span that triggered it—was it a Ctrl+C signal, a TUI quit event, or a programmatic shutdown?
2. If no explicit cancellation is found, check if the tool is running inside a `tokio::select!` where another branch completed first (e.g., a timeout or a different event).
3. Verify that the `CancellationToken` being used is the same one that the EventLoop manages. If a task creates its own token, it won't be linked to the global cancellation chain.

**Fix**: Ensure the `CancellationToken` is propagated from the `EventLoop` to all spawned tasks. Check for premature `cancel.cancel()` calls in shutdown handlers. If a timeout is cancelling the tool, increase the timeout or remove the timeout in favor of explicit cancellation.

---

### "cost limit exceeded"

**Symptom**: An agent fails with `ProviderError::CostLimitExceeded` before completing its task.

**Root cause**: The accumulated cost has exceeded the configured limit in `guardrail.cost_limit_config`.

**Diagnostic steps**:
1. Check `.xaft.toml` for the `[guardrail]` section and the `cost_limit_config` value.
2. Check `RUST_LOG=xaft_providers=debug` for the `CostedProvider` cost recording calls. Verify that the accumulated cost matches the expected value.
3. If the accumulated cost is unexpectedly high, check if the `CostedProvider` is subscribed before the first LLM call (invariant: subscription must happen before any calls).
4. If the limit is too low for the task, check the model's pricing and estimate the expected cost.

**Fix**: Increase the `cost_limit_config` value in `.xaft.toml`, switch to a cheaper model, or reduce the task scope. If the cost tracking seems incorrect, verify that `CostedProvider` is the outermost layer in the provider chain.

## Examples

```bash
# Diagnose "provider not found"
rg "ProviderFactory" ~/.xaft/debug-12345.log
rg "provider_type" ~/.xaft/debug-12345.log

# Diagnose "git worktree failed"
rg "WorktreeFailed" ~/.xaft/debug-12345.log
rg "worktree" ~/.xaft/debug-12345.log
git status  # in the workspace directory
git diff --stat

# Diagnose "session not found"
rg "SessionNotFound" ~/.xaft/debug-12345.log
rg "data_dir" ~/.xaft/debug-12345.log
ls -la ~/.xaft/sessions/  # or the configured data_dir

# Diagnose "tool call cancelled"
rg "Cancelled" ~/.xaft/debug-12345.log
rg "cancel.cancel()" ~/.xaft/debug-12345.log
rg "CancellationToken" ~/.xaft/debug-12345.log

# Diagnose "cost limit exceeded"
rg "CostLimitExceeded" ~/.xaft/debug-12345.log
rg "cost_tracker" ~/.xaft/debug-12345.log
rg "total_cost" ~/.xaft/debug-12345.log
```
