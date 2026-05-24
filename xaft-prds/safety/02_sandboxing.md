# Sandboxing

## ShellExecutor Policy Enforcement

The `ShellExecutor` from `agtrs-shell` enforces a policy at the point of execution. No command can bypass the policy — it is evaluated before any subprocess is spawned.

```rust
pub fn build_production_policy(config: &XaftConfig) -> ShellPolicy {
    ShellPolicy::new()
        .working_dir(&config.project_root)
        .max_output_bytes(10_000_000)
        .timeout(Duration::from_secs(300))
        // Allowlist approach
        .allow_commands(&config.safety.allowed_commands)
        // Env var restrictions
        .clear_env()
        .allow_env("PATH")
        .allow_env("HOME")
        .allow_env("CARGO_HOME")
        .allow_env("RUSTUP_HOME")
        .set_env("RUST_BACKTRACE", "1")
}
```

## Platform-Level Sandboxing (Future)

For maximum isolation, `xaft` plans platform-level sandboxing:

| Platform | Mechanism | Status |
|---|---|---|
| Linux | `seccomp` + `namespaces` via `landlock` | Planned v2 |
| macOS | `sandbox-exec` profiles | Planned v2 |
| Windows | Job Objects + AppContainer | Planned v3 |
| Cross-platform | Docker container per task | Available now via plugin |

## Worktree as Filesystem Boundary

The git worktree provides a logical filesystem boundary: agents write only to the worktree path, not the main working tree. The `WorkspaceEditor` enforces this:

```rust
impl WorkspaceEditor {
    fn validate_path(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let canonical = self.root.join(path).canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(WorkspaceError::PathEscape {
                attempted: path.to_owned(),
                root: self.root.clone(),
            });
        }
        Ok(canonical)
    }
}
```

## References

- agtrs: `agtrs-shell/src/policy.rs`, `agtrs-shell/src/sandbox.rs`
