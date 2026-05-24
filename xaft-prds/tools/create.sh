cat > ./01_tool_calling_system.md << 'EOF'
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
EOF

echo "Tools and orchestration handoff docs done"

cat > ./02_sandbox_execution.md << 'EOF'
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
EOF

cat > ./03_git_integration.md << 'EOF'
# Git Integration

## Core Git Operations

Built on `agtrs-git`, the git integration provides:

```rust
pub struct GitTools {
    repo: Arc<GitRepo>,
    worktree_mgr: Arc<WorktreeManager>,
    llm: Arc<dyn LlmProvider>,  // for commit message generation
}

impl GitTools {
    // Status / inspection
    pub async fn status(&self) -> Result<GitStatus, GitError>;
    pub async fn diff_unstaged(&self) -> Result<String, GitError>;
    pub async fn diff_staged(&self) -> Result<String, GitError>;
    pub async fn log(&self, n: usize) -> Result<Vec<Commit>, GitError>;

    // Staging and committing
    pub async fn stage_files(&self, paths: &[PathBuf]) -> Result<(), GitError>;
    pub async fn stage_all(&self) -> Result<(), GitError>;
    pub async fn commit(&self, message: &str) -> Result<String, GitError>;

    // Commit message generation
    pub async fn generate_commit_message(&self, diff: &str) -> Result<String, GitError>;

    // Worktree management
    pub async fn create_worktree(&self, task_id: Uuid) -> Result<GitWorktree, GitError>;
    pub async fn merge_worktree(&self, wt: &GitWorktree) -> Result<String, GitError>;
    pub async fn remove_worktree(&self, wt: &GitWorktree) -> Result<(), GitError>;

    // PR creation (if GitHub CLI available)
    pub async fn create_pr(&self, title: &str, body: &str) -> Result<String, GitError>;
}
```

## Commit Message Generation

```rust
pub async fn generate_commit_message(
    &self,
    diff: &str,
) -> Result<String, GitError> {
    let structured = StructuredLlm::<CommitMessage>::new(Arc::clone(&self.llm));
    let result = structured.complete(&[
        Message::system("Generate a concise git commit message following Conventional Commits format."),
        Message::user(format!("Generate a commit message for this diff:\n```diff\n{diff}\n```")),
    ]).await?;

    Ok(format!("{}: {}\n\n{}", result.type_, result.subject, result.body))
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct CommitMessage {
    #[schemars(description = "Conventional commit type: feat|fix|refactor|test|docs|chore")]
    type_: String,
    #[schemars(description = "Short imperative summary, max 72 chars")]
    subject: String,
    #[schemars(description = "Optional longer explanation")]
    body: String,
}
```

## PR Generation

After task completion, `xaft` can create a pull request:

```bash
xaft run "migrate auth to JWT" --create-pr
```

```rust
pub async fn create_pr(session: &XaftSession) -> Result<String, XaftError> {
    let diff = session.git.diff_between_branches("main", &session.task_branch()).await?;
    let plan_summary = session.completed_steps().await
        .iter()
        .map(|s| format!("- {}", s.step_description))
        .collect::<Vec<_>>()
        .join("\n");

    let pr_body = format!(
        "## Summary\nAutonomously generated by `xaft`.\n\n## Changes\n{plan_summary}\n\n## Cost\n${:.3}\n",
        session.cost_tracker.total().await
    );

    // Uses `gh` CLI if available
    let url = session.git.create_pr(
        &format!("xaft: {}", session.intent.goal),
        &pr_body,
    ).await?;

    Ok(url)
}
```

## References

- agtrs: `agtrs-git/src/{repo.rs, worktree.rs, tools.rs, message.rs}`
- agtrs tests: `agtrs-git/tests/git_integration.rs`
EOF

cat > ./04_patch_diff_engine.md << 'EOF'
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
EOF

echo "Tools docs done"