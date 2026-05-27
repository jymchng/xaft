# Tool Safety Contracts

## Purpose

Tools in xaft are the boundary between the LLM's autonomous decision-making and real-world side effects. Safety contracts are the multi-layered defense system that ensures tools execute only approved actions, within approved boundaries, and with the ability to abort at any time. This document describes each safety layer—confirmation gates, path validation, cancellation tokens, execution policies, sandboxing, and git isolation—and how they compose into a defense-in-depth strategy that prevents a misbehaving model from causing irreversible damage.

Safety is not optional or bolted on; it is woven into the tool execution path at every stage. Every tool author must understand these contracts to avoid creating gaps that an adversarial or confused model could exploit.

## Mental Model

Think of tool safety as a **nested checkpoint system**. A tool call must pass through every checkpoint before it produces side effects. If any checkpoint fails, the call is rejected or rolled back:

```
LLM requests tool call
       │
       ▼
  ┌─ Confirmation Gate ─── requires_confirmation() == true?
  │      │                    YES → wait for human approval
  │      │                    NO  → proceed
  │      ▼
  ├─ Input Validation ───── validate_path(), require_str(), type checks
  │      │                    FAIL → ToolResult::Error
  │      ▼
  ├─ Cancellation Check ─── cancellation_token.is_cancelled()?
  │      │                    YES → ToolResult::Error("cancelled")
  │      ▼
  ├─ Execution Policy ───── (for shell commands) allowed? timeout?
  │      │                    DENY → ToolResult::Error
  │      ▼
  ├─ Sandbox Boundary ───── workspace root confinement, resource limits
  │      │                    BREACH → ToolResult::Error
  │      ▼
  └─ Git Isolation ──────── worktree guard; rollback on failure
         │
         ▼
    Tool executes, produces ToolResult
```

