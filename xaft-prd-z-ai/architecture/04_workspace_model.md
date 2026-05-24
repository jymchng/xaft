# 04 — Workspace Model

> File editing, transactions, git integration, and workspace isolation.
> How xaft ensures every file mutation is safe, observable, and reversible.

---

## Overview

The workspace model is xaft's safety net for file operations. It ensures that:

1. **No partial writes** — File edits are transactional: they succeed completely or not at all.
2. **Changes are reversible** — Every edit can be rolled back to the pre-edit state.
3. **Git is integrated** — File changes map directly to git commits with branch isolation.
4. **Access is controlled** — Path sanitization prevents directory traversal attacks.
5. **State is observable** — The `WorkspaceStore` tracks dirty files and emits events.

The core primitives are `WorkspaceStore` (trait for file state management), `FileEditor` (transactional editing), and `GitRepo`/`WorktreeGuard` (git integration).

---

## WorkspaceStore Trait

The `WorkspaceStore` trait abstracts file system operations, enabling both in-memory (for testing) and on-disk (for production) implementations.

```rust
/// Trait for workspace file state management.
/// All file operations in xaft go through this trait.
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    // ── Read Operations ───────────────────────────────────────

    /// Read the entire contents of a file.
    async fn read_file(&self, path: &Path) -> Result<String, WorkspaceError>;

    /// Read a file with line numbers and metadata.
    async fn read_with_lines(&self, path: &Path) -> Result<FileContent, WorkspaceError>;

    /// Read a specific range of lines from a file.
    async fn read_lines(&self, path: &Path, start: u32, end: u32) -> Result<Vec<Line>, WorkspaceError>;

    /// List all files in the workspace, respecting ignore patterns.
    async fn list_files(&self) -> Result<Vec<FileEntry>, WorkspaceError>;

    /// List files matching a glob pattern.
    async fn list_files_matching(&self, pattern: &str) -> Result<Vec<FileEntry>, WorkspaceError>;

    /// Check if a file exists.
    async fn exists(&self, path: &Path) -> Result<bool, WorkspaceError>;

    /// Get file metadata (size, modified time, etc.)
    async fn metadata(&self, path: &Path) -> Result<FileMetadata, WorkspaceError>;

    // ── Write Operations (through FileEditor) ─────────────────

    /// Create a new FileEditor for transactional editing.
    fn editor(&self) -> FileEditor;

    // ── State Queries ─────────────────────────────────────────

    /// Get the set of files that have been modified since the last commit.
    fn dirty_files(&self) -> Vec<PathBuf>;

    /// Check if there are any uncommitted changes.
    fn has_uncommitted_changes(&self) -> bool;

    /// Get the workspace root directory.
    fn root(&self) -> &Path;

    // ── Search ────────────────────────────────────────────────

    /// Search for a pattern in workspace files (like grep).
    async fn grep(&self, pattern: &str, options: GrepOptions) -> Result<Vec<SearchMatch>, WorkspaceError>;

    // ── Snapshot ──────────────────────────────────────────────

    /// Create a point-in-time snapshot of the workspace state.
    async fn snapshot(&self) -> Result<WorkspaceSnapshot, WorkspaceError>;

    /// Restore the workspace to a previous snapshot.
    async fn restore_snapshot(&self, snapshot: &WorkspaceSnapshot) -> Result<(), WorkspaceError>;
}
```

### Supporting Types

