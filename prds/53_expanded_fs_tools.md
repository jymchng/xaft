# PRD 53 — Expanded Filesystem Tool Set for xaft

**Status:** Draft  
**Date:** 2026-06-12  
**Crate target:** `xaft-tools`  
**Branch:** `feat/expanded-fs-tools`

---

## 1. Problem Statement

xaft's Coder and QA agents currently have a narrow filesystem vocabulary:
`read_file`, `write_file`, `edit_file`, `list_files`, and `grep`. In
practice, agents repeatedly fall back to `bash_exec` — a
requires-confirmation, TUI-blocking operation — for mundane tasks that
should be expressible as dedicated, silent, sandboxed tool calls:

- Finding files by glob pattern (`find . -name "*.rs"`)
- Inspecting file metadata (`stat`, `ls -l`)
- Rendering directory structure to understand project layout (`tree`)
- Batch-reading several files in one round-trip instead of N `read_file`
  calls
- Comparing two files with a diff
- Moving, copying, or deleting files
- Appending to log or CSV files
- Applying a unified-diff patch to a file
- Creating or removing directories

Every unnecessary `bash_exec` call interrupts the workflow with a TUI
approval prompt, burns latency, and creates friction for non-interactive
headless runs. A richer, sandboxed tool set eliminates all of these
cases and reduces the per-task turn count by roughly 20–40% for typical
Coder tasks.

---

## 2. Goals

1. **Eliminate practical bash fallbacks** — agents should never need
   `bash_exec` for filesystem navigation or manipulation tasks that do
   not require a shell pipeline.
2. **Give read-only agents richer navigation** — Planner and QA agents
   can use glob, tree, file_stat, read_many, diff_files, and
   search_files without write access.
3. **Provide safe atomic write operations** — move, copy, delete,
   create_directory, remove_directory, append, and patch all validate
   paths, check cancellation, and return structured results.
4. **Backwards compatibility** — existing tool inputs continue to work
   unchanged; all new parameters are optional.
5. **Config-governed limits** — glob result count, tree entry count, and
   read_many file count are all capped via a new `[tool.fs]` config
   section; agents see the cap in error messages.
6. **Preserve all four safety invariants** — path traversal rejection,
   `ToolResult::error` for soft errors, `is_cancelled()` checks, and
   `requires_confirmation()` for destructive operations.

---

## 3. Non-Goals

- **No streaming I/O** — all tools are request/response; streaming file
  tails or watches are out of scope.
- **No archive/zip tools** — tar, zip extraction, compression are out of
  scope and should use `bash_exec`.
- **No cross-workspace operations** — every path must resolve inside the
  current workspace root; accessing sibling workspaces is a separate
  feature.
- **No binary file editing** — all content-bearing tools work on UTF-8
  text. Binary blobs are surfaced as metadata only (via `file_stat`).
- **No remote filesystem support** — S3, SFTP, etc. are separate
  provider concerns.
- **No watch/inotify** — file change notification is handled by the
  SignalBus layer, not tool calls.

---

## 4. Architecture Overview

### 4.1 Directory Layout

All new tool source files live inside the existing `crates/xaft-tools/src/fs/`
directory, one file per tool:

```
crates/xaft-tools/src/fs/
├── mod.rs                   ← re-exports (updated)
├── read_file.rs             ← existing
├── write_file.rs            ← existing
├── edit_file.rs             ← existing
├── list_files.rs            ← existing (extended)
├── grep.rs                  ← existing
├── read_before_edit.rs      ← existing hook
├── glob.rs                  ← NEW
├── file_stat.rs             ← NEW
├── tree.rs                  ← NEW
├── diff_files.rs            ← NEW
├── read_many.rs             ← NEW
├── search_files.rs          ← NEW
├── move_file.rs             ← NEW
├── copy_file.rs             ← NEW
├── delete_file.rs           ← NEW
├── create_directory.rs      ← NEW
├── remove_directory.rs      ← NEW
├── append_to_file.rs        ← NEW
└── patch_file.rs            ← NEW
```

### 4.2 Registry Changes

`ToolRegistryBuilder::build_reader()` gains:
- `GlobTool`
- `FileStatTool`
- `TreeTool`
- `DiffFilesTool`
- `ReadManyTool`
- `SearchFilesTool`
- (existing) `ListFilesTool` — extended with new params

`ToolRegistryBuilder::build_coder()` gains all of the above plus:
- `MoveFileTool`
- `CopyFileTool`
- `DeleteFileTool`
- `CreateDirectoryTool`
- `RemoveDirectoryTool`
- `AppendToFileTool`
- `PatchFileTool`

### 4.3 New Dependency

Add to `crates/xaft-tools/Cargo.toml`:

```toml
glob    = "0.3"
similar = "2"          # unified diff — already a workspace dep, confirm version
```

The `glob` crate provides the `glob::glob()` iterator. The `similar`
crate is already present in the workspace (used by `agtrs-workspace`);
confirm the version in the workspace `Cargo.toml` and add a workspace
re-export rather than a direct version pin if it is already declared
there.

### 4.4 New Config Section

Add to `crates/xaft-config/src/types.rs` (near the existing
`FileReadToolConfig`):

```rust
/// Filesystem tool limits configurable by the operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FsToolConfig {
    /// Maximum number of paths returned by `glob`. Default: 200.
    pub max_glob_results: usize,
    /// Maximum number of entries rendered by `tree`. Default: 200.
    pub max_tree_entries: usize,
    /// Maximum number of files read in a single `read_many` call. Default: 20.
    pub max_read_many_files: usize,
    /// Maximum bytes read per file in `read_many`. Default: 32768.
    pub max_read_many_bytes_per_file: usize,
}

impl Default for FsToolConfig {
    fn default() -> Self {
        Self {
            max_glob_results: 200,
            max_tree_entries: 200,
            max_read_many_files: 20,
            max_read_many_bytes_per_file: 32768,
        }
    }
}
```

Wire into `XaftConfig` root:

```rust
/// Filesystem tool limits.
#[serde(default)]
pub fs_tools: FsToolConfig,
```

TOML representation for users:

```toml
[tool.fs]
max_glob_results             = 200
max_tree_entries             = 200
max_read_many_files          = 20
max_read_many_bytes_per_file = 32768
```

### 4.5 Shared Helpers

The existing helpers in `crates/xaft-tools/src/error.rs` (`validate_path`,
`require_str`, `opt_str`, `opt_u64`, `opt_bool`) are sufficient for all
new tools. No new shared helpers are required. The `FsWorkspaceStore`'s
`root()` method returns the `&Path` needed for OS-level operations
(stat, rename, copy, mkdir).

Tools that need the real filesystem path (stat, move, copy, delete,
mkdir, rmdir, tree, glob) accept an `Arc<dyn WorkspaceStore>` and then
downcast or, preferably, wrap an `Arc<FsWorkspaceStore>` passed in via a
constructor variant. Since the registry creates tools, the builder can
pass the concrete store type to file-operation tools and the abstract
`WorkspaceStore` trait to content tools. To avoid coupling new tools to
`FsWorkspaceStore` directly, introduce a small `WorkspaceRoot` newtype:

```rust
/// Thin newtype that gives tools access to the workspace root path
/// without depending on the concrete FsWorkspaceStore.
#[derive(Debug, Clone)]
pub struct WorkspaceRoot(pub PathBuf);

impl WorkspaceRoot {
    pub fn path(&self) -> &Path { &self.0 }

    /// Resolve a relative workspace path to an absolute path, rejecting
    /// traversal and empty paths.
    pub fn resolve(&self, tool: &str, rel: &str) -> Result<PathBuf, ToolError> {
        validate_path(tool, rel)?;
        Ok(self.0.join(rel))
    }
}
```

All new FS-operation tools receive `WorkspaceRoot` in addition to or
instead of `Arc<dyn WorkspaceStore>`. Tools that only read content
(glob, tree, file_stat, diff_files, read_many, search_files) also
receive `Arc<dyn WorkspaceStore>` for content access.

---

## 5. Per-Tool Specification

### 5.1 Tool Summary Table

