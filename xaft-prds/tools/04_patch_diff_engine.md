# Patch & Diff Engine

## Unified Diff Format

`xaft` uses standard unified diff format for all patches. This ensures compatibility with `git apply`, `patch`, and human readers.

## DiffApplier Workflow

```rust
pub async fn generate_and_apply_patch(
    workspace: &WorkspaceEditor,
    path: &Path,
    new_content: &str,
) -> Result<PatchStats, XaftError> {
    // 1. Read current content
    let original = workspace.read(path).await?;

    // 2. Generate unified diff
    let diff = workspace.diff(path, new_content).await?;

    // 3. Validate patch applies cleanly
    let dry_run = workspace.apply_patch_dry_run(path, &diff).await?;
    if !dry_run.success {
        return Err(XaftError::Workspace(format!("patch does not apply cleanly: {:?}", dry_run.conflicts)));
    }

    // 4. Apply atomically
    let stats = workspace.apply_patch(path, &diff).await?;

    // 5. Emit signal
    workspace.signal_bus.emit(PatchApplied {
        path: path.to_owned(),
        hunks_applied: stats.hunks_applied,
        lines_added: stats.lines_added,
        lines_removed: stats.lines_removed,
    }).await;

    Ok(stats)
}
```

## Patch Conflict Resolution

When a patch conflicts (e.g., parallel agents modified the same file), `xaft` invokes the conflict resolver:

```rust
pub async fn resolve_patch_conflict(
    original: &str,
    patch_a: &str,
    patch_b: &str,
    llm: &dyn LlmProvider,
) -> Result<String, XaftError> {
    let structured = StructuredLlm::<MergeResult>::new(Arc::new(llm));
    let result = structured.complete(&[
        Message::system("You are a git merge expert. Resolve the conflict by producing the correct merged content."),
        Message::user(format!(
            "Original:\n```\n{original}\n```\n\nPatch A:\n```diff\n{patch_a}\n```\n\nPatch B:\n```diff\n{patch_b}\n```\n\nProduce the merged result."
        )),
    ]).await?;
    Ok(result.merged_content)
}
```

## Three-Way Merge for Parallel Worktrees

```
base (main HEAD)
    ├── worktree-A edits → diff_a
    └── worktree-B edits → diff_b
                    ↓
            three_way_merge(base, diff_a, diff_b)
                    ↓
            merged_content → main branch
```

## References

- agtrs: `agtrs-workspace/src/diff.rs`
- agtrs tests: `agtrs-workspace/tests/editor_integration.rs`