```rust
/// File content with line-level detail.
#[derive(Debug, Clone)]
pub struct FileContent {
    pub path: PathBuf,
    pub lines: Vec<Line>,
    pub total_lines: u32,
    pub encoding: FileEncoding,
    pub language: Option<Language>,
}

/// A single line with its number and content.
#[derive(Debug, Clone)]
pub struct Line {
    pub number: u32,
    pub content: String,
}

/// File entry in a directory listing.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// File metadata.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub size: u64,
    pub modified: std::time::SystemTime,
    pub is_readonly: bool,
    pub encoding: FileEncoding,
}

/// Point-in-time workspace snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: SnapshotId,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub files: HashMap<PathBuf, String>,
    pub dirty_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileEncoding {
    Utf8,
    Utf8WithBom,
    Latin1,
    Binary,
}

/// Search match from grep.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line_number: u32,
    pub line_content: String,
    pub match_start: u32,
    pub match_end: u32,
}

/// Grep search options.
#[derive(Debug, Clone)]
pub struct GrepOptions {
    pub case_insensitive: bool,
    pub regex: bool,
    pub max_results: Option<u32>,
    pub file_pattern: Option<String>,
    pub context_lines: u32,
}
```

---

## Store Implementations

### InMemoryWorkspaceStore

Used primarily for testing. All files live in a `HashMap`:

```rust
/// In-memory workspace store for testing and dry-run mode.
pub struct InMemoryWorkspaceStore {
    root: PathBuf,
    files: RwLock<HashMap<PathBuf, String>>,
    dirty: RwLock<HashSet<PathBuf>>,
    ignore_patterns: Vec<glob::Pattern>,
}

#[async_trait]
impl WorkspaceStore for InMemoryWorkspaceStore {
    async fn read_file(&self, path: &Path) -> Result<String, WorkspaceError> {
        let sanitized = self.sanitize_path(path)?;
        self.files
            .read()
            .await
            .get(&sanitized)
            .cloned()
            .ok_or(WorkspaceError::FileNotFound(sanitized))
    }

    async fn read_with_lines(&self, path: &Path) -> Result<FileContent, WorkspaceError> {
        let content = self.read_file(path).await?;
        let sanitized = self.sanitize_path(path)?;
        let lines = content
            .lines()
            .enumerate()
            .map(|(i, line)| Line {
                number: i as u32 + 1,
                content: line.to_string(),
            })
            .collect();

        Ok(FileContent {
            path: sanitized,
            lines,
            total_lines: content.lines().count() as u32,
            encoding: FileEncoding::Utf8,
            language: Language::from_path(path),
        })
    }

    fn dirty_files(&self) -> Vec<PathBuf> {
        self.dirty.read().unwrap().iter().cloned().collect()
    }

    fn has_uncommitted_changes(&self) -> bool {
        !self.dirty.read().unwrap().is_empty()
    }

    // ... other methods
}
```

### OnDiskWorkspaceStore

Production implementation. Reads and writes actual files on disk, with ignore pattern support:

```rust
/// On-disk workspace store for production use.
pub struct OnDiskWorkspaceStore {
    root: PathBuf,
    ignore_patterns: Vec<glob::Pattern>,
    dirty: RwLock<HashSet<PathBuf>>,
    /// Backup storage for rollback.
    backups: RwLock<HashMap<PathBuf, String>>,
}

#[async_trait]
impl WorkspaceStore for OnDiskWorkspaceStore {
    async fn read_file(&self, path: &Path) -> Result<String, WorkspaceError> {
        let full_path = self.resolve_path(path)?;

        // Check ignore patterns
        if self.is_ignored(&full_path) {
            return Err(WorkspaceError::IgnoredFile(path.to_path_buf()));
        }

        tokio::fs::read_to_string(&full_path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => WorkspaceError::FileNotFound(path.to_path_buf()),
                std::io::ErrorKind::PermissionDenied => WorkspaceError::PermissionDenied(path.to_path_buf()),
                _ => WorkspaceError::Io(e),
            })
    }

    async fn read_with_lines(&self, path: &Path) -> Result<FileContent, WorkspaceError> {
        let content = self.read_file(path).await?;
        let full_path = self.resolve_path(path)?;

        let lines = content
            .lines()
            .enumerate()
            .map(|(i, line)| Line {
                number: i as u32 + 1,
                content: line.to_string(),
            })
            .collect();

        Ok(FileContent {
            path: full_path,
            lines,
            total_lines: content.lines().count() as u32,
            encoding: detect_encoding(&content),
            language: Language::from_path(path),
        })
    }

    async fn list_files(&self) -> Result<Vec<FileEntry>, WorkspaceError> {
        let mut entries = Vec::new();
        self.walk_dir(&self.root, &mut entries).await?;
        Ok(entries)
    }

    // ... other methods
}

impl OnDiskWorkspaceStore {
    /// Walk directory recursively, respecting ignore patterns.
    async fn walk_dir(
        &self,
        dir: &Path,
        entries: &mut Vec<FileEntry>,
    ) -> Result<(), WorkspaceError> {
        let mut read_dir = tokio::fs::read_dir(dir).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();

            if self.is_ignored(&path) {
                continue;
            }

            let metadata = entry.metadata().await?;
            entries.push(FileEntry {
                path: path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf(),
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                modified: metadata.modified().ok(),
            });

            if metadata.is_dir() {
                Box::pin(self.walk_dir(&path, entries)).await?;
            }
        }

        Ok(())
    }
}
```