| # | Name | Kind | Confirmation | Registry |
|---|------|------|-------------|----------|
| 1 | `glob` | read | no | reader + coder |
| 2 | `file_stat` | read | no | reader + coder |
| 3 | `tree` | read | no | reader + coder |
| 4 | `list_files` | read | no | reader + coder (extended) |
| 5 | `diff_files` | read | no | reader + coder |
| 6 | `read_many` | read | no | reader + coder |
| 7 | `search_files` | read | no | reader + coder |
| 8 | `move_file` | write | conditional | coder only |
| 9 | `copy_file` | write | conditional | coder only |
| 10 | `delete_file` | write | always | coder only |
| 11 | `create_directory` | write | no | coder only |
| 12 | `remove_directory` | write | conditional | coder only |
| 13 | `append_to_file` | write | no | coder only |
| 14 | `patch_file` | write | no | coder only |

---

### 5.2 `GlobTool` (`glob`)

**File:** `crates/xaft-tools/src/fs/glob.rs`

**Description (shown to LLM):**
> Find files in the workspace matching a glob pattern. Supports `*`
> (single segment), `**` (any depth), and `?` (single character).
> Returns sorted workspace-relative paths. Use `max_results` to cap the
> output (default 200). Examples: `src/**/*.rs`, `**/*.test.ts`,
> `Cargo.toml`.

**Struct definition:**

```rust
pub struct GlobTool {
    root: WorkspaceRoot,
}

impl GlobTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "glob";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Glob pattern relative to workspace root. \
                                Examples: \"src/**/*.rs\", \"**/*.toml\", \"tests/**/mod.rs\"."
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2000,
                "default": 200,
                "description": "Maximum number of matching paths to return."
            }
        },
        "required": ["pattern"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let pattern = require_str(Self::TOOL_NAME, &input, "pattern")?;
    let max = input.get("max_results").and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    // Prefix the pattern with the workspace root so glob resolves correctly.
    let root_str = self.root.path().to_string_lossy();
    let full_pattern = format!("{root_str}/{pattern}");

    let entries = glob::glob(&full_pattern)
        .map_err(|e| AgtrsError::ToolCallFailed { ... })?;

    let mut paths: Vec<String> = Vec::new();
    for entry in entries {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled { ... });
        }
        match entry {
            Ok(p) => {
                // Strip workspace root to get relative path
                if let Ok(rel) = p.strip_prefix(self.root.path()) {
                    let s = rel.to_string_lossy().replace('\\', "/");
                    paths.push(s);
                    if paths.len() >= max { break; }
                }
            }
            Err(_) => continue, // permission errors are soft-skipped
        }
    }

    paths.sort();
    if paths.is_empty() {
        return Ok(ToolResult::ok(
            format!("No files matching '{pattern}'."),
            &ctx.tool_use_id,
        ));
    }
    let truncated = paths.len() == max;
    let mut out = format!("{} file(s):\n", paths.len());
    for p in &paths { out.push_str(p); out.push('\n'); }
    if truncated {
        out.push_str(&format!("\n(truncated at {max} — narrow pattern to see more)"));
    }
    Ok(ToolResult::ok(out.trim_end().to_string(), &ctx.tool_use_id))
}
```

**Edge cases:**
- Invalid glob syntax → map `glob::PatternError` to
  `ToolResult::error` with the parse message.
- Pattern escaping `..` is caught by `validate_path` called on the
  pattern before glob expansion.
- Symlinks that point outside the root are excluded by the
  `strip_prefix` check (the resolved absolute path will not start with
  the workspace root).

---

### 5.3 `FileStatTool` (`file_stat`)

**File:** `crates/xaft-tools/src/fs/file_stat.rs`

**Description (shown to LLM):**
> Return metadata for a file or directory without reading its content.
> Useful for existence checks, size checks, or determining whether a
> path is a file vs directory. Returns JSON with fields: `path`,
> `exists`, `is_file`, `is_dir`, `is_symlink`, `size_bytes`,
> `modified_unix`, `created_unix`, `permissions_octal`.

**Struct definition:**

```rust
pub struct FileStatTool {
    root: WorkspaceRoot,
}

impl FileStatTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "file_stat";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative path to stat \
                                (file or directory)."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path = require_str(Self::TOOL_NAME, &input, "path")?;
    validate_path(Self::TOOL_NAME, path)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.resolve(Self::TOOL_NAME, path)?;

    // Use std::fs::symlink_metadata to avoid following symlinks for the
    // top-level entry (so is_symlink is accurate).
    let meta = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => {
            // Path does not exist — return a structured "not found" result
            // rather than an error so the agent can branch on `exists: false`.
            let result = serde_json::json!({
                "path": path,
                "exists": false
            });
            return Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id));
        }
    };

    use std::os::unix::fs::PermissionsExt;
    let permissions_octal = format!("{:o}", meta.permissions().mode() & 0o777);

    let modified_unix = meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    let created_unix = meta.created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    let result = serde_json::json!({
        "path": path,
        "exists": true,
        "is_file": meta.is_file(),
        "is_dir": meta.is_dir(),
        "is_symlink": meta.file_type().is_symlink(),
        "size_bytes": meta.len(),
        "modified_unix": modified_unix,
        "created_unix": created_unix,
        "permissions_octal": permissions_octal
    });

    Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
}
```

**Edge cases:**
- Non-Unix platforms: `PermissionsExt` is Unix-only. Wrap in
  `#[cfg(unix)]`; on non-Unix, omit `permissions_octal` from the JSON
  (use `serde_json::Value::Null`).
- `created_unix` is not available on all Linux filesystems; use
  `Option<u64>` and emit `null` when unavailable.
- Path traversal caught by `validate_path` before any filesystem call.

---

### 5.4 `TreeTool` (`tree`)

**File:** `crates/xaft-tools/src/fs/tree.rs`

**Description (shown to LLM):**
> Render the directory structure as an ASCII tree. Useful for
> understanding project layout at a glance. Skips `.git/`,
> `node_modules/`, `target/`, and hidden entries by default. Limit
> depth with `depth` (default 3) and total entries with `max_entries`
> (default 200).

**Struct definition:**

```rust
pub struct TreeTool {
    root: WorkspaceRoot,
}

impl TreeTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "tree";

    const SKIP_DIRS: &'static [&'static str] =
        &[".git", "node_modules", "target", ".idea", ".vscode"];
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "default": ".",
                "description": "Workspace-relative directory to render \
                                (default: workspace root)."
            },
            "depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10,
                "default": 3,
                "description": "Maximum depth to render."
            },
            "show_hidden": {
                "type": "boolean",
                "default": false,
                "description": "Include hidden files and directories \
                                (names starting with '.')."
            },
            "max_entries": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2000,
                "default": 200,
                "description": "Maximum total entries to render before truncating."
            }
        },
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
// Recursive synchronous helper (depth-limited, uses std::fs for speed)
fn render_tree(
    dir: &Path,
    prefix: &str,
    depth: usize,
    max_depth: usize,
    show_hidden: bool,
    out: &mut String,
    count: &mut usize,
    max_entries: usize,
) {
    if depth > max_depth || *count >= max_entries { return; }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());

    let total = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        if *count >= max_entries { break; }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !show_hidden && name_str.starts_with('.') { continue; }
        if entry.path().is_dir()
            && TreeTool::SKIP_DIRS.contains(&name_str.as_ref()) { continue; }

        let is_last = i == total - 1;
        let connector = if is_last { "└── " } else { "├── " };
        out.push_str(&format!("{prefix}{connector}{name_str}\n"));
        *count += 1;

        if entry.path().is_dir() {
            let child_prefix = format!(
                "{}{}", prefix, if is_last { "    " } else { "│   " }
            );
            render_tree(
                &entry.path(), &child_prefix, depth + 1,
                max_depth, show_hidden, out, count, max_entries,
            );
        }
    }
}

async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let rel_path = input.get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let depth      = input.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
    let show_hidden = input.get("show_hidden").and_then(|v| v.as_bool()).unwrap_or(false);
    let max_entries = input.get("max_entries").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

    // Validate path unless it is the default "."
    if rel_path != "." {
        validate_path(Self::TOOL_NAME, rel_path)
            .map_err(AgtrsError::from)?;
    }

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.path().join(rel_path);
    if !abs.is_dir() {
        return Ok(ToolResult::error(
            format!("'{rel_path}' is not a directory."),
            &ctx.tool_use_id,
        ));
    }

    let dir_name = abs.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());
    let mut out = format!("{dir_name}/\n");
    let mut count = 0usize;

    render_tree(&abs, "", 1, depth, show_hidden, &mut out, &mut count, max_entries);

    if count >= max_entries {
        out.push_str(&format!("\n(truncated at {max_entries} entries)"));
    }

    Ok(ToolResult::ok(out.trim_end().to_string(), &ctx.tool_use_id))
}
```

