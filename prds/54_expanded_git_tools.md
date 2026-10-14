# PRD 54 — Expanded Git Tool Set for xaft Agents

**Status:** Draft  
**Date:** 2026-06-12  
**Crates affected:** `xaft-tools`, `agtrs-git`  
**Author:** xaft team

---

## 1. Problem Statement

Today the Coder and QA agents have only three git tools: `git_status`, `git_diff` (HEAD-only, no options), and `git_log` (no filters). Whenever they need anything beyond these basics — staging files, committing, pushing, browsing branches, looking at stashes, understanding blame, comparing arbitrary refs, or searching tracked content — they must fall back to `bash_exec`. That has three compounding costs:

1. **Safety regression.** Every `bash_exec` call goes through the TUI approval gate. A compound git command like `git add -p && git commit -m "..."` requires a human to review opaque shell text instead of structured, typed tool inputs.

2. **Reliability regression.** Shell quoting, stdin buffering, and stderr/stdout merging all create edge cases that typed tool implementations avoid. Agents frequently generate malformed shell strings for commit messages containing apostrophes or special characters.

3. **Context window waste.** `bash_exec` returns raw terminal output. The agent has to parse `git log --oneline` prose to extract SHAs, authors, and dates — information it would receive as clean JSON from a typed tool.

The goal of this PRD is to give Coder and QA agents a complete git vocabulary through typed tools, so that `bash_exec` is never needed for any standard git workflow. The only remaining use case for `bash_exec` on git operations would be advanced/exotic subcommands (`git bisect`, `git filter-branch`, custom hooks) that are explicitly out of scope.

---

## 2. Goals

- G1: Agents can stage, commit, and push changes without `bash_exec`.
- G2: Agents can inspect diffs at any granularity (staged/unstaged/between refs/single file).
- G3: Agents can browse branches, tags, remotes, and stashes for situational awareness.
- G4: Agents can understand authorship history through blame and show-commit.
- G5: Agents can search tracked content with `git_grep` (faster than `GrepTool` on large repos).
- G6: All destructive operations require explicit human confirmation via `requires_confirmation()`.
- G7: Protected branches (`main`, `master`, `develop` by default) cannot be pushed to without confirmation and a config override.
- G8: `GitToolSet` provides a clean registry API — `.all()`, `.read_only()`, `.write_tools()` — for use in `ToolRegistryBuilder`.
- G9: All new `agtrs-git` methods needed are specified with exact Rust signatures.
- G10: All new tools include complete JSON schemas and LLM-facing descriptions.

---

## 3. Non-Goals

The following remain in `bash_exec` territory and are explicitly excluded from this PRD:

- `git bisect` (interactive; requires multiple sequential commands)
- `git filter-branch` / `git filter-repo` (history rewriting; too dangerous to automate)
- `git submodule` management
- `git rebase --interactive`
- `git cherry-pick` with conflict resolution
- `git bundle` / archive creation
- CI-level push/force-push workflows
- SSH key management or credential helpers
- Any operation that changes `.git/config` directly

---

## 4. Architecture Overview

### 4.1 Crate Boundaries

```
agtrs-git (new methods on GitRepo + new data types)
    └── xaft-tools/src/git/
            ├── diff.rs           (extended GitDiffTool — replaces existing)
            ├── log.rs            (extended GitLogTool — replaces existing)
            ├── blame.rs          (new GitBlameTool)
            ├── show.rs           (new GitShowTool)
            ├── branch.rs         (new GitBranchTool)
            ├── stash_list.rs     (new GitStashListTool)
            ├── remote.rs         (new GitRemoteTool)
            ├── git_grep.rs       (new GitGrepTool)
            ├── tag.rs            (new GitTagTool)
            ├── add.rs            (new GitAddTool)
            ├── unstage.rs        (new GitUnstageTool)
            ├── commit.rs         (new GitCommitTool — xaft-tools layer)
            ├── stash.rs          (new GitStashTool)
            ├── stash_pop.rs      (new GitStashPopTool)
            ├── checkout.rs       (new GitCheckoutTool)
            ├── push.rs           (new GitPushTool)
            ├── create_branch.rs  (new GitCreateBranchTool)
            ├── merge.rs          (new GitMergeTool)
            └── mod.rs            (re-exports + GitToolSet)
```

The existing three tools (`status.rs`, `diff.rs`, `log.rs`) in `xaft-tools/src/git/` are modified in place. All new tools follow the identical pattern established by those files.

### 4.2 GitToolSet

A new `GitToolSet` struct replaces the existing `GitTools` struct in `crates/xaft-tools/src/git/mod.rs`. It owns an `Arc<GitRepo>` and exposes:

```rust
pub struct GitToolSet {
    repo: Arc<GitRepo>,
    config: GitToolSetConfig,
}

impl GitToolSet {
    pub fn new(repo: Arc<GitRepo>) -> Self;
    pub fn with_config(repo: Arc<GitRepo>, config: GitToolSetConfig) -> Self;
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, GitError>;

    /// Read-only tools: status, diff, log, blame, show, branch, stash_list,
    /// remote, git_grep, tag.
    pub fn read_only(&self) -> Vec<Arc<ErasedTool>>;

    /// Write tools: add, unstage, commit, stash, stash_pop, checkout,
    /// push, create_branch, merge.
    pub fn write_tools(&self) -> Vec<Arc<ErasedTool>>;

    /// All tools: read_only() + write_tools().
    pub fn all(&self) -> Vec<Arc<ErasedTool>>;
}
```

### 4.3 GitToolSetConfig

```rust
/// Runtime configuration for the xaft-tools git tool set.
#[derive(Debug, Clone)]
pub struct GitToolSetConfig {
    /// Branches that require confirmation before push (default: ["main", "master", "develop"]).
    pub protected_branches: Vec<String>,
    /// Whether push always requires confirmation regardless of branch (default: true).
    pub push_requires_confirmation: bool,
    /// Whether merge always requires confirmation (default: true).
    pub merge_requires_confirmation: bool,
    /// Whether checkout (file restore) always requires confirmation (default: true).
    pub checkout_requires_confirmation: bool,
    /// Whether commit requires confirmation (default: true).
    pub commit_requires_confirmation: bool,
}

impl Default for GitToolSetConfig {
    fn default() -> Self {
        Self {
            protected_branches: vec![
                "main".into(),
                "master".into(),
                "develop".into(),
            ],
            push_requires_confirmation: true,
            merge_requires_confirmation: true,
            checkout_requires_confirmation: true,
            commit_requires_confirmation: true,
        }
    }
}
```

### 4.4 Config Section (xaft-config)

Add to `crates/xaft-config/src/types.rs`:

```rust
/// Typed configuration for the git tool group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitToolConfig {
    /// Whether the git tool group is enabled.
    pub enabled: bool,
    /// Branches that reject direct push without human confirmation.
    pub protected_branches: Vec<String>,
    /// Whether git_push always requires user confirmation.
    pub push_requires_confirmation: bool,
    /// Whether git_commit always requires user confirmation.
    pub commit_requires_confirmation: bool,
    /// Whether git_merge always requires user confirmation.
    pub merge_requires_confirmation: bool,
    /// Whether git_checkout (file restore) always requires user confirmation.
    pub checkout_requires_confirmation: bool,
    /// Maximum log entries returned by git_log.
    pub log_max_entries: usize,
    /// Maximum blame entries returned before truncation.
    pub blame_max_lines: usize,
}

impl Default for GitToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            protected_branches: vec![
                "main".into(),
                "master".into(),
                "develop".into(),
            ],
            push_requires_confirmation: true,
            commit_requires_confirmation: true,
            merge_requires_confirmation: true,
            checkout_requires_confirmation: true,
            log_max_entries: 100,
            blame_max_lines: 5_000,
        }
    }
}
```

