# Workspace & Filesystem Architecture

## Design Principle: Immutable Working Tree

`xaft` never modifies the user's working tree directly. All agent file modifications occur in **isolated git worktrees**. The working tree remains clean until the user explicitly applies the generated changes.

```
project/                    ← user's working tree (NEVER modified by xaft)
├── src/
├── Cargo.toml
└── .xaft/                  ← xaft session data
    ├── sessions/
    ├── index/
    └── config.toml

/tmp/xaft-wt-{uuid}/        ← isolated git worktree for active task
├── src/                    ← agent edits go here
└── Cargo.toml
```

## WorkspaceEditor — Atomic File Operations

Built on `agtrs-workspace`, `WorkspaceEditor` provides atomic, audited file operations.

```rust
pub trait WorkspaceOperations: Send + Sync {
    // Read
    async fn read(&self, path: &Path) -> Result<String, WorkspaceError>;
    async fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, WorkspaceError>;
    async fn list(&self, glob: &str) -> Result<Vec<PathBuf>, WorkspaceError>;
    async fn fuzzy_find(&self, query: &str, limit: usize) -> Result<Vec<FuzzyMatch>, WorkspaceError>;
    async fn exists(&self, path: &Path) -> bool;

    // Write (atomic: temp file → rename)
    async fn write(&self, path: &Path, content: &str) -> Result<(), WorkspaceError>;
    async fn write_bytes(&self, path: &Path, content: &[u8]) -> Result<(), WorkspaceError>;
    async fn delete(&self, path: &Path) -> Result<(), WorkspaceError>;
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), WorkspaceError>;
    async fn mkdir(&self, path: &Path) -> Result<(), WorkspaceError>;

    // Diff and patch
    async fn diff(&self, path: &Path, new_content: &str) -> Result<UnifiedDiff, WorkspaceError>;
    async fn apply_patch(&self, path: &Path, diff: &UnifiedDiff) -> Result<PatchStats, WorkspaceError>;
    async fn apply_unified_diff_str(&self, diff_str: &str) -> Result<Vec<PatchStats>, WorkspaceError>;

    // Metadata
    async fn list_modified(&self) -> Result<Vec<PathBuf>, WorkspaceError>;
    async fn summarize_structure(&self) -> Result<String, WorkspaceError>;
    fn root(&self) -> &Path;
}
```

All writes emit `FileWritten` signal. All patch applications emit `PatchApplied` signal. These signals drive TUI diff pane updates.

## Git Worktree Lifecycle

### Creation

```rust
pub struct WorktreeManager {
    repo: Arc<GitRepo>,
    base_dir: PathBuf,   // /tmp/xaft-wt-*
}

impl WorktreeManager {
    /// Create a fresh worktree branching from the current HEAD.
    pub async fn create_for_task(
        &self,
        task_id: Uuid,
        base_branch: &str,
    ) -> Result<GitWorktree, XaftError> {
        let branch_name = format!("xaft/{task_id}");
        let path = self.base_dir.join(format!("xaft-wt-{task_id}"));

        let wt = self.repo.create_worktree(&path, &branch_name, base_branch).await?;

        self.signal_bus.emit(WorktreeCreated {
            worktree_path: wt.path().to_owned(),
            branch: branch_name,
            base_commit: self.repo.head_commit_sha().await?,
        }).await;

        Ok(wt)
    }

    /// Merge worktree changes back to base branch (fast-forward if possible).
    pub async fn merge_to_base(
        &self,
        wt: &GitWorktree,
        commit_message: &str,
    ) -> Result<String, XaftError> {
        // 1. Stage all modified files
        self.repo.stage_all_in_worktree(wt).await?;
        // 2. Commit
        let sha = self.repo.commit_in_worktree(wt, commit_message).await?;
        // 3. Merge to base (or create PR)
        Ok(sha)
    }

    /// Remove worktree, optionally preserving the branch for PR creation.
    pub async fn remove(&self, wt: &GitWorktree, keep_branch: bool) -> Result<(), XaftError> {
        self.repo.remove_worktree(wt).await?;
        if !keep_branch {
            self.repo.delete_branch(&wt.branch()).await?;
        }
        self.signal_bus.emit(WorktreeRemoved {
            worktree_path: wt.path().to_owned(),
            committed: true,
        }).await;
        Ok(())
    }
}
```

### Worktree Isolation Properties

- **No shared file locks**: Each worktree has its own working directory. Parallel agents can write different files simultaneously without conflict.
- **Branch isolation**: Each task gets a dedicated branch `xaft/{task_id}`. Merging is explicit and reviewable.
- **Rust toolchain**: `cargo` operations in a worktree use the same `Cargo.lock` as the base. Dependencies do not diverge.
- **Index coherence**: The `.git/index` is separate per worktree — staging in one worktree doesn't affect others.