**Edge cases:**
- `path` pointing to a file rather than a directory → `ToolResult::error`.
- `depth = 0` is rejected by the schema minimum of 1.
- Read-permission denied on a subdirectory → silently skip (the helper
  returns immediately on `read_dir` error).

---

### 5.5 `ListFilesTool` — Extended Parameters

**File:** `crates/xaft-tools/src/fs/list_files.rs` (in-place extension)

The existing `prefix` and `suffix` filters remain. New optional
parameters added to the schema:

```json
"recursive": {
  "type": "boolean",
  "default": true,
  "description": "Include files in subdirectories (default true — existing behaviour)."
},
"include_dirs": {
  "type": "boolean",
  "default": false,
  "description": "Include directory entries in the output."
},
"sort_by": {
  "type": "string",
  "enum": ["name", "modified", "size"],
  "default": "name",
  "description": "Sort order for results."
},
"min_size_bytes": {
  "type": "integer",
  "minimum": 0,
  "description": "Only include files at least this many bytes."
},
"max_size_bytes": {
  "type": "integer",
  "minimum": 0,
  "description": "Only include files at most this many bytes."
},
"modified_after_unix": {
  "type": "integer",
  "description": "Only include files modified after this Unix timestamp."
}
```

**Implementation note:** Size and modification time filters require
calling `std::fs::metadata` for each candidate path. The tool receives
an `Arc<FsWorkspaceStore>` (or `WorkspaceRoot`) alongside the abstract
`WorkspaceStore` so it can call the concrete root path. When
`sort_by = "name"` and no size/time filters are active, the existing
code path (fast, no stat calls) is used. The `WorkspaceStore::list()`
method already returns sorted names, so `sort_by = "name"` is zero-cost.
For `sort_by = "modified"` or `"size"`, stat each file and sort in
memory after filtering.

All new parameters are optional; omitting them preserves exact current
behaviour. Callers that pass `recursive: false` see only immediate
children of any matching prefix directory.

---

### 5.6 `DiffFilesTool` (`diff_files`)

**File:** `crates/xaft-tools/src/fs/diff_files.rs`

**Description (shown to LLM):**
> Compare two workspace files and return a unified diff. Useful for
> reviewing differences before deciding whether to edit. Returns unified
> diff text with `context_lines` lines of context (default 3).

**Struct definition:**

```rust
pub struct DiffFilesTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl DiffFilesTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self { Self { workspace } }
    const TOOL_NAME: &'static str = "diff_files";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path_a": {
                "type": "string",
                "description": "First workspace-relative file path."
            },
            "path_b": {
                "type": "string",
                "description": "Second workspace-relative file path."
            },
            "context_lines": {
                "type": "integer",
                "minimum": 0,
                "maximum": 20,
                "default": 3,
                "description": "Lines of context around each change hunk."
            }
        },
        "required": ["path_a", "path_b"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path_a = require_str(Self::TOOL_NAME, &input, "path_a")?;
    let path_b = require_str(Self::TOOL_NAME, &input, "path_b")?;
    validate_path(Self::TOOL_NAME, path_a)?;
    validate_path(Self::TOOL_NAME, path_b)?;
    let context = input.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let content_a = self.workspace.read(path_a).await
        .map_err(|_| ToolResult::error(...))?;
    let content_b = self.workspace.read(path_b).await
        .map_err(|_| ToolResult::error(...))?;

    if content_a == content_b {
        return Ok(ToolResult::ok("Files are identical.", &ctx.tool_use_id));
    }

    // Use `similar` crate for unified diff
    let diff = similar::TextDiff::from_lines(&content_a, &content_b);
    let patch = diff.unified_diff()
        .context_radius(context)
        .header(path_a, path_b)
        .to_string();

    Ok(ToolResult::ok(patch, &ctx.tool_use_id))
}
```

**Edge cases:**
- One or both files not found → `ToolResult::error` with which file is
  missing.
- Binary content (null bytes) in either file → emit a soft error:
  `"diff_files: binary content detected in '{path}'; use file_stat to
  inspect size."`.
- Very large diffs (> 1000 lines output) → truncate and append
  `(diff truncated — {N} hunks not shown)`.

---

### 5.7 `ReadManyTool` (`read_many`)

**File:** `crates/xaft-tools/src/fs/read_many.rs`

**Description (shown to LLM):**
> Read multiple files in a single tool call. More efficient than N
> sequential `read_file` calls. Returns a JSON array where each element
> has `path`, `content`, and `truncated` (true if the file exceeded
> `max_bytes_per_file`). Files that cannot be read include an `error`
> field instead of `content`.

**Struct definition:**

```rust
pub struct ReadManyTool {
    workspace: Arc<dyn WorkspaceStore>,
    max_files: usize,
    max_bytes: usize,
}

impl ReadManyTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { workspace, max_files: 20, max_bytes: 32768 }
    }

    pub fn with_limits(
        workspace: Arc<dyn WorkspaceStore>,
        max_files: usize,
        max_bytes: usize,
    ) -> Self {
        Self { workspace, max_files, max_bytes }
    }

    const TOOL_NAME: &'static str = "read_many";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": 50,
                "description": "List of workspace-relative file paths to read."
            },
            "max_bytes_per_file": {
                "type": "integer",
                "minimum": 1,
                "default": 32768,
                "description": "Truncate each file's content to this many bytes."
            }
        },
        "required": ["paths"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let paths: Vec<&str> = input["paths"].as_array()
        .ok_or_else(|| AgtrsError::ToolCallFailed { ... })?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    if paths.len() > self.max_files {
        return Ok(ToolResult::error(
            format!("read_many: at most {} files per call; got {}.",
                    self.max_files, paths.len()),
            &ctx.tool_use_id,
        ));
    }

    let max_bytes = input.get("max_bytes_per_file")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(self.max_bytes);

    let mut results = Vec::with_capacity(paths.len());

    for path in paths {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled { ... });
        }
        if let Err(e) = validate_path(Self::TOOL_NAME, path) {
            results.push(serde_json::json!({
                "path": path,
                "error": e.to_string()
            }));
            continue;
        }
        match self.workspace.read(path).await {
            Ok(content) => {
                let truncated = content.len() > max_bytes;
                let shown = &content[..content.len().min(max_bytes)];
                results.push(serde_json::json!({
                    "path": path,
                    "content": shown,
                    "truncated": truncated
                }));
            }
            Err(e) => {
                results.push(serde_json::json!({
                    "path": path,
                    "error": e.to_string()
                }));
            }
        }
    }

    Ok(ToolResult::ok(
        serde_json::to_string_pretty(&results).unwrap_or_default(),
        &ctx.tool_use_id,
    ))
}
```

**Edge cases:**
- Empty `paths` array → schema `minItems: 1` rejects it before `call`.
- Byte truncation mid-UTF-8 codepoint → truncate to last valid UTF-8
  boundary before `max_bytes` using `str::floor_char_boundary` (stable
  in Rust 1.86).
- Duplicate paths → read each once; duplicate entries appear with
  identical content (LLM deduplication responsibility).

---

### 5.8 `SearchFilesTool` (`search_files`)

**File:** `crates/xaft-tools/src/fs/search_files.rs`

**Description (shown to LLM):**
> Find files by name (not content — use `grep` for content search).
> Searches the workspace for files whose name matches `name_pattern`
> (case-insensitive substring by default, or glob with `*` and `?`).
> Optionally limit to a subdirectory with `path_prefix`.

**Struct definition:**