Each layer catches different classes of problems. The confirmation gate catches *intent* mismatches (the model wants to do something the user didn't intend). Input validation catches *malformed* requests. Cancellation catches *stale* requests. Execution policy catches *dangerous* commands. Sandbox catches *escape* attempts. Git isolation catches *irreversible* mutations.

## Extension Patterns

### Setting the Confirmation Flag

The `requires_confirmation` flag on the `Tool` trait is the simplest and most important safety mechanism. Set it to `true` for any tool that:

- Modifies files on disk (write, delete, move)
- Executes arbitrary shell commands
- Makes outbound network requests
- Installs packages or dependencies

```rust
struct DeleteFileTool;

impl Tool<DeleteFileInput> for DeleteFileTool {
    fn requires_confirmation(&self) -> bool { true }  // Always requires approval
    // ...
}
```

Read-only tools (file reads, directory listings, searches) can safely set `requires_confirmation() -> false`.

### Path Traversal Protection with `validate_path()`

The `validate_path` function is the cornerstone of filesystem safety. It performs three checks:

1. **Canonicalization**: Resolves symlinks and normalizes the path.
2. **Root confinement**: Ensures the canonical path starts with the workspace root.
3. **Traversal rejection**: Detects `../` sequences that would escape the workspace.

```rust
async fn call(&self, input: WriteFileInput, ctx: &ToolContext) -> ToolResult {
    // This will reject paths like "../../etc/passwd"
    let safe_path = validate_path(&input.path, ctx.workspace.root())?;

    // Now safe_path is guaranteed to be within the workspace
    tokio::fs::write(&safe_path, &input.content).await
        .map_err(|e| ToolResult::Error(e.to_string()))?;
    ToolResult::Ok(json!({ "written": safe_path.display().to_string() }))
}
```

Never skip `validate_path` when dealing with user-influenced file paths. Even if the model "shouldn't" produce malicious paths, prompt injection or confused reasoning can cause it to.

### Cancellation Token Checking

The `CancellationToken` in `ToolContext` is a cooperative cancellation mechanism. It is set by the agent runtime when the user presses Ctrl+C or when a handoff times out. Tools with long-running operations must check it:

```rust
async fn call(&self, input: ShellInput, ctx: &ToolContext) -> ToolResult {
    let safe_cmd = validate_path(&input.command, ctx.workspace.root())?;

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&safe_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolResult::Error(e.to_string()))?;

    // Poll for completion with cancellation check
    loop {
        tokio::select! {
            status = child.wait() => {
                return ToolResult::Ok(json!({ "exit_code": status?.code() }));
            }
            _ = ctx.cancellation_token.cancelled() => {
                let _ = child.kill().await;
                return ToolResult::Error("cancelled".into());
            }
        }
    }
}
```

The `tokio::select!` pattern ensures the cancellation token is checked concurrently with the operation, providing near-instant response to abort requests.

### ExecutionPolicy for Shell Commands

`ExecutionPolicy` governs which shell commands are permitted and with what constraints:

```rust
struct ExecutionPolicy {
    allowed_commands: Vec<String>,   // Whitelist of command prefixes
    blocked_commands: Vec<String>,   // Blacklist (takes precedence)
    timeout: Duration,               // Maximum execution time
    allow_network: bool,             // Whether network access is permitted
}
```

Shell-executing tools must consult the policy before running any command:

```rust
if !policy.is_allowed(&input.command) {
    return ToolResult::Error(format!(
        "Command '{}' blocked by execution policy", input.command
    ));
}
```

### Sandbox with Timeout

The `Sandbox` struct wraps tool execution in resource-constrained environment:

```rust
let sandbox = Sandbox::new()
    .with_timeout(Duration::from_secs(30))
    .with_max_output_bytes(1024 * 1024)  // 1MB output limit
    .with_workspace_root(ctx.workspace.root());

sandbox.run(async {
    // Tool execution happens here
    tool.call(input, ctx).await
}).await?;
```

If the tool exceeds the timeout, the sandbox kills the process and returns `ToolResult::Error("timeout")`.

## Common Pitfalls

1. **Marking destructive tools as not requiring confirmation.** This is the single most dangerous pitfall. A `rm -rf` tool with `requires_confirmation() -> false` means the model can delete files without any human oversight.

2. **Validating only the filename, not the full path.** A filename like `../../../etc/shadow` is harmless as a filename but dangerous when joined with a directory. Always use `validate_path` on the full resolved path.

3. **Checking the cancellation token only at the start.** A tool that checks `is_cancelled()` once and then runs for 60 seconds is effectively uncancellable for that duration. Check at every loop iteration or use `tokio::select!`.

4. **Trusting model-generated paths without validation.** Even in a "trusted" context, the model can be confused by prompt injection. `validate_path` is cheap; use it always.

5. **Bypassing ExecutionPolicy for "internal" commands.** There is no such thing as a safe command when the model is generating it. All commands must go through the policy.

6. **Forgetting to handle sandbox timeouts gracefully.** If a tool is killed mid-write, the file may be in an inconsistent state. Use the git worktree isolation so the incomplete write can be rolled back.

## Invariants

- **Every file path used by a tool must pass through `validate_path`.** No exceptions.
- **`requires_confirmation` is always `true` for tools with irreversible side effects.** This is a hard rule, not a guideline.
- **The cancellation token is always checked before any blocking or long-running operation.** A tool that doesn't respect cancellation is a bug.
- **Shell commands always go through `ExecutionPolicy`.** Even commands generated by other tools.
- **Sandbox timeout is always set for shell commands.** Infinite hangs are not acceptable.
- **Defense-in-depth: no single layer is sufficient.** Each layer (policy + gate + git isolation) must be independently correct.

## Examples

### Defense-in-Depth: A Safe Shell Tool

This example shows all five safety layers working together:

```rust
struct SafeShellTool {
    policy: ExecutionPolicy,
    sandbox: Sandbox,
}

#[async_trait]
impl Tool<ShellInput> for SafeShellTool {
    fn name(&self) -> &str { "shell" }
    fn description(&self) -> &str { "Execute a shell command." }
    fn schema(&self) -> Value { json_schema::<ShellInput>() }
    fn requires_confirmation(&self) -> bool { true }  // Layer 1: confirmation gate

    async fn call(&self, input: ShellInput, ctx: &ToolContext) -> ToolResult {
        // Layer 2: execution policy
        if !self.policy.is_allowed(&input.command) {
            return ToolResult::Error(format!("blocked by policy: {}", input.command));
        }

        // Layer 3: cancellation check
        if ctx.cancellation_token.is_cancelled() {
            return ToolResult::Error("cancelled before execution".into());
        }

        // Layer 4: sandbox with timeout
        self.sandbox.run(async {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&input.command)
                .current_dir(ctx.workspace.root())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| ToolResult::Error(e.to_string()))?;

            tokio::select! {
                status = child.wait() => {
                    let status = status.map_err(|e| ToolResult::Error(e.to_string()))?;
                    ToolResult::Ok(json!({ "exit_code": status.code() }))
                }
                _ = ctx.cancellation_token.cancelled() => {
                    let _ = child.kill().await;
                    ToolResult::Error("cancelled during execution".into())
                }
            }
        }).await
        // Layer 5: git worktree isolation ensures rollback on any error
    }
}
```

### Minimal Policy Configuration

```rust
let policy = ExecutionPolicy {
    allowed_commands: vec![
        "ls".into(), "cat".into(), "grep".into(),
        "cargo".into(), "npm".into(), "git".into(),
    ],
    blocked_commands: vec![
        "rm -rf /".into(), "sudo".into(), "curl".into(),
    ],
    timeout: Duration::from_secs(60),
    allow_network: false,
};
```