### Trade-offs

| Aspect | InMemoryWorkspaceStore | OnDiskWorkspaceStore |
|---|---|---|
| Speed | Microsecond reads | Millisecond reads (disk I/O) |
| Persistence | Lost on drop | Survives process restart |
| Disk usage | None | Actual file storage |
| Concurrency | RwLock (in-process) | OS file locks |
| Testing | Perfect for unit tests | Required for integration tests |
| Large files | Memory pressure | Stream from disk |
| Use case | Tests, dry-run, sub-agents | Primary workspace |

---

## Path Sanitization

Every path that enters the workspace is sanitized to prevent directory traversal attacks:

```rust
/// Security: sanitize paths to prevent directory traversal.
impl WorkspaceStore for OnDiskWorkspaceStore {
    fn sanitize_path(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        // 1. Canonicalize the path relative to workspace root
        let full_path = self.root.join(path);

        // 2. Resolve symlinks and ..
        let canonical = full_path
            .canonicalize()
            .unwrap_or_else(|_| full_path.clone());

        // 3. Verify the path is within the workspace root
        if !canonical.starts_with(&self.root) {
            return Err(WorkspaceError::PathTraversal {
                requested: path.to_path_buf(),
                root: self.root.clone(),
            });
        }

        // 4. Check for suspicious patterns
        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(WorkspaceError::SuspiciousPath(path.to_path_buf()));
        }

        // 5. Check against blocklist
        const BLOCKED_PREFIXES: &[&str] = &[
            "/etc", "/usr", "/bin", "/sbin", "/var", "/sys", "/proc",
            "~/.ssh", "~/.gnupg", "~/.config/xaft",
        ];

        for prefix in BLOCKED_PREFIXES {
            if path_str.starts_with(prefix) {
                return Err(WorkspaceError::BlockedPath(path.to_path_buf()));
            }
        }

        // 6. Return the sanitized relative path
        Ok(canonical.strip_prefix(&self.root).unwrap_or(&canonical).to_path_buf())
    }
}
```

---

## FileEditor Transactional Model

The `FileEditor` is xaft's most critical safety mechanism. It provides transactional file editing with commit/rollback semantics.

### Core Principles

1. **Atomic edits** — Each edit operation (replace_block, apply_diff, multi_edit) either fully succeeds or has no effect.
2. **Pre-edit backup** — Before any modification, the original file content is backed up.
3. **Dirty tracking** — Uncommitted edits are tracked in the dirty set.
4. **Explicit commit/rollback** — Changes are not finalized until `commit()` is called; `rollback()` restores the original state.

### FileEditor Struct