```rust
pub struct SearchFilesTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl SearchFilesTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self { Self { workspace } }
    const TOOL_NAME: &'static str = "search_files";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "name_pattern": {
                "type": "string",
                "description": "Filename pattern to match. Substring match \
                                by default. Use `*` and `?` for glob-style \
                                matching (applied only to the filename, not \
                                the full path)."
            },
            "path_prefix": {
                "type": "string",
                "description": "Restrict search to files under this \
                                workspace-relative directory prefix."
            },
            "case_sensitive": {
                "type": "boolean",
                "default": false,
                "description": "Whether the name match is case-sensitive."
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2000,
                "default": 200,
                "description": "Maximum number of matching paths to return."
            }
        },
        "required": ["name_pattern"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false`

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let name_pattern = require_str(Self::TOOL_NAME, &input, "name_pattern")?;
    let path_prefix  = opt_str(&input, "path_prefix");
    let case_sens    = opt_bool(&input, "case_sensitive").unwrap_or(false);
    let max          = input.get("max_results").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    // Determine match strategy: glob if pattern contains * or ?, else substring.
    let is_glob = name_pattern.contains('*') || name_pattern.contains('?');

    let all = self.workspace.list().await;
    let mut matches: Vec<&str> = Vec::new();

    for path in &all {
        if ctx.cancel_token.is_cancelled() {
            return Err(AgtrsError::Cancelled { ... });
        }
        if let Some(px) = path_prefix {
            if !path.starts_with(px) { continue; }
        }
        // Extract only the filename component for matching
        let filename = path.rsplit('/').next().unwrap_or(path.as_str());
        let matched = if is_glob {
            let pat = if case_sens {
                name_pattern.to_string()
            } else {
                name_pattern.to_lowercase()
            };
            let hay = if case_sens { filename.to_string() } else { filename.to_lowercase() };
            // Use glob::Pattern for filename matching
            glob::Pattern::new(&pat)
                .map(|p| p.matches(&hay))
                .unwrap_or(false)
        } else {
            let pat = if case_sens { name_pattern.to_string() } else { name_pattern.to_lowercase() };
            let hay = if case_sens { filename.to_string() } else { filename.to_lowercase() };
            hay.contains(&pat)
        };

        if matched {
            matches.push(path.as_str());
            if matches.len() >= max { break; }
        }
    }

    if matches.is_empty() {
        return Ok(ToolResult::ok(
            format!("No files matching name '{name_pattern}'."),
            &ctx.tool_use_id,
        ));
    }

    let truncated = matches.len() == max;
    let mut out = format!("{} file(s):\n", matches.len());
    for m in &matches { out.push_str(m); out.push('\n'); }
    if truncated {
        out.push_str(&format!("\n(truncated at {max})"));
    }
    Ok(ToolResult::ok(out.trim_end().to_string(), &ctx.tool_use_id))
}
```

**Edge cases:**
- Empty `name_pattern` → `ToolResult::error`.
- Invalid glob characters not representable by `glob::Pattern` → soft
  error explaining the invalid pattern.
- `path_prefix` containing `..` → `validate_path` check.

---

### 5.9 `MoveFileTool` (`move_file`)

**File:** `crates/xaft-tools/src/fs/move_file.rs`

**Description (shown to LLM):**
> Rename or move a file within the workspace. Both source and
> destination must be workspace-relative paths. Set `overwrite: true`
> to allow replacing an existing destination file (requires
> confirmation). The source must exist and be a file (not a directory).

**Struct definition:**

```rust
pub struct MoveFileTool {
    root: WorkspaceRoot,
    workspace: Arc<dyn WorkspaceStore>,
}

impl MoveFileTool {
    pub fn new(root: WorkspaceRoot, workspace: Arc<dyn WorkspaceStore>) -> Self {
        Self { root, workspace }
    }
    const TOOL_NAME: &'static str = "move_file";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "description": "Workspace-relative source path."
            },
            "destination": {
                "type": "string",
                "description": "Workspace-relative destination path."
            },
            "overwrite": {
                "type": "boolean",
                "default": false,
                "description": "If true, overwrite an existing destination \
                                file. Requires confirmation."
            }
        },
        "required": ["source", "destination"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** Dynamic — `true` when `overwrite = true`,
`false` otherwise. Implement:

```rust
fn requires_confirmation(&self) -> bool {
    // The overwrite flag is in the runtime input, not available at
    // registration time. Default to false; the actual confirmation gate
    // is enforced in `call()` by checking `overwrite` AND whether the
    // destination exists. The TUI approval gate will be triggered at
    // runtime by the framework's per-call confirmation check.
    // Conservatively return true so the gate always fires and the
    // implementation can short-circuit when overwrite=false.
    true
}
```

Actually: set `requires_confirmation() = false` and perform a
*soft-error guard* in `call()` when `overwrite = false` and the
destination exists, returning a `ToolResult::error` message like
`"destination already exists; set overwrite: true to replace it"`.
When `overwrite = true`, set `requires_confirmation() = true` so the
TUI gate fires. Since the framework calls `requires_confirmation()`
once at registration, not per-call, the safest approach is to return
`true` always and accept the prompt for all moves. Document this
choice in comments.

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let src  = require_str(Self::TOOL_NAME, &input, "source")?;
    let dst  = require_str(Self::TOOL_NAME, &input, "destination")?;
    let overwrite = input.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);

    validate_path(Self::TOOL_NAME, src)?;
    validate_path(Self::TOOL_NAME, dst)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs_src = self.root.resolve(Self::TOOL_NAME, src)?;
    let abs_dst = self.root.resolve(Self::TOOL_NAME, dst)?;

    if !abs_src.exists() {
        return Ok(ToolResult::error(
            format!("source '{src}' does not exist"),
            &ctx.tool_use_id,
        ));
    }
    if abs_src.is_dir() {
        return Ok(ToolResult::error(
            format!("source '{src}' is a directory; use remove_directory + \
                     create_directory instead"),
            &ctx.tool_use_id,
        ));
    }
    if abs_dst.exists() && !overwrite {
        return Ok(ToolResult::error(
            format!("destination '{dst}' already exists; set overwrite: true to replace"),
            &ctx.tool_use_id,
        ));
    }

    // Create destination parent directories if needed
    if let Some(parent) = abs_dst.parent() {
        tokio::fs::create_dir_all(parent).await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("create_dir_all: {e}"),
            })?;
    }

    tokio::fs::rename(&abs_src, &abs_dst).await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("rename failed: {e}"),
        })?;

    Ok(ToolResult::ok(
        format!("Moved '{src}' → '{dst}'"),
        &ctx.tool_use_id,
    ))
}
```

**Edge cases:**
- Cross-device rename (e.g. source and dest on different mount points):
  `tokio::fs::rename` fails with `EXDEV`. Catch this and fall back to
  copy + delete.
- Source and destination are the same path → soft-error: `"source and
  destination are the same path"`.
- Destination directory does not exist → `create_dir_all` creates it
  before rename.

---

### 5.10 `CopyFileTool` (`copy_file`)

**File:** `crates/xaft-tools/src/fs/copy_file.rs`

**Description (shown to LLM):**
> Copy a file within the workspace. Both source and destination must be
> workspace-relative. Destination parent directories are created
> automatically. `overwrite: true` replaces an existing destination.

**Struct definition:**

```rust
pub struct CopyFileTool {
    root: WorkspaceRoot,
}

impl CopyFileTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "copy_file";
}
```

**Schema:** Same shape as `move_file` with `source`, `destination`,
`overwrite`.