## Parallel Worktree Execution

For multi-agent tasks (e.g., "migrate auth module" + "add logging to API layer"):

```
Main worktree (read-only for agents)
├── xaft-wt-{task1}/    ← CodeAgent A (auth migration)
│   └── src/auth/
└── xaft-wt-{task2}/    ← CodeAgent B (API logging)
    └── src/api/

After both complete:
git merge --no-ff xaft/{task1} xaft/{task2}
```

Conflict detection: before spawning parallel agents, `xaft` analyzes the plan to identify file-level conflicts. If Agent A and Agent B would modify the same file, they are serialized, not parallelized.

```rust
pub fn has_file_conflict(plan_a: &Plan, plan_b: &Plan) -> bool {
    let files_a: HashSet<&str> = plan_a.steps.iter()
        .filter_map(|s| s.target_file.as_deref())
        .collect();
    let files_b: HashSet<&str> = plan_b.steps.iter()
        .filter_map(|s| s.target_file.as_deref())
        .collect();
    !files_a.is_disjoint(&files_b)
}
```

## Repository Indexing

`xaft-index` builds a semantic understanding of the repository for efficient code search.

### Index Structure

```rust
pub struct RepoIndex {
    /// Tree-sitter parsed symbol table: file → Vec<Symbol>
    pub symbols: Arc<SymbolIndex>,
    /// TF-IDF + optional embedding index for semantic search
    pub content: Arc<dyn MemoryStore>,
    /// File dependency graph: file A imports file B
    pub import_graph: Arc<ImportGraph>,
    /// Last index build time
    pub built_at: DateTime<Utc>,
    /// File hashes for incremental rebuild
    pub checksums: Arc<RwLock<HashMap<PathBuf, [u8; 32]>>>,
}
```

### Index Build Process

```
xaft index build
    ↓
1. Walk workspace (respect .gitignore)
2. For each .rs file:
   a. Parse with tree-sitter → extract: functions, structs, enums, traits, impl blocks
   b. Hash file content (SHA-256) → compare with stored checksum
   c. If changed or new: update symbol table + content index
3. Build import graph: parse `use` statements → edges
4. Save to .xaft/index/ (SQLite + serialized graph)
5. Report: N files, M symbols, K edges
```

### Incremental Indexing

`xaft` watches the filesystem during a session and updates the index incrementally:

```rust
pub async fn watch_and_reindex(
    watcher: RecommendedWatcher,
    index: Arc<RepoIndex>,
    signal_bus: Arc<SignalBus>,
) {
    let mut events = watcher.events();
    while let Some(event) = events.next().await {
        if let Some(path) = event.path() {
            if path.extension() == Some(OsStr::new("rs")) {
                let new_hash = hash_file(&path).await?;
                if index.checksums.read().await.get(&path) != Some(&new_hash) {
                    index.reindex_file(&path).await?;
                    index.checksums.write().await.insert(path, new_hash);
                }
            }
        }
    }
}
```

## Workspace Context Injection

Before each agent run, `PlanExecutor` injects workspace context into `AgentContext`:

```rust
pub async fn inject_workspace_context(
    ctx: &mut AgentContext,
    session: &XaftSession,
    step: &PlanStep,
) -> Result<(), XaftError> {
    // 1. Current worktree path
    ctx.set_context_state("worktree_root",
        serde_json::json!(session.active_worktree.read().await
            .as_ref().map(|wt| wt.path().to_string_lossy().to_string())));

    // 2. Relevant files for this step (from index)
    let relevant = session.index.search(&step.description, 10).await?;
    ctx.set_context_state("relevant_files", serde_json::to_value(&relevant)?);

    // 3. Current git status
    let status = session.git_repo.status().await?;
    ctx.set_context_state("git_status", serde_json::to_value(&status)?);

    // 4. Step metadata
    ctx.set_context_state("current_step", serde_json::json!({
        "id": step.id,
        "description": step.description,
        "tool_hint": step.tool_name,
    }));

    Ok(())
}
```

## .xaft/ Project Directory

```
{project}/.xaft/
├── config.toml          ← Project-level config overrides
├── sessions/
│   └── {session_id}.db  ← SQLite: messages, plan, task state, checkpoints
├── index/
│   ├── symbols.db        ← Tree-sitter symbol index (SQLite)
│   ├── content.db        ← Content search index
│   └── checksums.bin     ← File hash map (bincode)
└── audit/
    └── {date}.jsonl      ← Append-only audit log (one JSON event per line)
```

## References

- agtrs: `agtrs-workspace/src/{editor.rs, diff.rs, fuzzy.rs, store.rs}`
- agtrs: `agtrs-git/src/{repo.rs, worktree.rs, diff.rs}`
- Next: [Streaming Engine →](05_streaming_engine.md)