```rust
/// Transactional file editor with commit/rollback semantics.
pub struct FileEditor {
    /// Reference to the workspace store.
    workspace: Arc<dyn WorkspaceStore>,

    /// Backups of original file content (path → content before edits).
    backups: HashMap<PathBuf, String>,

    /// Pending edits that have been applied but not committed.
    pending_edits: Vec<PendingEdit>,

    /// Files that have been modified (dirty set).
    dirty: HashSet<PathBuf>,

    /// Whether a commit has been made (editor is sealed after commit).
    committed: bool,

    /// Whether a rollback has been performed (editor is sealed after rollback).
    rolled_back: bool,
}

/// A pending edit operation.
#[derive(Debug, Clone)]
pub struct PendingEdit {
    pub path: PathBuf,
    pub operation: EditOperation,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Types of edit operations.
#[derive(Debug, Clone)]
pub enum EditOperation {
    ReplaceBlock {
        start_line: u32,
        end_line: u32,
        old_content: String,
        new_content: String,
    },
    ApplyDiff {
        hunks: Vec<DiffHunk>,
    },
    MultiEdit {
        edits: Vec<SingleEdit>,
    },
    CreateFile {
        content: String,
    },
    DeleteFile,
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SingleEdit {
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    pub new_content: String,
}
```

### Editing Operations

#### replace_block

Replace a contiguous block of lines in a file:

```rust
impl FileEditor {
    /// Replace lines [start_line, end_line] with new_content.
    /// Lines are 1-indexed, inclusive on both ends.
    ///
    /// # Example
    /// ```
    /// // Replace lines 10-15 with new code
    /// editor.replace_block(
    ///     Path::new("src/main.rs"),
    ///     10,    // start_line
    ///     15,    // end_line
    ///     "fn new_function() {\n    println!(\"hello\");\n}".to_string(),
    /// )?;
    /// ```
    pub fn replace_block(
        &mut self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        new_content: String,
    ) -> Result<(), WorkspaceError> {
        self.ensure_not_sealed()?;

        let sanitized = self.workspace.sanitize_path(path)?;
        let current = self.workspace.read_file(&sanitized).await?;

        // Backup original content (only once per file)
        self.backup_if_needed(&sanitized, &current);

        // Validate line range
        let line_count = current.lines().count() as u32;
        if start_line == 0 || end_line > line_count || start_line > end_line {
            return Err(WorkspaceError::InvalidLineRange {
                path: sanitized,
                start: start_line,
                end: end_line,
                total: line_count,
            });
        }

        // Perform the replacement
        let mut lines: Vec<String> = current.lines().map(|s| s.to_string()).collect();
        let replacement: Vec<String> = new_content.lines().map(|s| s.to_string()).collect();

        // Verify old content matches (safety check)
        let old_content: String = lines[(start_line - 1) as usize..end_line as usize]
            .join("\n");

        lines.splice(
            (start_line - 1) as usize..end_line as usize,
            replacement,
        );

        let new_file_content = lines.join("\n");

        // Write the modified content
        self.workspace.write_file_internal(&sanitized, &new_file_content)?;

        // Track the edit
        self.pending_edits.push(PendingEdit {
            path: sanitized.clone(),
            operation: EditOperation::ReplaceBlock {
                start_line,
                end_line,
                old_content,
                new_content,
            },
            timestamp: chrono::Utc::now(),
        });
        self.dirty.insert(sanitized);

        Ok(())
    }
}
```

#### apply_diff

Apply a unified diff to a file:

```rust
impl FileEditor {
    /// Apply a unified diff to a file.
    /// The diff must apply cleanly; if there are conflicts, an error is returned.
    pub fn apply_diff(
        &mut self,
        path: &Path,
        diff: &str,
    ) -> Result<DiffResult, WorkspaceError> {
        self.ensure_not_sealed()?;

        let sanitized = self.workspace.sanitize_path(path)?;
        let current = self.workspace.read_file(&sanitized).await?;

        // Backup original
        self.backup_if_needed(&sanitized, &current);

        // Parse the diff into hunks
        let hunks = parse_unified_diff(diff)?;

        // Verify all hunks apply cleanly (dry run)
        let mut lines: Vec<String> = current.lines().map(|s| s.to_string()).collect();
        for hunk in &hunks {
            if !hunk.verify(&lines)? {
                return Err(WorkspaceError::DiffConflict {
                    path: sanitized,
                    hunk: hunk.clone(),
                });
            }
        }

        // Apply hunks in reverse order (to preserve line numbers)
        let mut applied = 0;
        for hunk in hunks.iter().rev() {
            hunk.apply(&mut lines)?;
            applied += 1;
        }

        let new_content = lines.join("\n");
        self.workspace.write_file_internal(&sanitized, &new_content)?;

        self.pending_edits.push(PendingEdit {
            path: sanitized.clone(),
            operation: EditOperation::ApplyDiff { hunks },
            timestamp: chrono::Utc::now(),
        });
        self.dirty.insert(sanitized);

        Ok(DiffResult {
            hunks_applied: applied,
            lines_added: /* count + lines */,
            lines_removed: /* count - lines */,
        })
    }
}
```

#### multi_edit

Apply multiple edits to potentially different files in a single transaction:

```rust
impl FileEditor {
    /// Apply multiple edits atomically.
    /// If any edit fails, all are rolled back.
    pub fn multi_edit(
        &mut self,
        edits: Vec<SingleEdit>,
    ) -> Result<MultiEditResult, WorkspaceError> {
        self.ensure_not_sealed()?;

        // Backup all files first
        for edit in &edits {
            let sanitized = self.workspace.sanitize_path(&edit.path)?;
            if let Ok(content) = self.workspace.read_file(&sanitized).await {
                self.backup_if_needed(&sanitized, &content);
            }
        }

        // Apply edits one by one
        let mut results = Vec::new();
        let mut applied_edits = Vec::new();

        for edit in &edits {
            match self.replace_block(
                &edit.path,
                edit.start_line,
                edit.end_line,
                edit.new_content.clone(),
            ) {
                Ok(()) => {
                    applied_edits.push(edit.clone());
                    results.push(EditResult::Success { path: edit.path.clone() });
                }
                Err(e) => {
                    // Rollback all applied edits
                    for applied in applied_edits.iter().rev() {
                        self.rollback_single(&applied.path)?;
                    }
                    return Err(WorkspaceError::MultiEditFailed {
                        failed_path: edit.path.clone(),
                        error: e.to_string(),
                        rolled_back: applied_edits.len(),
                    });
                }
            }
        }

        Ok(MultiEditResult { results })
    }
}
```

### Commit and Rollback

```rust
impl FileEditor {
    /// Commit all pending edits. After commit, the editor is sealed.
    /// Committed edits are persisted to disk and cannot be undone via rollback.
    pub fn commit(mut self) -> Result<CommitResult, WorkspaceError> {
        self.ensure_not_sealed()?;

        // All pending edits are already written to disk by the editing operations.
        // Commit just finalizes the state.

        let committed_files = self.dirty.iter().cloned().collect::<Vec<_>>();
        let edit_count = self.pending_edits.len();

        // Clear backups (no longer needed)
        self.backups.clear();

        // Seal the editor
        self.committed = true;

        // Emit events
        for path in &committed_files {
            self.signal_bus.emit(FileEditCommitted {
                path: path.clone(),
                lines_changed: self.count_changed_lines(path),
            })?;
        }

        Ok(CommitResult {
            files: committed_files,
            edits: edit_count,
        })
    }

    /// Rollback all pending edits. After rollback, the editor is sealed.
    /// Restores all files to their pre-edit state.
    pub fn rollback(mut self) -> Result<RollbackResult, WorkspaceError> {
        self.ensure_not_sealed()?;

        let mut rolled_back = Vec::new();

        // Restore each file from its backup
        for (path, original_content) in &self.backups {
            self.workspace.write_file_internal(path, original_content)?;
            rolled_back.push(path.clone());

            self.signal_bus.emit(FileEditRolledBack {
                path: path.clone(),
                reason: "user rollback".to_string(),
            })?;
        }

        // For files that were created (not backed up), delete them
        for path in &self.dirty {
            if !self.backups.contains_key(path) {
                self.workspace.delete_file_internal(path)?;
                rolled_back.push(path.clone());
            }
        }

        // Seal the editor
        self.rolled_back = true;

        Ok(RollbackResult {
            files: rolled_back,
            edits_undone: self.pending_edits.len(),
        })
    }