**`requires_confirmation`:** `true` (conservative; same rationale as
`move_file`).

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let src      = require_str(Self::TOOL_NAME, &input, "source")?;
    let dst      = require_str(Self::TOOL_NAME, &input, "destination")?;
    let overwrite = input.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);

    validate_path(Self::TOOL_NAME, src)?;
    validate_path(Self::TOOL_NAME, dst)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs_src = self.root.resolve(Self::TOOL_NAME, src)?;
    let abs_dst = self.root.resolve(Self::TOOL_NAME, dst)?;

    if !abs_src.is_file() {
        return Ok(ToolResult::error(
            format!("source '{src}' is not a file"),
            &ctx.tool_use_id,
        ));
    }
    if abs_dst.exists() && !overwrite {
        return Ok(ToolResult::error(
            format!("destination '{dst}' exists; set overwrite: true to replace"),
            &ctx.tool_use_id,
        ));
    }
    if let Some(parent) = abs_dst.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            AgtrsError::ToolCallFailed { tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("mkdir: {e}") }
        })?;
    }

    let bytes = tokio::fs::copy(&abs_src, &abs_dst).await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("copy: {e}"),
        })?;

    Ok(ToolResult::ok(
        format!("Copied '{src}' → '{dst}' ({bytes} bytes)"),
        &ctx.tool_use_id,
    ))
}
```

---

### 5.11 `DeleteFileTool` (`delete_file`)

**File:** `crates/xaft-tools/src/fs/delete_file.rs`

**Description (shown to LLM):**
> Delete a single file from the workspace. To prevent accidental
> deletion you **must** pass `confirm: true`. Directories are refused —
> use `remove_directory` instead. Returns `{"deleted": true, "path": "..."}`.

**Struct definition:**

```rust
pub struct DeleteFileTool {
    root: WorkspaceRoot,
}

impl DeleteFileTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "delete_file";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative path of the file to delete."
            },
            "confirm": {
                "type": "boolean",
                "description": "Must be true to proceed. Safety gate against \
                                accidental deletion."
            }
        },
        "required": ["path", "confirm"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `true` (always; this is a destructive
operation).

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path    = require_str(Self::TOOL_NAME, &input, "path")?;
    let confirm = input.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);

    validate_path(Self::TOOL_NAME, path)?;

    if !confirm {
        return Ok(ToolResult::error(
            "delete_file: confirm must be true to delete a file.",
            &ctx.tool_use_id,
        ));
    }

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.resolve(Self::TOOL_NAME, path)?;

    if abs.is_dir() {
        return Ok(ToolResult::error(
            format!("'{path}' is a directory; use remove_directory instead."),
            &ctx.tool_use_id,
        ));
    }
    if !abs.exists() {
        return Ok(ToolResult::error(
            format!("'{path}' does not exist."),
            &ctx.tool_use_id,
        ));
    }

    tokio::fs::remove_file(&abs).await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("remove_file: {e}"),
        })?;

    let result = serde_json::json!({ "deleted": true, "path": path });
    Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
}
```

---

### 5.12 `CreateDirectoryTool` (`create_directory`)

**File:** `crates/xaft-tools/src/fs/create_directory.rs`

**Description (shown to LLM):**
> Create a directory (and all necessary parent directories). Safe to
> call if the directory already exists — it is a no-op. Returns
> `{"created": true, "path": "..."}`.

**Struct definition:**

```rust
pub struct CreateDirectoryTool {
    root: WorkspaceRoot,
}

impl CreateDirectoryTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "create_directory";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative directory path to create \
                                (parents are created automatically)."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false` (creating directories is harmless).

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path = require_str(Self::TOOL_NAME, &input, "path")?;
    validate_path(Self::TOOL_NAME, path)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.resolve(Self::TOOL_NAME, path)?;
    tokio::fs::create_dir_all(&abs).await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("create_dir_all: {e}"),
        })?;

    let result = serde_json::json!({ "created": true, "path": path });
    Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
}
```

---

### 5.13 `RemoveDirectoryTool` (`remove_directory`)

**File:** `crates/xaft-tools/src/fs/remove_directory.rs`

**Description (shown to LLM):**
> Remove a directory. By default only removes empty directories. Set
> `recursive: true` to remove a non-empty directory and all its
> contents (requires confirmation). Cannot remove the workspace root.

**Struct definition:**

```rust
pub struct RemoveDirectoryTool {
    root: WorkspaceRoot,
}

impl RemoveDirectoryTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "remove_directory";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative directory path to remove."
            },
            "recursive": {
                "type": "boolean",
                "default": false,
                "description": "Remove directory and all its contents. \
                                Requires confirmation."
            },
            "confirm": {
                "type": "boolean",
                "description": "Must be true when recursive is true."
            }
        },
        "required": ["path"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `true` when `recursive = true`, `false`
otherwise. Since the framework cannot inspect per-call input, return
`true` always and let the call body short-circuit gracefully for
non-recursive, non-dangerous invocations.

**Core implementation logic:**

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path      = require_str(Self::TOOL_NAME, &input, "path")?;
    let recursive = input.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);
    let confirm   = input.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);

    validate_path(Self::TOOL_NAME, path)?;

    // Guard: refuse to remove workspace root
    if path.trim_matches('/') == "." || path.is_empty() {
        return Ok(ToolResult::error(
            "remove_directory: cannot remove the workspace root.",
            &ctx.tool_use_id,
        ));
    }

    if recursive && !confirm {
        return Ok(ToolResult::error(
            "remove_directory: confirm must be true for recursive removal.",
            &ctx.tool_use_id,
        ));
    }

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.resolve(Self::TOOL_NAME, path)?;

    if !abs.is_dir() {
        return Ok(ToolResult::error(
            format!("'{path}' is not a directory."),
            &ctx.tool_use_id,
        ));
    }

    if recursive {
        tokio::fs::remove_dir_all(&abs).await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("remove_dir_all: {e}"),
            })?;
    } else {
        tokio::fs::remove_dir(&abs).await
            .map_err(|e| AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("remove_dir (not empty? use recursive: true): {e}"),
            })?;
    }

    Ok(ToolResult::ok(
        format!("Removed directory '{path}'"),
        &ctx.tool_use_id,
    ))
}
```

---

### 5.14 `AppendToFileTool` (`append_to_file`)

**File:** `crates/xaft-tools/src/fs/append_to_file.rs`

**Description (shown to LLM):**
> Append text to the end of a file. Creates the file if it does not
> exist. Useful for adding log entries, CSV rows, or trailing config
> blocks without reading and rewriting the whole file.

**Struct definition:**

```rust
pub struct AppendToFileTool {
    root: WorkspaceRoot,
}