In `XaftConfig`, the `tool` map key `"git"` deserializes into `GitToolConfig`. Example TOML:

```toml
[tool.git]
push_requires_confirmation = true
protected_branches = ["main", "master", "develop"]
commit_requires_confirmation = true
```

### 4.5 ToolRegistryBuilder changes

`build_reader()` adds all `GitToolSet::read_only()` tools.  
`build_coder()` adds all `GitToolSet::all()` tools.

The existing `GitRepo::open` + three manual registrations are replaced with a single `GitToolSet::open(root)?.read_only()` / `.all()` call.

---

## 5. agtrs-git Additions Required

The following methods do not exist in `agtrs-git` and must be added to `GitRepo` in `crates/xaft-tools/../../../agtrs/agtrs-git/src/repo.rs` (or a new `src/extended.rs` impl block). Signatures use the existing `git_cmd` helper internally.

### 5.1 New data types (add to `agtrs-git/src/diff.rs`)

```rust
/// Per-line blame entry returned by git_blame with line-range filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameEntry {
    pub line_number: usize,
    pub sha: String,
    pub author: String,
    pub date: String,       // ISO-8601
    pub content: String,
}

/// Full details of a single commit (from git show).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDetail {
    pub sha: String,
    pub author: String,
    pub author_email: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub files_changed: Vec<CommitFileStat>,
}

/// Per-file stats within a CommitDetail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFileStat {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub patch: String,
}

/// Branch listing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEntry {
    pub name: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub last_commit_sha: String,
    pub last_commit_subject: String,
}

/// Stash listing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StashEntry {
    pub index: usize,
    pub sha: String,
    pub message: String,
    pub date: String,
}

/// Remote listing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub fetch_url: String,
    pub push_url: String,
}

/// A single git-grep match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub path: String,
    pub line_number: usize,
    pub content: String,
}

/// Tag listing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagEntry {
    pub name: String,
    pub sha: String,
    pub message: String,
    pub is_annotated: bool,
}

/// Result of a git push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub pushed: bool,
    pub branch: String,
    pub remote: String,
    pub commits_pushed: usize,
}

/// Diff result for arbitrary ref pairs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDiff {
    pub unified: String,
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
    // Extended field — None when called without target context
    pub target: Option<String>,
}
```

### 5.2 New GitRepo methods

Add these to `impl GitRepo` in `agtrs-git/src/repo.rs`:

```rust
/// Show blame for a file, optionally limited to a line range.
///
/// Uses `git blame --porcelain [-L start,end] -- path`.
pub async fn blame_range(
    &self,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<Vec<BlameEntry>, GitError>;

/// Show full commit details including per-file patches.
///
/// Uses `git show --format=... --patch path_or_ref`.
pub async fn show_commit(
    &self,
    ref_: &str,
) -> Result<CommitDetail, GitError>;

/// List local (and optionally remote) branches.
///
/// Uses `git branch [-a] --format=...`.
pub async fn list_branches(
    &self,
    include_remote: bool,
) -> Result<Vec<BranchEntry>, GitError>;

/// Extended log with author/path/date/grep filters.
///
/// Uses `git log` with the applicable flags.
pub async fn log_filtered(
    &self,
    max_count: usize,
    path: Option<&str>,
    author: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    grep: Option<&str>,
    format: LogFormat,
) -> Result<Vec<LogEntry>, GitError>;

/// List all stash entries.
///
/// Uses `git stash list --format=...`.
pub async fn stash_list(&self) -> Result<Vec<StashEntry>, GitError>;

/// List all remotes with fetch and push URLs.
///
/// Uses `git remote -v` and deduplicates.
pub async fn list_remotes(&self) -> Result<Vec<RemoteEntry>, GitError>;

/// Search tracked files using `git grep`.
///
/// Uses `git grep [-i] [-E] [--] pattern [path_prefix]`.
pub async fn grep(
    &self,
    pattern: &str,
    path_prefix: Option<&str>,
    case_sensitive: bool,
    use_regex: bool,
) -> Result<Vec<GrepMatch>, GitError>;

/// List tags, optionally filtered by pattern.
///
/// Uses `git tag -l [pattern] --format=...`.
pub async fn list_tags(
    &self,
    pattern: Option<&str>,
) -> Result<Vec<TagEntry>, GitError>;

/// Diff with extended options (staged, unstaged, HEAD, or between refs).
///
/// Uses `git diff [--cached] [from_ref..to_ref] [-- path]`.
pub async fn diff_extended(
    &self,
    target: DiffTarget,
    path: Option<&str>,
    from_ref: Option<&str>,
    to_ref: Option<&str>,
) -> Result<GitDiff, GitError>;

/// Stage specific paths or all changes.
///
/// Uses `git add -- paths` or `git add -A`.
pub async fn add(
    &self,
    paths: &[&str],
    all: bool,
) -> Result<Vec<String>, GitError>;

/// Unstage specific paths or all staged changes.
///
/// Uses `git restore --staged -- paths` or `git restore --staged .`.
pub async fn unstage(
    &self,
    paths: &[&str],
    all: bool,
) -> Result<Vec<String>, GitError>;

/// Stash working-tree changes.
///
/// Uses `git stash push [-u] [-m message]`.
pub async fn stash_push(
    &self,
    message: Option<&str>,
    include_untracked: bool,
) -> Result<StashEntry, GitError>;

/// Pop a stash entry.
///
/// Uses `git stash pop [stash@{index}]`.
pub async fn stash_pop(
    &self,
    index: usize,
) -> Result<Vec<String>, GitError>;

/// Restore specific files from HEAD (discards local changes).
///
/// Uses `git checkout HEAD -- paths`.
pub async fn checkout_files(
    &self,
    paths: &[&str],
) -> Result<(), GitError>;

/// Push the current branch to its upstream or a named remote.
///
/// Uses `git push [--force] [-u remote branch]`.
pub async fn push(
    &self,
    remote: Option<&str>,
    force: bool,
    set_upstream: bool,
) -> Result<PushResult, GitError>;

/// Create a new branch, optionally checking it out.
///
/// Uses `git branch name [from_ref]` + optionally `git checkout name`.
pub async fn create_branch(
    &self,
    name: &str,
    from_ref: Option<&str>,
    checkout: bool,
) -> Result<(), GitError>;

/// Merge a branch into the current branch.
///
/// Uses `git merge [--squash|--no-ff] [-m message] branch`.
/// Returns conflict file paths if merge fails with conflicts.
pub async fn merge(
    &self,
    branch: &str,
    strategy: MergeStrategy,
    message: Option<&str>,
) -> Result<MergeResult, GitError>;

/// Count commits on `branch` that are not reachable from `upstream`.
///
/// Used by git_push to populate `commits_pushed` in PushResult.
pub async fn count_commits_ahead(
    &self,
    branch: &str,
    upstream: &str,
) -> Result<usize, GitError>;
```

### 5.3 Supporting enums (add to `agtrs-git/src/diff.rs`)