    fn backup_if_needed(&mut self, path: &Path, content: &str) {
        // Only backup the first time we edit a file
        if !self.backups.contains_key(path) {
            self.backups.insert(path.to_path_buf(), content.to_string());
        }
    }

    fn ensure_not_sealed(&self) -> Result<(), WorkspaceError> {
        if self.committed {
            return Err(WorkspaceError::EditorSealed { reason: "committed" });
        }
        if self.rolled_back {
            return Err(WorkspaceError::EditorSealed { reason: "rolled back" });
        }
        Ok(())
    }
}
```

### Complete Edit Flow

```
Agent decides to edit src/lib.rs
    │
    ├── FileEditor::new(&workspace)
    │
    ├── editor.replace_block("src/lib.rs", 10, 15, new_code)?
    │       │
    │       ├── sanitize_path("src/lib.rs")
    │       ├── read_file("src/lib.rs") → current content
    │       ├── backup original content
    │       ├── validate line range (10-15 valid?)
    │       ├── splice new content into lines
    │       ├── write modified content to disk
    │       └── add to pending_edits + dirty set
    │
    ├── (optionally more edits)
    │
    ├── ── Branch A: Commit ──────────────────────────────
    │       │
    │       editor.commit()?
    │       ├── clear backups
    │       ├── seal editor
    │       ├── emit FileEditCommitted events
    │       └── return CommitResult { files, edits }
    │
    ├── ── Branch B: Rollback ───────────────────────────
    │       │
    │       editor.rollback()?
    │       ├── restore files from backups
    │       ├── delete newly created files
    │       ├── seal editor
    │       ├── emit FileEditRolledBack events
    │       └── return RollbackResult { files, edits_undone }
    │
    ▼
  Tool result returned to agent
```

---

## GitRepo and WorktreeGuard

### GitRepo Trait

```rust
/// Trait for git operations within the workspace.
#[async_trait]
pub trait GitRepo: Send + Sync {
    /// Initialize a new git repository.
    async fn init(&self, path: &Path) -> Result<(), GitError>;

    /// Open an existing git repository.
    async fn open(path: &Path) -> Result<Self, GitError> where Self: Sized;

    /// Create a new branch and switch to it.
    fn create_branch(&self, name: &str) -> Result<(), GitError>;

    /// Switch to an existing branch.
    fn switch_branch(&self, name: &str) -> Result<(), GitError>;

    /// Get the current branch name.
    fn current_branch(&self) -> Result<String, GitError>;

    /// Get the list of modified files.
    fn status(&self) -> Result<GitStatus, GitError>;

    /// Get a diff of uncommitted changes.
    fn diff(&self) -> Result<String, GitError>;

    /// Get a diff of a specific file.
    fn diff_file(&self, path: &Path) -> Result<String, GitError>;

    /// Stage specific files.
    fn add(&self, paths: &[&Path]) -> Result<(), GitError>;

    /// Commit staged changes with a message.
    fn commit(&self, message: &str) -> Result<CommitHash, GitError>;

    /// Stage all changes and commit.
    fn commit_all(&self, message: &str) -> Result<CommitHash, GitError>;

    /// Commit specific files with a message.
    fn commit_specific(&self, paths: &[&Path], message: &str) -> Result<CommitHash, GitError>;

    /// Get the commit log.
    fn log(&self, max_count: u32) -> Result<Vec<CommitEntry>, GitError>;

    /// Check if there are uncommitted changes.
    fn is_dirty(&self) -> Result<bool, GitError>;

    /// Restore a file to its last committed state.
    fn restore(&self, path: &Path) -> Result<(), GitError>;

    /// Stash current changes.
    fn stash(&self, message: &str) -> Result<(), GitError>;

