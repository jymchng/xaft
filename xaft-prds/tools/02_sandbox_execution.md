# Sandbox Execution

## ShellPolicy Configuration

```rust
pub fn build_shell_policy(config: &XaftConfig) -> ShellPolicy {
    let mut policy = ShellPolicy::new()
        .working_dir(&config.project_root)
        .max_output_bytes(10_000_000)  // 10MB output cap
        .timeout(Duration::from_secs(300));  // 5min per command

    // Allowlist approach: only permit known-safe commands
    for cmd in &config.safety.allowed_commands {
        policy = policy.allow_command(cmd);
    }

    // Blocklist for extra safety
    for cmd in &["rm", "rmdir", "find", "curl", "wget", "ssh", "scp"] {
        if !config.safety.allowed_commands.contains(&cmd.to_string()) {
            policy = policy.deny_command(cmd);
        }
    }

    policy
}
```

Default allowed commands:
```toml
# ~/.config/xaft/config.toml
[safety.allowed_commands]
commands = [
    "cargo",
    "rustfmt",
    "clippy-driver",
    "git",
    "grep",
    "rg",     # ripgrep
    "fd",     # fd-find
    "cat",
    "head",
    "tail",
    "wc",
    "diff",
    "ls",
]
```

## RunCargoTool — Sandboxed Cargo

```rust
#[tool(
    description = "Run a cargo command in the workspace. Only allowed subcommands: check, test, build, clippy, fmt",
    param(subcommand, description = "Cargo subcommand and args (e.g. 'test --workspace')", ty = "string"),
)]
async fn run_cargo(subcommand: String, ctx: &ToolContext) -> Result<String, AgtrsError> {
    // Validate subcommand starts with allowed prefix
    let allowed = ["check", "test", "build", "clippy", "fmt", "doc"];
    let first_word = subcommand.split_whitespace().next().unwrap_or("");
    if !allowed.contains(&first_word) {
        return Err(AgtrsError::ToolCallRejected {
            tool_name: "run_cargo".into(),
            reason: format!("subcommand '{first_word}' not allowed. Allowed: {allowed:?}"),
        });
    }

    let shell = ctx.resolve_shell()?;
    let cmd = format!("cargo {subcommand}");

    let mut output_lines = Vec::new();
    let mut stream = shell.run_stream(&cmd, None);

    while let Some(chunk) = stream.next().await {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled { reason: "cargo cancelled".into() });
        }
        output_lines.push(chunk.data);
    }

    let combined = output_lines.join("");
    let exit_code = stream.exit_code().await;

    if exit_code == 0 {
        Ok(format!("cargo {subcommand} succeeded:\n{combined}"))
    } else {
        Ok(format!("cargo {subcommand} failed (exit {exit_code}):\n{combined}"))
        // Returns as Ok (not Err) so LLM can read the error output
    }
}
```

## Replay Audit

Every shell command is recorded to the session audit log:

```json
{"ts":"2026-01-15T10:23:45Z","event":"shell_started","command":"cargo test --workspace","pid":12345,"session_id":"ses-abc"}
{"ts":"2026-01-15T10:23:52Z","event":"shell_complete","command":"cargo test --workspace","exit_code":1,"duration_ms":7234,"stderr_bytes":2048}
```

The `agtrs-shell::replay::ReplayRecorder` captures full I/O for forensic analysis.

## References

- agtrs: `agtrs-shell/src/{policy.rs, executor.rs, replay.rs}`