```rust
/// Which side of the working tree to diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffTarget {
    /// `git diff HEAD` — all changes vs last commit (staged + unstaged).
    Head,
    /// `git diff` — unstaged changes only.
    Unstaged,
    /// `git diff --cached` — staged changes only.
    Staged,
}

impl Default for DiffTarget {
    fn default() -> Self { DiffTarget::Head }
}

/// Merge strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

impl Default for MergeStrategy {
    fn default() -> Self { MergeStrategy::Merge }
}

/// Result of a merge attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    pub merged: bool,
    pub conflicts: Vec<String>,
}

/// Log format detail level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// One line per commit: sha, author, date, subject.
    Oneline,
    /// Full entry: sha, author, date, subject, body.
    Detailed,
}

impl Default for LogFormat {
    fn default() -> Self { LogFormat::Detailed }
}
```

Additional `GitError` variants needed:

```rust
// In agtrs-git/src/error.rs — add to GitError:

/// Push would overwrite remote history without --force.
#[error("push rejected: remote has commits not in local branch — use force=true to override")]
PushRejected,

/// Push to a protected branch was blocked.
#[error("push to protected branch '{branch}' is blocked by configuration")]
ProtectedBranch {
    branch: String,
},

/// Merge conflict detected.
#[error("merge conflict in {count} file(s): {files}")]
MergeConflict {
    count: usize,
    files: String,
},

/// Stash is empty.
#[error("stash is empty — nothing to pop")]
StashEmpty,

/// Invalid branch name.
#[error("invalid branch name '{name}': {reason}")]
InvalidBranchName {
    name: String,
    reason: String,
},
```

---

## 6. Tool Specifications

Each tool specification follows this structure:
- Struct definition and `TOOL_NAME`
- `description()` — the string the LLM sees
- `schema()` — complete JSON Schema object
- `requires_confirmation()` — value and justification
- Core implementation notes
- Edge cases

---

### 6.1 GitDiffTool (extended — replaces existing)

**File:** `crates/xaft-tools/src/git/diff.rs`

```rust
pub struct GitDiffTool {
    repo: Arc<GitRepo>,
}
impl GitDiffTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_diff";
}
```

**description:**
```
Show a unified diff. By default shows all changes since HEAD (git diff HEAD).
Use 'target' to select staged-only, unstaged-only, or HEAD changes.
Use 'path' to limit the diff to one file.
Use 'from_ref' and 'to_ref' to compare two arbitrary commits or branches.
Returns the raw unified diff text followed by a summary line.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "target": {
      "type": "string",
      "enum": ["head", "staged", "unstaged"],
      "default": "head",
      "description": "Which changes to diff: 'head' (all changes vs HEAD), 'staged' (git diff --cached), 'unstaged' (git diff, working tree only). Ignored if from_ref/to_ref are set."
    },
    "path": {
      "type": "string",
      "description": "Limit diff to this repo-relative file path. Optional."
    },
    "from_ref": {
      "type": "string",
      "description": "Start commit/branch/tag for a ref-to-ref diff. Requires to_ref."
    },
    "to_ref": {
      "type": "string",
      "description": "End commit/branch/tag for a ref-to-ref diff. Requires from_ref."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only operation.

**Implementation:**
- If `from_ref` and `to_ref` are both set: call `self.repo.diff_extended(DiffTarget::Head, path, Some(from_ref), Some(to_ref))`.
- If only `from_ref` is set: return error `"to_ref is required when from_ref is provided"`.
- Otherwise: parse `target` (default `"head"`) and call `self.repo.diff_extended(target, path, None, None)`.
- Empty diff → return `"No differences found."`.

**Edge cases:**
- Invalid ref names → propagate `GitError::OperationFailed` as `ToolResult::error`.
- Binary files in diff → include the `Binary files ... differ` line verbatim.
- `path` containing `..` → reject via `validate_path`.

---

### 6.2 GitBlameTool

**File:** `crates/xaft-tools/src/git/blame.rs`

```rust
pub struct GitBlameTool {
    repo: Arc<GitRepo>,
}
impl GitBlameTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_blame";
}
```

**description:**
```
Show per-line authorship for a file (git blame). Returns a JSON array of
{line_number, sha, author, date, content} entries. Use start_line/end_line
to limit the output to a range. Useful for understanding why code was written.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Repo-relative path to the file to blame."
    },
    "start_line": {
      "type": "integer",
      "minimum": 1,
      "description": "First line of the range to annotate (1-indexed). Optional."
    },
    "end_line": {
      "type": "integer",
      "minimum": 1,
      "description": "Last line of the range to annotate (1-indexed, inclusive). Optional."
    }
  },
  "required": ["path"],
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Validate `path` via `validate_path`.
- Call `self.repo.blame_range(path, start_line, end_line).await`.
- Return `serde_json::to_string_pretty(&entries)`.
- If `start_line > end_line`: return `ToolResult::error("start_line must be <= end_line")`.

**Edge cases:**
- File not tracked → `GitError::OperationFailed` → `ToolResult::error("file is not tracked by git: <path>")`.
- Binary file → `ToolResult::error("git_blame does not support binary files")`.
- Line range extends past EOF → git truncates silently; propagate whatever git returns.

---

### 6.3 GitShowTool

**File:** `crates/xaft-tools/src/git/show.rs`

```rust
pub struct GitShowTool {
    repo: Arc<GitRepo>,
}
impl GitShowTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_show";
}
```

**description:**
```
Show full details of a commit: SHA, author, date, subject, body, and per-file
patch hunks. Input is any ref: full or short SHA, branch name, tag, or relative
ref like HEAD~2. Returns JSON with {sha, author, author_email, date, subject,
body, files_changed: [{path, additions, deletions, patch}]}.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "ref": {
      "type": "string",
      "description": "Commit reference: SHA (full or short), branch, tag, or relative ref like HEAD~2."
    }
  },
  "required": ["ref"],
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.show_commit(ref_).await`.
- Return `serde_json::to_string_pretty(&detail)`.

**`agtrs-git` implementation notes for `show_commit`:**
```
git show --format="%H|%an|%ae|%ai|%s|%b" --patch <ref>

Parse the header block (up to first diff hunk) to extract sha/author/email/date/subject/body.
Then parse each `diff --git` block to extract path, +/- counts (from @@ lines), and patch text.
```

**Edge cases:**
- Unknown ref → `ToolResult::error("unknown revision or path: <ref>")`.
- Merge commit → `files_changed` lists all files from all parents; `patch` may be empty for trivial merges.

---

### 6.4 GitBranchTool

**File:** `crates/xaft-tools/src/git/branch.rs`

```rust
pub struct GitBranchTool {
    repo: Arc<GitRepo>,
}
impl GitBranchTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_branch";
}
```

**description:**
```
List git branches. By default lists local branches only. Set all=true to include
remote-tracking branches. Returns JSON: {current: "branch-name", branches: [{name,
is_current, is_remote, last_commit_sha, last_commit_subject}]}.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "all": {
      "type": "boolean",
      "default": false,
      "description": "Include remote-tracking branches (git branch -a). Default: false."
    },
    "verbose": {
      "type": "boolean",
      "default": false,
      "description": "Include last_commit_sha and last_commit_subject for each branch. Default: false."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.list_branches(all).await`.