    /// Pop a stash.
    fn stash_pop(&self) -> Result<(), GitError>;
}
```

### WorktreeGuard

The `WorktreeGuard` provides git worktree isolation for xaft tasks. When created, it switches to a dedicated branch. When dropped, it optionally restores the original branch.

```rust
/// RAII guard for git worktree isolation.
/// Creates a branch on creation; restores original state on drop.
pub struct WorktreeGuard {
    /// The git repo being guarded.
    repo: Arc<dyn GitRepo>,

    /// The branch that was active before this guard was created.
    original_branch: String,

    /// The branch created by this guard for the task.
    task_branch: String,

    /// Whether to auto-commit remaining changes on drop.
    auto_commit_on_drop: bool,

    /// Whether to restore the original branch on drop.
    restore_on_drop: bool,

    /// Whether the guard has been explicitly released.
    released: bool,
}

impl WorktreeGuard {
    /// Create a new worktree guard for a task.
    /// This creates a new branch and switches to it.
    pub fn new(
        repo: Arc<dyn GitRepo>,
        branch_name: &str,
        config: &WorktreeGuardConfig,
    ) -> Result<Self, GitError> {
        let original_branch = repo.current_branch()?;

        // Create and switch to the task branch
        repo.create_branch(branch_name)?;

        Ok(Self {
            repo,
            original_branch,
            task_branch: branch_name.to_string(),
            auto_commit_on_drop: config.auto_commit_on_drop,
            restore_on_drop: config.restore_on_drop,
            released: false,
        })
    }

    /// Commit all remaining changes on the task branch.
    pub fn commit_all(&self, message: &str) -> Result<CommitHash, GitError> {
        self.repo.commit_all(message)
    }

    /// Explicitly release the guard without restoring the original branch.
    /// Use this when you want to keep the task branch active.
    pub fn release_without_restore(mut self) {
        self.released = true;
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if self.released {
            return;
        }

        // Auto-commit if configured
        if self.auto_commit_on_drop {
            if let Ok(true) = self.repo.is_dirty() {
                let msg = format!("xaft: auto-commit on guard drop");
                let _ = self.repo.commit_all(&msg);
            }
        }

        // Restore original branch if configured
        if self.restore_on_drop {
            let _ = self.repo.switch_branch(&self.original_branch);
        }
    }
}
```

### Branch-per-Task Strategy

```
Before xaft run:
    main ──── A ──── B ──── C (HEAD)

After xaft starts task:
    main ──── A ──── B ──── C
                         \
                          └── xaft/task-fix-bug (HEAD)

After xaft completes task:
    main ──── A ──── B ──── C
                         \
                          └── xaft/task-fix-bug ──── D ──── E (committed)

After WorktreeGuard drops (restore_on_drop=true):
    main ──── A ──── B ──── C (HEAD)
                         \
                          └── xaft/task-fix-bug ──── D ──── E

User can then:
    git merge xaft/task-fix-bug    # Accept changes
    git cherry-pick E              # Accept specific commits
    git branch -D xaft/task-fix-bug  # Reject changes
```

### Auto-Commit Strategy

```rust
/// Configuration for when xaft auto-commits.
pub struct AutoCommitConfig {
    /// Commit after every N file modifications.
    pub after_n_files: usize,

    /// Commit after every N turns.
    pub after_n_turns: u32,

    /// Commit when a plan step completes.
    pub on_plan_step_complete: bool,

    /// Commit before running shell commands (checkpoint).
    pub before_shell_command: bool,

    /// Commit message prefix.
    pub prefix: String,
}

impl AutoCommitConfig {
    pub fn default() -> Self {
        Self {
            after_n_files: 3,
            after_n_turns: 5,
            on_plan_step_complete: true,
            before_shell_command: true,
            prefix: "xaft: ".to_string(),
        }
    }
}
```

---

## Workspace Snapshots

Snapshots provide point-in-time workspace state for undo/redo:

```rust
/// Snapshot manager for workspace state.
pub struct SnapshotManager {
    workspace: Arc<dyn WorkspaceStore>,
    git: Arc<dyn GitRepo>,
    snapshots: Vec<WorkspaceSnapshot>,
    max_snapshots: usize,
}