impl AppendToFileTool {
    pub fn new(root: WorkspaceRoot) -> Self { Self { root } }
    const TOOL_NAME: &'static str = "append_to_file";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative path to append to \
                                (created if absent)."
            },
            "content": {
                "type": "string",
                "description": "Text to append. A trailing newline is \
                                recommended but not enforced."
            }
        },
        "required": ["path", "content"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false` (append is non-destructive).

**Core implementation logic:**

```rust
use tokio::io::AsyncWriteExt;
use tokio::fs::OpenOptions;

async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path    = require_str(Self::TOOL_NAME, &input, "path")?;
    let content = require_str(Self::TOOL_NAME, &input, "content")?;

    validate_path(Self::TOOL_NAME, path)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let abs = self.root.resolve(Self::TOOL_NAME, path)?;

    // Create parent directories if needed
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            AgtrsError::ToolCallFailed {
                tool_name: Self::TOOL_NAME.to_string(),
                reason: format!("create_dir_all: {e}"),
            }
        })?;
    }

    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&abs)
        .await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("open: {e}"),
        })?;

    file.write_all(content.as_bytes()).await
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("write: {e}"),
        })?;

    Ok(ToolResult::ok(
        format!("Appended {} bytes to '{path}'", content.len()),
        &ctx.tool_use_id,
    ))
}
```

---

### 5.15 `PatchFileTool` (`patch_file`)

**File:** `crates/xaft-tools/src/fs/patch_file.rs`

**Description (shown to LLM):**
> Apply a unified diff patch to a file. The patch must be in standard
> unified diff format (as produced by `diff_files` or `git diff`).
> Returns the number of hunks applied and lines changed. Refuses to
> apply a patch that would produce an empty file.

**Struct definition:**

```rust
pub struct PatchFileTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl PatchFileTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self { Self { workspace } }
    const TOOL_NAME: &'static str = "patch_file";
}
```

**Schema:**

```rust
fn schema(&self) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Workspace-relative path of the file to patch."
            },
            "patch": {
                "type": "string",
                "description": "Unified diff patch text (the output of \
                                diff_files, git diff, or similar tools)."
            }
        },
        "required": ["path", "patch"],
        "additionalProperties": false
    })
}
```

**`requires_confirmation`:** `false` (the `edit_file` precedent; the
workspace commit is atomic and the before/after are logged).

**Core implementation logic:**

The `similar` crate provides `TextDiff` for producing diffs but does
not ship a patch *applier*. Two strategies:

1. **Preferred:** Use the `diffy` crate (MIT, actively maintained,
   pure-Rust patch applier). Add `diffy = "0.4"` to `xaft-tools/Cargo.toml`.
2. **Fallback:** Implement a minimal hunk applier using `similar`'s
   change iterator in reverse.

Strategy 1 is recommended:

```rust
async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
    let path  = require_str(Self::TOOL_NAME, &input, "path")?;
    let patch = require_str(Self::TOOL_NAME, &input, "patch")?;

    validate_path(Self::TOOL_NAME, path)?;

    if ctx.cancel_token.is_cancelled() {
        return Err(AgtrsError::Cancelled { ... });
    }

    let original = self.workspace.read(path).await
        .map_err(|_| ToolResult::error(...))?;

    // `diffy::apply` applies a unified patch to the original text
    let patched = diffy::apply(&original, &diffy::Patch::from_str(patch)
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("patch parse error: {e}"),
        })?)
        .map_err(|e| AgtrsError::ToolCallFailed {
            tool_name: Self::TOOL_NAME.to_string(),
            reason: format!("patch apply error: {e}"),
        })?;

    // Safeguard: refuse to write an empty result
    if patched.trim().is_empty() && !original.trim().is_empty() {
        return Ok(ToolResult::error(
            "patch_file: patch would produce an empty file; aborting.",
            &ctx.tool_use_id,
        ));
    }

    // Count changed lines for the success message
    let diff = similar::TextDiff::from_lines(&original, &patched);
    let lines_changed: usize = diff.iter_all_changes()
        .filter(|c| c.tag() != similar::ChangeTag::Equal)
        .count();
    let hunks_applied = diff.unified_diff().iter_hunks().count();

    self.workspace.write(path, &patched).await
        .map_err(|e| AgtrsError::ToolCallFailed { ... })?;

    let result = serde_json::json!({
        "applied": true,
        "hunks_applied": hunks_applied,
        "lines_changed": lines_changed
    });
    Ok(ToolResult::ok(result.to_string(), &ctx.tool_use_id))
}
```

**Edge cases:**
- Patch context lines do not match original file (file was modified
  since the diff was generated) → `diffy::apply` returns an error;
  propagate as `ToolResult::error` with the hunk details.
- Patch targets a non-existent file → `workspace.read` returns error;
  propagate as soft error.
- Empty patch string → `ToolResult::error("patch is empty")`.
- Patch reduces file to entirely empty content → safeguard fires.

---

## 6. `mod.rs` Export Changes

```rust
// crates/xaft-tools/src/fs/mod.rs  (additions)
mod glob;
mod file_stat;
mod tree;
mod diff_files;
mod read_many;
mod search_files;
mod move_file;
mod copy_file;
mod delete_file;
mod create_directory;
mod remove_directory;
mod append_to_file;
mod patch_file;

pub use glob::GlobTool;
pub use file_stat::FileStatTool;
pub use tree::TreeTool;
pub use diff_files::DiffFilesTool;
pub use read_many::ReadManyTool;
pub use search_files::SearchFilesTool;
pub use move_file::MoveFileTool;
pub use copy_file::CopyFileTool;
pub use delete_file::DeleteFileTool;
pub use create_directory::CreateDirectoryTool;
pub use remove_directory::RemoveDirectoryTool;
pub use append_to_file::AppendToFileTool;
pub use patch_file::PatchFileTool;
```

---

## 7. Registry Integration

`crates/xaft-tools/src/registry.rs` — updated `build_reader` and
`build_coder` methods:

```rust
pub fn build_reader(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
    let store  = self.make_store();
    let wroot  = WorkspaceRoot(self.workspace_root.clone());
    let mut reg = ToolRegistry::new();

    // Existing
    reg.add(ListFilesTool::new(Arc::clone(&store)));
    reg.add(ReadFileTool::new(Arc::clone(&store)));
    reg.add(GrepTool::new(Arc::clone(&store)));

    // New read-only tools
    reg.add(GlobTool::new(wroot.clone()));
    reg.add(FileStatTool::new(wroot.clone()));
    reg.add(TreeTool::new(wroot.clone()));
    reg.add(DiffFilesTool::new(Arc::clone(&store)));
    reg.add(ReadManyTool::new(Arc::clone(&store)));
    reg.add(SearchFilesTool::new(Arc::clone(&store)));

    if self.include_git { /* ... unchanged ... */ }
    Ok(reg)
}