- Assemble `{current, branches}` JSON object.
- If `verbose=false`, omit `last_commit_sha` and `last_commit_subject` from each entry to keep output compact.

**`agtrs-git` implementation notes for `list_branches`:**
```
git branch [--all] --format='%(refname:short)|%(HEAD)|%(objectname:short)|%(subject)'
Parse each line: is_current = (HEAD == '*'), is_remote = name.starts_with("remotes/").
```

**Edge cases:**
- Detached HEAD → `current` is `"(HEAD detached at <sha>)"`.
- Empty repo (no commits) → return `{current: null, branches: []}`.

---

### 6.5 GitLogTool (extended — replaces existing)

**File:** `crates/xaft-tools/src/git/log.rs`

```rust
pub struct GitLogTool {
    repo: Arc<GitRepo>,
}
impl GitLogTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_log";
}
```

**description:**
```
Show commit history. Returns JSON array of {sha, author, date, subject} (oneline
format) or {sha, author, author_email, date, subject, body} (detailed format).
Filter by file path, author name, date range, or commit message grep. Set max to
control result count (default 10, max 100).
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "max": {
      "type": "integer",
      "minimum": 1,
      "maximum": 100,
      "default": 10,
      "description": "Maximum number of commits to return."
    },
    "path": {
      "type": "string",
      "description": "Only show commits touching this file or directory."
    },
    "author": {
      "type": "string",
      "description": "Filter to commits by this author (substring match on name or email)."
    },
    "since": {
      "type": "string",
      "description": "Only show commits after this date or ref (e.g. '2024-01-01', 'v1.0.0')."
    },
    "until": {
      "type": "string",
      "description": "Only show commits before this date or ref."
    },
    "grep": {
      "type": "string",
      "description": "Only show commits whose message matches this pattern."
    },
    "format": {
      "type": "string",
      "enum": ["oneline", "detailed"],
      "default": "detailed",
      "description": "Output verbosity. 'oneline' omits body; 'detailed' includes body."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.log_filtered(max, path, author, since, until, grep, format).await`.
- Serialize result.

**`agtrs-git` implementation notes for `log_filtered`:**
```
Build git args dynamically:
  ["log", "-<max>"]
  + ["--author=<author>"] if Some
  + ["--since=<since>"] if Some
  + ["--until=<until>"] if Some
  + ["--grep=<grep>"] if Some
  + ["--format=%h|%an|%ae|%ai|%s|%b%x00"] (null-delimited entries)
  + ["--", "<path>"] if Some

Parse on null-byte delimiter to handle multi-line bodies correctly.
```

**Edge cases:**
- No commits match filters → return `[]`.
- `path` is a directory → git log shows all commits touching any file under it.
- `grep` and `author` combined → both conditions must match (AND semantics, git default).

---

### 6.6 GitStashListTool

**File:** `crates/xaft-tools/src/git/stash_list.rs`

```rust
pub struct GitStashListTool {
    repo: Arc<GitRepo>,
}
impl GitStashListTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_stash_list";
}
```

**description:**
```
List all stash entries. Returns JSON array of {index, sha, message, date}.
index 0 is the most recent stash (git stash list). Use git_stash_pop with
the index to restore a specific stash.
```

**schema:**
```json
{
  "type": "object",
  "properties": {},
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.stash_list().await`.
- Return serialized array or `"[]  (stash is empty)"` if empty.

**`agtrs-git` implementation notes for `stash_list`:**
```
git stash list --format="%gd|%H|%s|%ai"
Parse: index from "stash@{N}" → N, sha is short (first 8), message, date.
```

---

### 6.7 GitRemoteTool

**File:** `crates/xaft-tools/src/git/remote.rs`

```rust
pub struct GitRemoteTool {
    repo: Arc<GitRepo>,
}
impl GitRemoteTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_remote";
}
```

**description:**
```
List configured git remotes. Returns JSON: {remotes: [{name, fetch_url, push_url}]}.
Useful to verify that an 'origin' remote exists before attempting git_push,
or to find the correct remote name.
```

**schema:**
```json
{
  "type": "object",
  "properties": {},
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.list_remotes().await`.
- Return `{remotes: [...]}`.
- Empty → `{remotes: []}` with message `"No remotes configured."`.

**`agtrs-git` implementation notes for `list_remotes`:**
```
git remote -v
Output has two lines per remote: "<name>\t<url> (fetch)" and "<name>\t<url> (push)".
Deduplicate by name; set fetch_url and push_url separately.
```

---

### 6.8 GitGrepTool

**File:** `crates/xaft-tools/src/git/git_grep.rs`

```rust
pub struct GitGrepTool {
    repo: Arc<GitRepo>,
}
impl GitGrepTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_grep";
}
```

**description:**
```
Search content of tracked files using git grep (faster than the workspace grep
tool on large repositories because it uses git's index). Returns results in the
same format as the 'grep' tool: "path:line_number: content". Set use_regex=true
for extended regex (ERE). Set case_sensitive=false for case-insensitive matching.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Pattern to search for."
    },
    "path_prefix": {
      "type": "string",
      "description": "Limit search to files under this directory prefix. Optional."
    },
    "case_sensitive": {
      "type": "boolean",
      "default": true,
      "description": "Case-sensitive search. Default: true."
    },
    "use_regex": {
      "type": "boolean",
      "default": false,
      "description": "Treat pattern as an extended regular expression (ERE). Default: false."
    },
    "max_matches": {
      "type": "integer",
      "minimum": 1,
      "maximum": 1000,
      "default": 100,
      "description": "Maximum matches to return."
    }
  },
  "required": ["pattern"],
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Validate `pattern` is non-empty.
- Call `self.repo.grep(pattern, path_prefix, case_sensitive, use_regex).await`.
- Truncate at `max_matches` and append a truncation notice if needed.
- Output format per line: `"path:line_number: content"`.

**`agtrs-git` implementation notes for `grep`:**
```
git grep -n [-i] [-E] [--] <pattern> [<path_prefix>]
Parse output lines: "path:line_number:content" (git grep default with -n).
```

**Edge cases:**
- Pattern contains shell metacharacters → pass as positional argument after `--`, never via shell interpolation.
- Untracked files are not searched (by design — use `grep` tool for those).
- Empty repo → return empty results gracefully.

---

### 6.9 GitTagTool

**File:** `crates/xaft-tools/src/git/tag.rs`

```rust
pub struct GitTagTool {
    repo: Arc<GitRepo>,
}
impl GitTagTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_tag";
}
```

**description:**
```
List git tags, optionally filtered by a glob pattern. Returns JSON array of
{name, sha, message, is_annotated}. Annotated tags include a message;
lightweight tags have an empty message. Useful for identifying release versions.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Glob pattern to filter tags (e.g. 'v1.*'). Optional."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — read-only.

**Implementation:**
- Call `self.repo.list_tags(pattern).await`.
- Empty → `"[]  (no tags)"`.

**`agtrs-git` implementation notes for `list_tags`:**
```
git tag -l [pattern] --format='%(refname:short)|%(objectname:short)|%(subject)|%(objecttype)'
is_annotated = (objecttype == "tag").
```

---

### 6.10 GitAddTool

**File:** `crates/xaft-tools/src/git/add.rs`

```rust
pub struct GitAddTool {
    repo: Arc<GitRepo>,
}
impl GitAddTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_add";
}
```