impl SnapshotManager {
    /// Create a snapshot before a risky operation.
    pub async fn checkpoint(&mut self, label: &str) -> Result<SnapshotId, WorkspaceError> {
        let snapshot = self.workspace.snapshot().await?;
        let id = snapshot.id.clone();

        self.snapshots.push(snapshot);

        // Prune old snapshots
        if self.snapshots.len() > self.max_snapshots {
            self.snapshots.remove(0);
        }

        Ok(id)
    }

    /// Restore to a specific snapshot.
    pub async fn restore(&self, id: &SnapshotId) -> Result<(), WorkspaceError> {
        let snapshot = self.snapshots.iter()
            .find(|s| &s.id == id)
            .ok_or(WorkspaceError::SnapshotNotFound(id.clone()))?;

        self.workspace.restore_snapshot(snapshot).await
    }

    /// List all available snapshots.
    pub fn list(&self) -> &[WorkspaceSnapshot] {
        &self.snapshots
    }
}
```

---

## Concurrent Access

When multiple agents share a workspace (Coordinator or Collaborate mode), xaft uses file-level locking:

```rust
/// File-level lock manager for concurrent access.
pub struct FileLockManager {
    locks: RwLock<HashMap<PathBuf, AgentId>>,
}

impl FileLockManager {
    /// Acquire a lock on a file for an agent.
    /// Returns Err if the file is already locked by another agent.
    pub fn acquire(&self, path: &Path, agent_id: &AgentId) -> Result<FileLock, LockError> {
        let mut locks = self.locks.write().unwrap();

        if let Some(owner) = locks.get(path) {
            if owner != agent_id {
                return Err(LockError::FileLocked {
                    path: path.to_path_buf(),
                    owner: owner.clone(),
                });
            }
        }

        locks.insert(path.to_path_buf(), agent_id.clone());

        Ok(FileLock {
            path: path.to_path_buf(),
            manager: self.clone(),
        })
    }
}

/// RAII guard for a file lock.
pub struct FileLock {
    path: PathBuf,
    manager: FileLockManager,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let mut locks = self.manager.locks.write().unwrap();
        locks.remove(&self.path);
    }
}
```

---

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Permission denied: {0}")]
    PermissionDenied(PathBuf),

    #[error("Path traversal detected: {0} is outside root {1}")]
    PathTraversal { requested: PathBuf, root: PathBuf },

    #[error("Suspicious path: {0}")]
    SuspiciousPath(PathBuf),

    #[error("Blocked path: {0}")]
    BlockedPath(PathBuf),

    #[error("File is ignored: {0}")]
    IgnoredFile(PathBuf),

    #[error("Invalid line range: {path} {start}-{end} (total: {total})")]
    InvalidLineRange { path: PathBuf, start: u32, end: u32, total: u32 },

    #[error("Diff conflict in {path} at hunk")]
    DiffConflict { path: PathBuf, hunk: DiffHunk },

    #[error("Multi-edit failed on {failed_path}: {error}. Rolled back {rolled_back} edits.")]
    MultiEditFailed { failed_path: PathBuf, error: String, rolled_back: usize },

    #[error("Editor is sealed ({reason})")]
    EditorSealed { reason: &'static str },

    #[error("Snapshot not found: {0}")]
    SnapshotNotFound(SnapshotId),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("Not a git repository: {0}")]
    NotARepo(PathBuf),

    #[error("Branch already exists: {0}")]
    BranchExists(String),

    #[error("Branch not found: {0}")]
    BranchNotFound(String),

    #[error("Merge conflict: {0}")]
    MergeConflict(String),

    #[error("Git command failed: {0}")]
    CommandFailed(String),

    #[error("Dirty working tree")]
    DirtyTree,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("File {path} is locked by agent {owner}")]
    FileLocked { path: PathBuf, owner: AgentId },
}
```