pub fn build_coder(self) -> Result<ToolRegistry, Box<dyn std::error::Error + Send + Sync>> {
    let store  = self.make_store();
    let wroot  = WorkspaceRoot(self.workspace_root.clone());
    let executor = self.make_executor();
    let mut reg = ToolRegistry::new();

    // Existing
    reg.add(ListFilesTool::new(Arc::clone(&store)));
    reg.add(ReadFileTool::new(Arc::clone(&store)));
    reg.add(GrepTool::new(Arc::clone(&store)));
    reg.add(WriteFileTool::new(Arc::clone(&store)));
    reg.add(EditFileTool::new(Arc::clone(&store)));

    // New read-only tools (same as reader)
    reg.add(GlobTool::new(wroot.clone()));
    reg.add(FileStatTool::new(wroot.clone()));
    reg.add(TreeTool::new(wroot.clone()));
    reg.add(DiffFilesTool::new(Arc::clone(&store)));
    reg.add(ReadManyTool::new(Arc::clone(&store)));
    reg.add(SearchFilesTool::new(Arc::clone(&store)));

    // New write tools (coder only)
    reg.add(MoveFileTool::new(wroot.clone(), Arc::clone(&store)));
    reg.add(CopyFileTool::new(wroot.clone()));
    reg.add(DeleteFileTool::new(wroot.clone()));
    reg.add(CreateDirectoryTool::new(wroot.clone()));
    reg.add(RemoveDirectoryTool::new(wroot.clone()));
    reg.add(AppendToFileTool::new(wroot.clone()));
    reg.add(PatchFileTool::new(Arc::clone(&store)));

    if self.include_shell { reg.add(BashExecTool::new(Arc::clone(&executor))); }
    if self.include_git { /* ... unchanged ... */ }
    Ok(reg)
}
```

---

## 8. Cargo.toml Changes

`crates/xaft-tools/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
glob  = "0.3"
diffy = "0.4"
# `similar` is pulled transitively via agtrs-workspace; declare directly:
similar = "2"
```

If `similar` is already a workspace-level dependency, use:
```toml
similar = { workspace = true }
```

---

## 9. Files Table

| File | Action | Notes |
|------|--------|-------|
| `crates/xaft-tools/Cargo.toml` | modify | Add `glob`, `diffy`, confirm `similar` |
| `crates/xaft-tools/src/fs/mod.rs` | modify | Add module declarations + re-exports |
| `crates/xaft-tools/src/fs/glob.rs` | create | `GlobTool` |
| `crates/xaft-tools/src/fs/file_stat.rs` | create | `FileStatTool` |
| `crates/xaft-tools/src/fs/tree.rs` | create | `TreeTool` |
| `crates/xaft-tools/src/fs/list_files.rs` | modify | Add 6 new optional params |
| `crates/xaft-tools/src/fs/diff_files.rs` | create | `DiffFilesTool` |
| `crates/xaft-tools/src/fs/read_many.rs` | create | `ReadManyTool` |
| `crates/xaft-tools/src/fs/search_files.rs` | create | `SearchFilesTool` |
| `crates/xaft-tools/src/fs/move_file.rs` | create | `MoveFileTool` |
| `crates/xaft-tools/src/fs/copy_file.rs` | create | `CopyFileTool` |
| `crates/xaft-tools/src/fs/delete_file.rs` | create | `DeleteFileTool` |
| `crates/xaft-tools/src/fs/create_directory.rs` | create | `CreateDirectoryTool` |
| `crates/xaft-tools/src/fs/remove_directory.rs` | create | `RemoveDirectoryTool` |
| `crates/xaft-tools/src/fs/append_to_file.rs` | create | `AppendToFileTool` |
| `crates/xaft-tools/src/fs/patch_file.rs` | create | `PatchFileTool` |
| `crates/xaft-tools/src/registry.rs` | modify | Wire new tools into reader/coder |
| `crates/xaft-config/src/types.rs` | modify | Add `FsToolConfig` struct |
| `crates/xaft-config/src/defaults.rs` | modify | `impl Default for FsToolConfig` |

---

## 10. Acceptance Criteria

The following criteria must all pass for this feature to be considered
complete:

1. **AC-01 Glob basic** — `GlobTool::call` with pattern `"src/**/*.rs"`
   returns only `.rs` files under `src/` and no files outside it.

2. **AC-02 Glob traversal rejection** — `GlobTool::call` with pattern
   `"../etc/**"` returns a `ToolResult::error` (or `AgtrsError`)
   without accessing any filesystem path outside the workspace root.

3. **AC-03 Glob empty match** — `GlobTool::call` with a pattern that
   matches nothing returns `ok("No files matching …")`, not an error.

4. **AC-04 FileStat exists** — `FileStatTool::call` on an existing file
   returns valid JSON with `exists: true`, correct `size_bytes`, and
   `is_file: true`.

5. **AC-05 FileStat missing** — `FileStatTool::call` on a non-existent
   path returns `ok` (not `error`) with `{"exists": false, "path": "..."}`.

6. **AC-06 Tree renders structure** — `TreeTool::call` on a directory
   with 3 levels of nesting renders ASCII tree lines containing `├──`
   or `└──` connectors at each depth.

7. **AC-07 Tree skips target** — A workspace with a `target/` directory
   at the root does not include `target/` or any of its contents in the
   tree output when `show_hidden` is false.

8. **AC-08 ReadMany batch** — `ReadManyTool::call` with 3 paths reads
   all three and returns a JSON array of length 3 with correct `content`
   values. Total round-trips to `WorkspaceStore::read` is exactly 3.

9. **AC-09 ReadMany over-limit** — Calling `ReadManyTool::call` with
   more paths than `max_files` returns a `ToolResult::error` explaining
   the limit.

10. **AC-10 DiffFiles identical** — `DiffFilesTool::call` when both
    files have identical content returns `ok("Files are identical.")`.

11. **AC-11 DiffFiles diff** — `DiffFilesTool::call` on two files that
    differ by one line returns a unified diff string containing `---`,
    `+++`, and `@@` markers.

12. **AC-12 SearchFiles substring** — `SearchFilesTool::call` with
    `name_pattern: "main"` returns all files whose filename contains
    `main` (case-insensitive by default).

13. **AC-13 MoveFile success** — `MoveFileTool::call` moves a file, the
    source no longer exists, and the destination contains the original
    content.

14. **AC-14 MoveFile overwrite guard** — `MoveFileTool::call` with
    `overwrite: false` when destination exists returns
    `ToolResult::error` without touching either file.

15. **AC-15 DeleteFile confirm gate** — `DeleteFileTool::call` with
    `confirm: false` returns `ToolResult::error` and leaves the file
    untouched.

16. **AC-16 DeleteFile removes** — `DeleteFileTool::call` with
    `confirm: true` removes the file and returns `{"deleted": true}`.

17. **AC-17 CreateDirectory idempotent** — Calling `CreateDirectoryTool`
    on a path that already exists does not error.

18. **AC-18 RemoveDirectory root guard** — `RemoveDirectoryTool::call`
    with `path: "."` returns `ToolResult::error("cannot remove
    workspace root")`.

19. **AC-19 AppendToFile creates** — `AppendToFileTool::call` on a
    non-existent path creates the file with the appended content.

20. **AC-20 AppendToFile appends** — Calling `AppendToFileTool` twice
    produces a file containing the concatenation of both payloads.

21. **AC-21 PatchFile applies** — `PatchFileTool::call` with a valid
    unified diff patch modifies the file correctly and returns
    `{"applied": true, "hunks_applied": N, "lines_changed": M}` with
    correct values.

22. **AC-22 PatchFile empty guard** — A patch that would reduce the
    file to empty content is rejected with `ToolResult::error`.

23. **AC-23 All tools cancel** — Every new tool checks
    `ctx.cancel_token.is_cancelled()` before performing any filesystem
    I/O and returns `AgtrsError::Cancelled` promptly.

24. **AC-24 Reader registry count** — `ToolRegistryBuilder::build_reader()`
    without git returns exactly 9 tools: the existing 3 plus 6 new
    read-only tools.

25. **AC-25 Coder registry count** — `ToolRegistryBuilder::build_coder()`
    without git and without shell returns exactly 16 tools: existing 5
    plus 6 read-only plus 7 write-only (excluding `list_files` extension
    which is not a new tool).

---

## 11. Test Stubs

All tests live in `#[cfg(test)] mod tests` within each tool source file.
Integration tests live in `crates/xaft-tools/tests/`.

```rust
// ── glob.rs ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn glob_finds_rs_files() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/sub")).unwrap();
    std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(tmp.path().join("src/sub/lib.rs"), "").unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();

    let tool = GlobTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(serde_json::json!({"pattern": "src/**/*.rs"}), &ctx)
        .await.unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("src/main.rs"));
    assert!(res.content.contains("src/sub/lib.rs"));
    assert!(!res.content.contains("Cargo.toml"));
}

#[tokio::test]
async fn glob_rejects_traversal() {
    let tmp  = TempDir::new().unwrap();
    let tool = GlobTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(serde_json::json!({"pattern": "../**/*.rs"}), &ctx).await;
    assert!(res.is_err() || res.unwrap().is_error);
}

#[tokio::test]
async fn glob_empty_match_returns_ok() {
    let tmp  = TempDir::new().unwrap();
    let tool = GlobTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t3");
    let res  = tool.call(serde_json::json!({"pattern": "**/*.ts"}), &ctx)
        .await.unwrap();
    assert!(!res.is_error);
    assert!(res.content.contains("No files"));
}

// ── file_stat.rs ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn file_stat_existing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("hello.txt");
    std::fs::write(&path, "world").unwrap();

    let tool = FileStatTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(serde_json::json!({"path": "hello.txt"}), &ctx)
        .await.unwrap();

    assert!(!res.is_error);
    let json: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    assert_eq!(json["exists"], true);
    assert_eq!(json["is_file"], true);
    assert_eq!(json["size_bytes"], 5);
}

#[tokio::test]
async fn file_stat_missing_path_returns_ok_not_error() {
    let tmp  = TempDir::new().unwrap();
    let tool = FileStatTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(serde_json::json!({"path": "nope.txt"}), &ctx)
        .await.unwrap();

    // Should be a successful ToolResult, not an error
    assert!(!res.is_error);
    let json: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    assert_eq!(json["exists"], false);
}

// ── tree.rs ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tree_renders_nested_dirs() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/utils")).unwrap();
    std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
    std::fs::write(tmp.path().join("src/utils/helpers.rs"), "").unwrap();

    let tool = TreeTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(serde_json::json!({}), &ctx).await.unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("src"));
    assert!(res.content.contains("main.rs"));
    assert!(res.content.contains("helpers.rs"));
    assert!(res.content.contains("├──") || res.content.contains("└──"));
}

#[tokio::test]
async fn tree_skips_target_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("target/debug")).unwrap();
    std::fs::write(tmp.path().join("target/debug/binary"), "").unwrap();
    std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();

    let tool = TreeTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(serde_json::json!({}), &ctx).await.unwrap();

    assert!(!res.is_error);
    assert!(!res.content.contains("target"));
    assert!(!res.content.contains("binary"));
}

// ── diff_files.rs ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn diff_files_identical_reports_same() {
    let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
        ("a.txt".into(), "hello\nworld\n".into()),
        ("b.txt".into(), "hello\nworld\n".into()),
    ])) as Arc<dyn WorkspaceStore>;

    let tool = DiffFilesTool::new(store);
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(serde_json::json!({"path_a": "a.txt", "path_b": "b.txt"}), &ctx)
        .await.unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("identical"));
}

#[tokio::test]
async fn diff_files_produces_unified_diff() {
    let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
        ("a.txt".into(), "line1\nline2\nline3\n".into()),
        ("b.txt".into(), "line1\nLINE2\nline3\n".into()),
    ])) as Arc<dyn WorkspaceStore>;

    let tool = DiffFilesTool::new(store);
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(
        serde_json::json!({"path_a": "a.txt", "path_b": "b.txt"}), &ctx,
    ).await.unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("@@"));
    assert!(res.content.contains("-line2"));
    assert!(res.content.contains("+LINE2"));
}

// ── read_many.rs ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_many_reads_all_paths() {
    let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
        ("a.rs".into(), "fn a() {}".into()),
        ("b.rs".into(), "fn b() {}".into()),
        ("c.rs".into(), "fn c() {}".into()),
    ])) as Arc<dyn WorkspaceStore>;

    let tool = ReadManyTool::new(store);
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(
        serde_json::json!({"paths": ["a.rs", "b.rs", "c.rs"]}), &ctx,
    ).await.unwrap();

    assert!(!res.is_error);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&res.content).unwrap();
    assert_eq!(arr.len(), 3);
    assert!(arr.iter().any(|v| v["path"] == "a.rs"));
}

#[tokio::test]
async fn read_many_over_limit_returns_error() {
    let store = Arc::new(InMemoryWorkspaceStore::new()) as Arc<dyn WorkspaceStore>;
    let tool  = ReadManyTool::with_limits(store, 3, 32768);
    let ctx   = ToolContext::new("t2");
    let paths: Vec<serde_json::Value> = (0..5).map(|i| format!("f{i}.rs").into()).collect();
    let res   = tool.call(serde_json::json!({"paths": paths}), &ctx)
        .await.unwrap();
    assert!(res.is_error);
    assert!(res.content.contains("at most 3"));
}

// ── delete_file.rs ────────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_file_requires_confirm_true() {
    let tmp  = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("del.txt"), "data").unwrap();
    let tool = DeleteFileTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(
        serde_json::json!({"path": "del.txt", "confirm": false}), &ctx,
    ).await.unwrap();
    assert!(res.is_error);
    assert!(tmp.path().join("del.txt").exists()); // file still there
}

#[tokio::test]
async fn delete_file_removes_with_confirm() {
    let tmp  = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("del.txt"), "data").unwrap();
    let tool = DeleteFileTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(
        serde_json::json!({"path": "del.txt", "confirm": true}), &ctx,
    ).await.unwrap();
    assert!(!res.is_error);
    assert!(!tmp.path().join("del.txt").exists());
}

// ── append_to_file.rs ─────────────────────────────────────────────────────────

#[tokio::test]
async fn append_creates_file_if_absent() {
    let tmp  = TempDir::new().unwrap();
    let tool = AppendToFileTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(
        serde_json::json!({"path": "new.txt", "content": "hello\n"}), &ctx,
    ).await.unwrap();
    assert!(!res.is_error);
    let content = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
    assert_eq!(content, "hello\n");
}

#[tokio::test]
async fn append_adds_to_existing_file() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("log.txt"), "line1\n").unwrap();
    let tool = AppendToFileTool::new(WorkspaceRoot(tmp.path().to_path_buf()));
    let ctx  = ToolContext::new("t2");
    tool.call(serde_json::json!({"path": "log.txt", "content": "line2\n"}), &ctx)
        .await.unwrap();
    let content = std::fs::read_to_string(tmp.path().join("log.txt")).unwrap();
    assert_eq!(content, "line1\nline2\n");
}

// ── patch_file.rs ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn patch_file_applies_valid_patch() {
    let original = "fn main() {\n    println!(\"old\");\n}\n";
    let patched  = "fn main() {\n    println!(\"new\");\n}\n";
    let patch_text = {
        let diff = similar::TextDiff::from_lines(original, patched);
        diff.unified_diff().header("main.rs", "main.rs").to_string()
    };

    let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
        ("main.rs".into(), original.into()),
    ])) as Arc<dyn WorkspaceStore>;

    let tool = PatchFileTool::new(Arc::clone(&store));
    let ctx  = ToolContext::new("t1");
    let res  = tool.call(
        serde_json::json!({"path": "main.rs", "patch": patch_text}), &ctx,
    ).await.unwrap();
    assert!(!res.is_error);
    let json: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    assert_eq!(json["applied"], true);
    assert!(json["lines_changed"].as_u64().unwrap() > 0);
    let new_content = store.read("main.rs").await.unwrap();
    assert!(new_content.contains("new"));
    assert!(!new_content.contains("old"));
}

#[tokio::test]
async fn patch_file_rejects_empty_result() {
    // A patch that removes all content should be rejected
    let original = "important content\n";
    // Manually craft a patch that deletes all lines
    let patch_text = "--- a\n+++ b\n@@ -1 +1,0 @@\n-important content\n";

    let store = Arc::new(InMemoryWorkspaceStore::with_files(vec![
        ("a.txt".into(), original.into()),
    ])) as Arc<dyn WorkspaceStore>;

    let tool = PatchFileTool::new(store);
    let ctx  = ToolContext::new("t2");
    let res  = tool.call(
        serde_json::json!({"path": "a.txt", "patch": patch_text}), &ctx,
    ).await.unwrap();
    assert!(res.is_error);
    assert!(res.content.contains("empty"));
}
```

---

## 12. Migration and Rollout

1. **Phase 1 — Read-only tools only** (`glob`, `file_stat`, `tree`,
   `diff_files`, `read_many`, `search_files`): merge to main behind
   `--feature expanded-fs-read` if desired; these have no destructive
   risk and can ship with minimal QA overhead.

2. **Phase 2 — Safe write tools** (`create_directory`,
   `append_to_file`, `patch_file`): these are non-destructive writes;
   merge after Phase 1 integration tests pass.

3. **Phase 3 — Destructive tools with confirmation gates**
   (`move_file`, `copy_file`, `delete_file`, `remove_directory`): merge
   after TUI approval flow is verified to fire correctly for
   `requires_confirmation = true` tools in headless mode.

4. **list_files extension**: merge with Phase 1; new parameters are
   purely additive.

5. **Config section** `[tool.fs]`: deploy with Phase 1.

No database migrations are required. No protocol changes are required
(tools are agent-level; the TUI and session layers are unaffected).

---

## 13. Open Questions

| # | Question | Resolution needed by |
|---|----------|---------------------|
| OQ-1 | Should `move_file` support directory moves (rename a whole subtree), or only files? | Phase 3 planning |
| OQ-2 | `similar` vs `diffy` for `patch_file` — is `diffy` acceptable as a new transitive dependency? | Before Phase 2 |
| OQ-3 | Should `read_many` respect `ReadBeforeEditHook`? | Before Phase 1 |
| OQ-4 | For non-Unix targets (Windows CI), should `file_stat` skip `permissions_octal`? | Phase 1 |
| OQ-5 | Is there a desire to surface `FsToolConfig` limits to agents via their system prompt? | System prompt sprint |
| OQ-6 | Should `patch_file` accept git-extended unified diff headers (old-mode, new-mode lines)? | Phase 2 |

---

## 14. Security Considerations

All new tools continue to enforce the xaft path traversal invariant:
`validate_path(tool, rel)` is called on every user-supplied path before
any filesystem call. The `WorkspaceRoot::resolve` method applies
`std::path::Component`-level traversal detection (matching the existing
`FsWorkspaceStore::resolve` implementation) in addition to the
`validate_path` string-level check, providing defence-in-depth.

Destructive tools (`delete_file`, `remove_directory`, `move_file`,
`copy_file`) all have either a `confirm: bool` in-schema safety gate or
`requires_confirmation() = true` or both. Neither gate can be bypassed
by the agent without explicit user input (the TUI approval prompt or the
`confirm: true` parameter in the tool call JSON).

`patch_file` does not execute any shell commands; it operates entirely
on in-memory strings and then calls `workspace.write`. There is no
injection surface.

`glob` patterns are evaluated by the `glob` crate's purely in-process
iterator; no subprocess is spawned.