**description:**
```
Stage files for the next commit. Provide 'paths' to stage specific files, or
set 'all=true' to stage all changes including untracked files (equivalent to
git add -A). Returns {staged: [paths], count: N}. Staging is reversible with
git_unstage and does not require confirmation.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "paths": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1,
      "description": "Specific file paths to stage. Mutually exclusive with all=true."
    },
    "all": {
      "type": "boolean",
      "default": false,
      "description": "Stage all changes including untracked files (git add -A). Mutually exclusive with paths."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — staging is reversible; `git_unstage` can undo it.

**Implementation:**
- If both `paths` and `all=true` are provided: return `ToolResult::error("provide either 'paths' or 'all', not both")`.
- If neither: return `ToolResult::error("provide either 'paths' or 'all=true'")`.
- Validate each path via `validate_path`.
- Call `self.repo.add(paths, all).await`.
- Return `{staged: [...], count: N}`.

**Edge cases:**
- Path does not exist: `ToolResult::error("path not found: <path>")`.
- Nothing new to stage (already staged): succeed silently — git add is idempotent.

---

### 6.11 GitUnstageTool

**File:** `crates/xaft-tools/src/git/unstage.rs`

```rust
pub struct GitUnstageTool {
    repo: Arc<GitRepo>,
}
impl GitUnstageTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_unstage";
}
```

**description:**
```
Remove files from the staging area (git restore --staged). Provide 'paths' to
unstage specific files or set 'all=true' to unstage everything. Does not
modify working-tree content — only the index. Does not require confirmation.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "paths": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1,
      "description": "Specific file paths to unstage."
    },
    "all": {
      "type": "boolean",
      "default": false,
      "description": "Unstage all staged changes."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — only modifies the index, not working-tree files.

**Implementation:**
- Same mutual exclusion check as `GitAddTool`.
- Validate each path.
- Call `self.repo.unstage(paths, all).await`.
- Return `{unstaged: [...], count: N}`.

**`agtrs-git` notes for `unstage`:**
```
git restore --staged -- paths
or
git restore --staged .
```

---

### 6.12 GitCommitTool

**File:** `crates/xaft-tools/src/git/commit.rs`

Note: this is the `xaft-tools`-layer commit tool, distinct from `agtrs_git::tools::GitCommitTool` which is WorktreeGuard-backed. The xaft-tools version operates on `GitRepo` directly and is registered in `build_coder()`.

```rust
pub struct GitCommitTool {
    repo: Arc<GitRepo>,
}
impl GitCommitTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_commit";
}
```

**description:**
```
Commit staged changes. Requires a non-empty commit message. Will fail if nothing
is staged (use git_add first). Set allow_empty=true only for intentional empty
commits. Returns {sha, subject, files_changed: N, lines_added: N, lines_removed: N}.
This operation requires confirmation and appears in the TUI approval gate.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "message": {
      "type": "string",
      "minLength": 1,
      "description": "Commit message. Must not be empty."
    },
    "allow_empty": {
      "type": "boolean",
      "default": false,
      "description": "Allow committing with nothing staged (git commit --allow-empty). Default: false."
    }
  },
  "required": ["message"],
  "additionalProperties": false
}
```

**requires_confirmation:** `true` — commits change repository history and are difficult to undo cleanly once pushed.

**Implementation:**
- Reject empty `message` string even though schema has `minLength` (defensive validation).
- If `allow_empty=false`: run `git diff --cached --stat` first; if empty, return `ToolResult::error("nothing is staged — use git_add first, or set allow_empty=true")`.
- Build `CommitOptions { message: Some(message), allow_empty, .. CommitOptions::default() }`.
- Call `self.repo.commit(opts).await`.
- Return `{sha, subject, files_changed, lines_added, lines_removed}`.

**Edge cases:**
- `HookRejected` → surface hook output verbatim in `ToolResult::error`.
- Message containing only whitespace → normalize to empty and reject.
- Concurrent commit attempt → git returns `OperationFailed`; propagate.

---

### 6.13 GitStashTool

**File:** `crates/xaft-tools/src/git/stash.rs`

```rust
pub struct GitStashTool {
    repo: Arc<GitRepo>,
}
impl GitStashTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_stash";
}
```

**description:**
```
Stash current working-tree changes (git stash push). Provide an optional message
to label the stash entry. Set include_untracked=true to also stash new untracked
files. Returns {stash_sha, message, index: 0}. Does not require confirmation since
stashing is easily reversible with git_stash_pop.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "message": {
      "type": "string",
      "description": "Optional label for the stash entry."
    },
    "include_untracked": {
      "type": "boolean",
      "default": false,
      "description": "Also stash untracked files (git stash push -u). Default: false."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — stash is reversible.

**Implementation:**
- Call `self.repo.stash_push(message, include_untracked).await`.
- Return `{stash_sha, message, index: 0}`.
- If working tree is clean: `ToolResult::error("no local changes to stash")`.

**`agtrs-git` notes for `stash_push`:**
```
git stash push [-u] [-m message]
After success, run `git stash list --format="%gd|%H|%s|%ai" stash@{0}`
to get the new stash entry's SHA and message.
```

---

### 6.14 GitStashPopTool

**File:** `crates/xaft-tools/src/git/stash_pop.rs`

```rust
pub struct GitStashPopTool {
    repo: Arc<GitRepo>,
}
impl GitStashPopTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_stash_pop";
}
```

**description:**
```
Pop a stash entry and restore it to the working tree (git stash pop). index 0
(default) pops the most recent stash. If there are unstaged changes that would
conflict, this requires confirmation. Returns {files_restored: [paths]}.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "index": {
      "type": "integer",
      "minimum": 0,
      "default": 0,
      "description": "Which stash entry to pop (0 = most recent). Default: 0."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `true` when there are existing unstaged changes (would overwrite them). Implementation: check `git status --porcelain` for unstaged modifications before popping; if any exist, the tool was already passed to the approval gate since the gate is evaluated by the runtime before `call()` is invoked — so `requires_confirmation()` returns `true` unconditionally to be safe.

**Implementation:**
- Check stash is non-empty (run `stash_list` first; if empty, return `StashEmpty` error).
- Call `self.repo.stash_pop(index).await`.
- Return `{files_restored: [...]}`.

**`agtrs-git` notes for `stash_pop`:**
```
git stash pop stash@{index}
Parse output: lines starting with file paths from the merge output.
On conflict, GitError::MergeConflict.
```

**Edge cases:**
- `index` out of range → `ToolResult::error("stash@{index} does not exist — use git_stash_list to see available entries")`.
- Conflict on pop → `ToolResult::error("stash pop had conflicts in: <files>")`.

---

### 6.15 GitCheckoutTool

**File:** `crates/xaft-tools/src/git/checkout.rs`

```rust
pub struct GitCheckoutTool {
    repo: Arc<GitRepo>,
}
impl GitCheckoutTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_checkout";
}
```

**description:**
```
Restore one or more files to their state at HEAD, discarding all local
modifications to those files (git checkout HEAD -- paths). This is a
DESTRUCTIVE operation that permanently discards unsaved changes.
Always requires confirmation. Does NOT switch branches.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "paths": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1,
      "description": "File paths to restore from HEAD. Each path must be repo-relative."
    }
  },
  "required": ["paths"],
  "additionalProperties": false
}
```

**requires_confirmation:** `true` — discards working-tree changes permanently; cannot be undone.

**Implementation:**
- Validate each path via `validate_path`.
- Reject branch names being passed (path containing `/` is fine; bare branch names must be rejected — check `git show-ref --verify refs/heads/<path>` would succeed, meaning it's a branch).
- Call `self.repo.checkout_files(paths).await`.
- Return `{restored: paths, count: N}`.

**Edge cases:**
- Path not modified → git succeeds silently; report as restored.
- Path is untracked → `ToolResult::error("path is not tracked: <path> — use git_status to check")`.

---

### 6.16 GitPushTool

**File:** `crates/xaft-tools/src/git/push.rs`

```rust
pub struct GitPushTool {
    repo: Arc<GitRepo>,
    config: Arc<GitToolSetConfig>,
}
impl GitPushTool {
    pub fn new(repo: Arc<GitRepo>, config: Arc<GitToolSetConfig>) -> Self {
        Self { repo, config }
    }
    const TOOL_NAME: &'static str = "git_push";
}
```

**description:**
```
Push the current branch to its upstream remote (git push). Set set_upstream=true
to set the upstream tracking ref (git push -u origin <branch>) when pushing a
new branch for the first time. Set force=true only when you intentionally need
to overwrite remote history (e.g. after rebase). Always requires confirmation.
Never pushes directly to main/master/develop by default — that requires
overriding protected_branches in [tool.git] config.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "force": {
      "type": "boolean",
      "default": false,
      "description": "Force push (git push --force). Only use after rebase; overwrites remote history."
    },
    "set_upstream": {
      "type": "boolean",
      "default": false,
      "description": "Set tracking remote (git push -u origin <branch>). Use when pushing a new branch."
    },
    "remote": {
      "type": "string",
      "default": "origin",
      "description": "Remote name to push to. Default: 'origin'."
    }
  },
  "additionalProperties": false
}
```

**requires_confirmation:** `true` — modifies remote state; can trigger CI pipelines and affect collaborators.

**Implementation:**
- Resolve current branch via `self.repo.current_branch().await`.
- If current branch is in `self.config.protected_branches`: return `ToolResult::error("refusing to push to protected branch '<name>' — override [tool.git] protected_branches to allow this")`.
- Verify remote exists by checking `self.repo.list_remotes().await`; if the resolved remote is not listed, return error.
- Call `self.repo.push(remote, force, set_upstream).await`.
- Return `{pushed: true, branch, remote, commits_pushed}`.

**`agtrs-git` notes for `push`:**
```
git push [--force] [-u remote branch] or git push [--force] [remote]
To compute commits_pushed: git rev-list --count remote/branch..HEAD (before push).
If upstream not set yet: commits_pushed = git rev-list --count HEAD.
On push failure with "rejected": return GitError::PushRejected.
```

**Edge cases:**
- No upstream configured and `set_upstream=false`: `ToolResult::error("no upstream configured — use set_upstream=true with a remote name, or run git_remote to check available remotes")`.
- `force=true` on a protected branch: still blocked (protection takes priority).
- Network error → propagate as `ToolResult::error`.

---

### 6.17 GitCreateBranchTool

**File:** `crates/xaft-tools/src/git/create_branch.rs`

```rust
pub struct GitCreateBranchTool {
    repo: Arc<GitRepo>,
}
impl GitCreateBranchTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_create_branch";
}
```

**description:**
```
Create a new git branch. By default, creates from the current HEAD and
immediately checks it out. Use from_ref to branch from a different commit,
tag, or branch. Use checkout=false to create without switching. Returns
{branch, from_ref, checked_out}.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "name": {
      "type": "string",
      "description": "New branch name. Must not contain spaces, start with '-', or end with '.git'."
    },
    "from_ref": {
      "type": "string",
      "description": "Create from this ref instead of HEAD (commit SHA, branch, or tag). Optional."
    },
    "checkout": {
      "type": "boolean",
      "default": true,
      "description": "Switch to the new branch immediately. Default: true."
    }
  },
  "required": ["name"],
  "additionalProperties": false
}
```

**requires_confirmation:** `false` — creating a branch is non-destructive.

**Implementation:**
- Validate branch name:
  - No spaces
  - Does not start with `-`
  - Does not end with `.git`
  - Does not contain `..` or `~` or `^` or `:`
  - If invalid: `ToolResult::error("invalid branch name '<name>': <reason>")`
- Call `self.repo.create_branch(name, from_ref, checkout).await`.
- Return `{branch: name, from_ref: resolved_sha, checked_out: checkout}`.

**Edge cases:**
- Branch already exists → `ToolResult::error("branch '<name>' already exists")`.
- `from_ref` not found → `ToolResult::error("unknown ref: <from_ref>")`.

---

### 6.18 GitMergeTool

**File:** `crates/xaft-tools/src/git/merge.rs`

```rust
pub struct GitMergeTool {
    repo: Arc<GitRepo>,
}
impl GitMergeTool {
    pub fn new(repo: Arc<GitRepo>) -> Self { Self { repo } }
    const TOOL_NAME: &'static str = "git_merge";
}
```

**description:**
```
Merge a branch into the current branch. strategy='merge' creates a merge commit
(default). strategy='squash' squashes all commits from the target branch into a
single staged diff (you must then call git_commit). strategy='rebase' replays
commits from the current branch on top of the target. Returns {merged: bool,
conflicts: [paths]}. Always requires confirmation.
```

**schema:**
```json
{
  "type": "object",
  "properties": {
    "branch": {
      "type": "string",
      "description": "Name of the branch to merge into the current branch."
    },
    "strategy": {
      "type": "string",
      "enum": ["merge", "squash", "rebase"],
      "default": "merge",
      "description": "How to merge. 'merge' = standard merge commit, 'squash' = squash to staged changes, 'rebase' = rebase current onto target."
    },
    "message": {
      "type": "string",
      "description": "Override the merge commit message. Optional; only relevant for strategy='merge'."
    }
  },
  "required": ["branch"],
  "additionalProperties": false
}
```

**requires_confirmation:** `true` — merges can introduce conflicts and change commit history.

**Implementation:**
- Validate `branch` exists (check `list_branches`).
- Parse `strategy` (default `merge`).
- Call `self.repo.merge(branch, strategy, message).await`.
- On `GitError::MergeConflict`: return `{merged: false, conflicts: [files]}` as a `ToolResult::error`.
- On success: return `{merged: true, conflicts: []}`.

**`agtrs-git` notes for `merge`:**
```
merge: git merge [--no-ff] [-m message] branch
squash: git merge --squash branch
rebase: git rebase branch

On merge conflict, git exits non-zero and lists conflicted files in stdout.
Parse with: git diff --name-only --diff-filter=U
```

**Edge cases:**
- `strategy='rebase'` and `message` both provided → warn that message is ignored for rebase.
- Current branch is the same as target → `ToolResult::error("cannot merge a branch into itself")`.
- Merge already in progress → `ToolResult::error("a merge is already in progress — resolve conflicts first or use git_checkout to abort")`.

---

## 7. Tool Reference Table

| # | Tool Name | File | Read/Write | requires_confirmation | Registry |
|---|-----------|------|------------|----------------------|----------|
| 1 | `git_diff` | `diff.rs` | Read | false | reader + coder |
| 2 | `git_blame` | `blame.rs` | Read | false | reader + coder |
| 3 | `git_show` | `show.rs` | Read | false | reader + coder |
| 4 | `git_branch` | `branch.rs` | Read | false | reader + coder |
| 5 | `git_log` | `log.rs` | Read | false | reader + coder |
| 6 | `git_stash_list` | `stash_list.rs` | Read | false | reader + coder |
| 7 | `git_remote` | `remote.rs` | Read | false | reader + coder |
| 8 | `git_grep` | `git_grep.rs` | Read | false | reader + coder |
| 9 | `git_tag` | `tag.rs` | Read | false | reader + coder |
| 10 | `git_add` | `add.rs` | Write | false | coder only |
| 11 | `git_unstage` | `unstage.rs` | Write | false | coder only |
| 12 | `git_commit` | `commit.rs` | Write | **true** | coder only |
| 13 | `git_stash` | `stash.rs` | Write | false | coder only |
| 14 | `git_stash_pop` | `stash_pop.rs` | Write | **true** | coder only |
| 15 | `git_checkout` | `checkout.rs` | Write | **true** | coder only |
| 16 | `git_push` | `push.rs` | Write | **true** | coder only |
| 17 | `git_create_branch` | `create_branch.rs` | Write | false | coder only |
| 18 | `git_merge` | `merge.rs` | Write | **true** | coder only |

---

## 8. Files Table

### Modified files

| File | Change |
|------|--------|
| `crates/xaft-tools/src/git/diff.rs` | Extend with `target`, `path`, `from_ref`, `to_ref` inputs |
| `crates/xaft-tools/src/git/log.rs` | Extend with `path`, `author`, `since`, `until`, `grep`, `format` inputs |
| `crates/xaft-tools/src/git/mod.rs` | Replace `GitTools` with `GitToolSet` + `GitToolSetConfig`; add all re-exports |
| `crates/xaft-tools/src/registry.rs` | Replace manual git tool registration with `GitToolSet::read_only()` / `.all()` |
| `crates/xaft-tools/src/lib.rs` | Add flat re-exports for all new git tool types |
| `crates/xaft-config/src/types.rs` | Add `GitToolConfig` struct |
| `agtrs/agtrs-git/src/diff.rs` | Add `BlameEntry.date`, `CommitDetail`, `CommitFileStat`, `BranchEntry`, `StashEntry`, `RemoteEntry`, `GrepMatch`, `TagEntry`, `PushResult`, `DiffTarget`, `MergeStrategy`, `MergeResult`, `LogFormat` |
| `agtrs/agtrs-git/src/error.rs` | Add `PushRejected`, `ProtectedBranch`, `MergeConflict`, `StashEmpty`, `InvalidBranchName` variants |
| `agtrs/agtrs-git/src/repo.rs` | Add all new methods: `blame_range`, `show_commit`, `list_branches`, `log_filtered`, `stash_list`, `list_remotes`, `grep`, `list_tags`, `diff_extended`, `add`, `unstage`, `stash_push`, `stash_pop`, `checkout_files`, `push`, `create_branch`, `merge`, `count_commits_ahead` |
| `agtrs/agtrs-git/src/lib.rs` | Add re-exports for all new types |

### New files

| File | Contents |
|------|----------|
| `crates/xaft-tools/src/git/blame.rs` | `GitBlameTool` |
| `crates/xaft-tools/src/git/show.rs` | `GitShowTool` |
| `crates/xaft-tools/src/git/branch.rs` | `GitBranchTool` |
| `crates/xaft-tools/src/git/stash_list.rs` | `GitStashListTool` |
| `crates/xaft-tools/src/git/remote.rs` | `GitRemoteTool` |
| `crates/xaft-tools/src/git/git_grep.rs` | `GitGrepTool` |
| `crates/xaft-tools/src/git/tag.rs` | `GitTagTool` |
| `crates/xaft-tools/src/git/add.rs` | `GitAddTool` |
| `crates/xaft-tools/src/git/unstage.rs` | `GitUnstageTool` |
| `crates/xaft-tools/src/git/commit.rs` | `GitCommitTool` (xaft-tools layer) |
| `crates/xaft-tools/src/git/stash.rs` | `GitStashTool` |
| `crates/xaft-tools/src/git/stash_pop.rs` | `GitStashPopTool` |
| `crates/xaft-tools/src/git/checkout.rs` | `GitCheckoutTool` |
| `crates/xaft-tools/src/git/push.rs` | `GitPushTool` |
| `crates/xaft-tools/src/git/create_branch.rs` | `GitCreateBranchTool` |
| `crates/xaft-tools/src/git/merge.rs` | `GitMergeTool` |
| `crates/xaft-tools/tests/git_tools_integration.rs` | Integration tests (see §10) |
| `agtrs/agtrs-git/src/extended.rs` | (optional) impl block for new GitRepo methods if repo.rs grows too large |

---

## 9. Acceptance Criteria

**AC-1:** `cargo test --workspace` passes with all new tools registered and all existing tests unchanged.

**AC-2:** `GitToolSet::read_only()` returns exactly 9 tools: `git_status`, `git_diff`, `git_log`, `git_blame`, `git_show`, `git_branch`, `git_stash_list`, `git_remote`, `git_grep`, `git_tag`. *(10 including tag; count adjusted in implementation.)*

**AC-3:** `GitToolSet::write_tools()` returns exactly 9 tools: `git_add`, `git_unstage`, `git_commit`, `git_stash`, `git_stash_pop`, `git_checkout`, `git_push`, `git_create_branch`, `git_merge`.

**AC-4:** `GitToolSet::all()` returns `read_only().len() + write_tools().len()` tools with no duplicates.

**AC-5:** `ToolRegistryBuilder::build_reader()` on a valid git repo returns a registry that includes all 9 read-only git tools plus the 3 fs tools (12 total with git).

**AC-6:** `ToolRegistryBuilder::build_coder()` on a valid git repo returns a registry that includes all 18 new git tools plus fs tools.

**AC-7:** `git_diff` with `target="staged"` returns only staged changes; `target="unstaged"` returns only unstaged changes; `target="head"` returns all changes since HEAD. All three are verified by integration tests with a real temp repo.

**AC-8:** `git_blame` for a known file returns a JSON array where every entry has `line_number`, `sha`, `author`, `date`, and `content` fields with non-empty values. The `line_number` values form a contiguous range.

**AC-9:** `git_commit` with `requires_confirmation() = true` — verifying that `requires_confirmation()` returns `true` at the type level (not runtime-conditional).

**AC-10:** `git_push` rejects a push to any branch listed in `GitToolSetConfig::protected_branches` with a `ToolResult::error` containing the word "protected", without ever invoking the git subprocess.

**AC-11:** `git_push` with `force=true` to a protected branch is still rejected (force does not bypass protection).

**AC-12:** `git_create_branch` rejects names containing spaces, ending with `.git`, or containing `..` with `ToolResult::error` citing the invalid character.

**AC-13:** `git_stash_pop` on an empty stash returns `ToolResult::error` containing "stash is empty".

**AC-14:** `git_checkout` with a path containing `..` returns `ToolResult::error` (path traversal rejected via `validate_path`).

**AC-15:** `git_grep` with `use_regex=true` and an invalid regex pattern returns `ToolResult::error` (git itself surfaces the error; tool propagates it).

**AC-16:** `git_merge` with `branch == current_branch` returns `ToolResult::error("cannot merge a branch into itself")` without invoking git.

**AC-17:** `git_log` with `path` pointing to a file that exists but has no commits returns `[]` (not an error).

**AC-18:** All tools check `ctx.cancel_token.is_cancelled()` before the first git subprocess call and return `Err(AgtrsError::Cancelled { .. })` if cancelled.

**AC-19:** `GitToolConfig` deserializes correctly from a TOML config with `[tool.git]` section; default values are populated for missing keys; `cargo test -p xaft-config` passes.

**AC-20:** `BlameEntry.date` is populated from `git blame --porcelain` output (the `author-time` line), not left empty.

---

## 10. Test Stubs

All integration tests live in `crates/xaft-tools/tests/git_tools_integration.rs`. Each test uses `tempfile::TempDir`, initializes a real git repo with `Command::new("git")`, and exercises the tool against it.

```rust
// Helper reused across all tests.
fn setup_repo() -> (TempDir, Arc<GitRepo>) { ... }
fn commit_file(dir: &Path, name: &str, content: &str) { ... }

// T-01: git_diff target=staged vs target=unstaged
#[tokio::test]
async fn test_git_diff_staged_vs_unstaged() {
    // Stage one file, leave another modified but unstaged.
    // staged diff contains staged file only; unstaged diff contains the other.
}

// T-02: git_diff from_ref + to_ref
#[tokio::test]
async fn test_git_diff_between_refs() {
    // Commit A, commit B; diff A..B; verify only B's changes appear.
}

// T-03: git_blame line range
#[tokio::test]
async fn test_git_blame_line_range() {
    // File with 20 lines, blame lines 5-10; verify exactly 6 entries returned.
}

// T-04: git_show
#[tokio::test]
async fn test_git_show_commit_detail() {
    // Commit a known file; git_show that SHA; verify files_changed includes the path.
}

// T-05: git_branch all=false vs all=true
#[tokio::test]
async fn test_git_branch_local_vs_all() {
    // Create a local branch and a remote-tracking ref; all=false omits remote.
}

// T-06: git_log path filter
#[tokio::test]
async fn test_git_log_path_filter() {
    // Commit file_a, then file_b; log with path=file_b returns only one entry.
}

// T-07: git_log author filter
#[tokio::test]
async fn test_git_log_author_filter() {
    // Two commits with different authors; filter by second author; only one entry.
}

// T-08: git_stash_list
#[tokio::test]
async fn test_git_stash_list_and_pop() {
    // Stash changes; stash_list returns one entry; stash_pop restores them.
}

// T-09: git_remote
#[tokio::test]
async fn test_git_remote_lists_origin() {
    // git remote add origin <url>; git_remote returns entry with name="origin".
}

// T-10: git_grep
#[tokio::test]
async fn test_git_grep_finds_tracked_content() {
    // Commit a file containing "MARKER"; git_grep "MARKER"; verify match returned.
}

// T-11: git_grep untracked files excluded
#[tokio::test]
async fn test_git_grep_ignores_untracked() {
    // Create untracked file with "MARKER"; git_grep "MARKER"; zero matches.
}

// T-12: git_add + git_unstage round-trip
#[tokio::test]
async fn test_git_add_unstage_roundtrip() {
    // Modify a file; git_add paths=[file]; status shows staged.
    // git_unstage paths=[file]; status shows unstaged again.
}

// T-13: git_commit rejects empty message
#[tokio::test]
async fn test_git_commit_rejects_empty_message() {
    // git_add all; git_commit message=""; result.is_error == true.
}

// T-14: git_commit rejects nothing-staged
#[tokio::test]
async fn test_git_commit_rejects_nothing_staged() {
    // No staged changes; git_commit message="fix"; result.is_error == true.
}

// T-15: git_create_branch validates name
#[tokio::test]
async fn test_git_create_branch_rejects_invalid_name() {
    // name="bad name with spaces"; result.is_error == true.
    // name="valid-branch"; result.is_error == false.
}

// T-16: git_push protected branch rejection
#[tokio::test]
async fn test_git_push_rejects_protected_branch() {
    // On branch "main"; push; result.is_error == true, content contains "protected".
}

// T-17: git_checkout path traversal rejection
#[tokio::test]
async fn test_git_checkout_path_traversal_rejected() {
    // paths=["../secret"]; result.is_error == true.
}

// T-18: git_merge rejects self-merge
#[tokio::test]
async fn test_git_merge_rejects_self() {
    // Current branch = "main"; merge branch="main"; result.is_error == true.
}

// T-19: GitToolSet registry counts
#[test]
fn test_gittoolset_counts() {
    // GitToolSet::open(repo_path).unwrap();
    // read_only().len() == 10; write_tools().len() == 9; all().len() == 19;
}

// T-20: cancellation propagated
#[tokio::test]
async fn test_cancelled_token_returns_err() {
    // Create cancelled CancellationToken; call any git tool; expect Err(AgtrsError::Cancelled).
}
```

---

## 11. Implementation Order

The following sequencing minimizes merge conflicts and allows incremental review:

1. **agtrs-git data types** — add new structs/enums/error variants to `diff.rs` and `error.rs`. No functional change yet.
2. **agtrs-git GitRepo methods** — add all new `impl GitRepo` methods. Wire each to `git_cmd`. Add unit tests in `agtrs-git/tests/git_integration.rs`.
3. **GitToolSet + GitToolSetConfig** — update `xaft-tools/src/git/mod.rs`. No new tool files yet.
4. **Read-only tools** (9 files in order): `blame.rs`, `show.rs`, `branch.rs`, `stash_list.rs`, `remote.rs`, `git_grep.rs`, `tag.rs`, then extend `diff.rs` and `log.rs`.
5. **Write tools** (9 files): `add.rs`, `unstage.rs`, `commit.rs`, `stash.rs`, `stash_pop.rs`, `checkout.rs`, `create_branch.rs`, `push.rs`, `merge.rs`.
6. **Registry update** — switch `ToolRegistryBuilder` to `GitToolSet`.
7. **Config types** — add `GitToolConfig` to `xaft-config`.
8. **Integration tests** — write all 20 test stubs in `git_tools_integration.rs`.
9. **Docs pass** — ensure `#![deny(missing_docs)]` is satisfied in both crates.

---

## 12. Open Questions

- **OQ-1:** Should `git_stash_pop` be `requires_confirmation=true` always, or only when there are unstaged changes? Current proposal: always, to keep the gate logic simple. If this creates friction in CI/headless mode, introduce a `force` flag.
- **OQ-2:** Should `git_show` include the full patch for every changed file or truncate large patches? Proposed limit: 100 KB of patch text total, with a truncation notice.
- **OQ-3:** The `agtrs-git` crate is in a separate repo (`/root/rust_projects/agtrs`). Semver bump needed: the new methods are additive (minor), but new `GitError` variants break exhaustive match arms downstream. Current agtrs-git dependents: only `xaft-tools`. Propose a `0.x` minor bump during the alpha phase.
- **OQ-4:** `git_merge` with `strategy='rebase'` produces no merge commit, so there is nothing to return in `CommitDetail`. Should `git_merge` return just `{merged: true, conflicts: []}` for rebase, or should it also return the rebase-created commit SHAs?
- **OQ-5:** `GitToolSetConfig` is constructed in `ToolRegistryBuilder` but `XaftConfig` is not available at that layer today. Proposal: add a `with_git_config(GitToolSetConfig)` method to `ToolRegistryBuilder` and populate it from `XaftConfig::tool["git"]` in `xaft-runtime`.
