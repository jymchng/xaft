# Tool Calling System

## Tool Registry

`xaft` registers tools at session startup. The registry is passed to every `AgentContext`.

```rust
pub fn build_tool_registry(session: &XaftSession) -> Vec<(String, Arc<ErasedTool>)> {
    let policy = session.config.shell_policy();
    let shell = Arc::new(ShellExecutor::new(policy));

    vec![
        // Filesystem
        ("read_file",    Arc::new(ReadFileTool::new(session.workspace.clone()))),
        ("write_file",   Arc::new(WriteFileTool::new(session.workspace.clone()))),
        ("list_files",   Arc::new(ListFilesTool::new(session.workspace.clone()))),
        ("search_files", Arc::new(SearchFilesTool::new(session.workspace.clone()))),
        ("apply_patch",  Arc::new(ApplyPatchTool::new(session.workspace.clone()))),
        ("delete_file",  Arc::new(DeleteFileTool::new(session.workspace.clone()))),  // High risk

        // Shell execution
        ("run_cargo",    Arc::new(RunCargoTool::new(shell.clone()))),
        ("run_command",  Arc::new(RunCommandTool::new(shell.clone()))),  // High risk

        // Git
        ("git_status",   Arc::new(GitStatusTool::new(session.git.clone()))),
        ("git_diff",     Arc::new(GitDiffTool::new(session.git.clone()))),
        ("git_commit",   Arc::new(GitCommitTool::new(session.git.clone()))),
        ("git_log",      Arc::new(GitLogTool::new(session.git.clone()))),

        // Index / Search
        ("search_code",  Arc::new(SearchCodeTool::new(session.index.clone()))),
        ("find_symbol",  Arc::new(FindSymbolTool::new(session.index.clone()))),
        ("get_deps",     Arc::new(GetDependenciesTool::new(session.index.clone()))),

        // Meta
        ("checkpoint",   Arc::new(CheckpointTool::new(session.task_runner.clone()))),
        ("replan",       Arc::new(ReplanTool::new(session.planner.clone(), session.task_runner.clone()))),
    ]
}
```

## Tool Risk Classification

Each tool declares its risk level via metadata:

```rust
pub trait XaftTool: Tool {
    fn risk_level(&self) -> RiskLevel { RiskLevel::Low }
    fn target_files(&self, input: &serde_json::Value) -> Vec<PathBuf> { vec![] }
    fn estimated_side_effects(&self, input: &serde_json::Value) -> String {
        "Unknown side effects".to_string()
    }
}
```

| Tool | Risk | Auto-approve |
|---|---|---|
| `read_file` | Low | Always |
| `list_files` | Low | Always |
| `search_files` | Low | Always |
| `search_code` | Low | Always |
| `find_symbol` | Low | Always |
| `git_status` | Low | Always |
| `git_diff` | Low | Always |
| `git_log` | Low | Always |
| `write_file` | Medium | Config-dependent |
| `apply_patch` | Medium | Config-dependent |
| `run_cargo check` | Medium | Config-dependent |
| `run_cargo test` | Medium | Config-dependent |
| `git_commit` | Medium | Config-dependent |
| `delete_file` | High | Always pause |
| `run_command` | High | Always pause |
| `git_push` | High | Always pause |

## Tool Schema Examples

### read_file

```rust
#[tool(
    description = "Read the contents of a file from the workspace",
    param(path, description = "Relative path from workspace root (e.g. src/main.rs)", ty = "string"),
    param(start_line, description = "Start line (1-indexed, optional)", ty = "integer", default = 1),
    param(end_line, description = "End line (inclusive, optional, 0 = all)", ty = "integer", default = 0),
)]
async fn read_file(path: String, start_line: usize, end_line: usize, ctx: &ToolContext) -> Result<String, AgtrsError> {
    let workspace = ctx.resolve_workspace()?;
    let content = workspace.read(Path::new(&path)).await?;
    // Apply line range if specified
    if end_line > 0 || start_line > 1 {
        let lines: Vec<&str> = content.lines().collect();
        let start = (start_line - 1).min(lines.len());
        let end = if end_line == 0 { lines.len() } else { end_line.min(lines.len()) };
        Ok(lines[start..end].join("\n"))
    } else {
        Ok(content)
    }
}
```

### write_file

```rust
#[tool(
    description = "Write content to a file in the active worktree. Creates the file if it doesn't exist.",
    param(path, description = "Relative path from workspace root", ty = "string"),
    param(content, description = "Full file content to write", ty = "string"),
    param(commit_message, description = "Optional commit message for this change", ty = "string", default = ""),
)]
async fn write_file(path: String, content: String, commit_message: String, ctx: &ToolContext) -> Result<String, AgtrsError> {
    if ctx.is_cancelled() {
        return Err(AgtrsError::Cancelled { reason: "write_file cancelled".into() });
    }
    let workspace = ctx.resolve_workspace()?;
    workspace.write(Path::new(&path), &content).await?;
    Ok(format!("Written {} bytes to {path}", content.len()))
}
```

## Tool Hook Global Registration

`xaft` installs global tool hooks at session startup:

```rust
pub fn install_global_hooks(session: &mut AgentContext, config: &XaftConfig) {
    // 1. Audit log hook (all tools)
    session.add_global_tool_hook(Arc::new(AuditLogHook::new(session.audit_writer.clone())));

    // 2. Cost tracking hook (all tools - measures duration)
    session.add_global_tool_hook(Arc::new(MetricsHook::new(session.signal_bus.clone())));

    // 3. Risk classification + approval gate (high-risk tools)
    session.add_global_tool_hook(Arc::new(ApprovalHook::new(
        session.approval_gate.clone(),
        config.safety.auto_approve.clone(),
    )));

    // 4. Cancellation check (all tools)
    session.add_global_tool_hook(Arc::new(CancellationHook::new(session.root_cancel.clone())));
}
```

## References

- agtrs: `agtrs-runtime/src/tool.rs`, `agtrs-runtime/src/tool_hooks.rs`
- agtrs guide: `guides/02-defining-tools.md`, `guides/05-tool-hooks.md`
