# File Tools

The file tools are the most frequently invoked tools in any xaft workflow. They provide structured, validated access to the workspace filesystem — reading, writing, editing, listing, and searching files. Every file tool routes through `FsWorkspaceStore` and validates paths with `validate_path()`, creating a secure boundary between the agent and the host filesystem.

---

## Common Security Model

Before diving into individual tools, it is essential to understand the shared security infrastructure that all file tools enforce:

### `validate_path()`

Every file tool begins by calling `validate_path(requested_path, workspace_root)`. This function:

1. Joins the requested path to `workspace_root` to produce an absolute path.
2. Calls `std::fs::canonicalize()` (or its async equivalent) to resolve symlinks and `..` components.
3. Verifies that the canonicalized path starts with `workspace_root`.
4. Returns `Err(AgtrsError::PathTraversal)` if the path escapes the root.

This check prevents the most common filesystem escape vectors: `../../etc/passwd`, symlink chains pointing outside the workspace, and Unicode normalization tricks. It is applied uniformly across all file tools so that no tool can be used as a bypass.

### Cancellation Check

Every file tool checks `ctx.cancel_token.is_cancelled()` before performing I/O. If the token is cancelled, the tool returns `AgtrsError::Cancelled` immediately, without opening files or spawning search processes. This makes long-running operations like `GrepTool` over large codebases cooperatively cancellable.

---

## `ReadFileTool`

Reads the contents of a single file from the workspace, with optional line-range selection and line-number annotation.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Path to the file, relative to workspace root"
    },
    "start_line": {
      "type": "integer",
      "description": "1-based start line (inclusive). Defaults to 1."
    },
    "end_line": {
      "type": "integer",
      "description": "1-based end line (inclusive). Defaults to start_line + MAX_LINES_DEFAULT."
    },
    "with_line_numbers": {
      "type": "boolean",
      "description": "Prepend line numbers to each line. Defaults to true."
    }
  },
  "required": ["path"]
}
```

### Behavior

`ReadFileTool` opens the file, reads the requested line range, and returns the content as a single string. The default maximum is `MAX_LINES_DEFAULT = 500` lines, which balances usefulness (most source files fit within 500 lines) against prompt-length limits (sending thousands of lines to the LLM degrades response quality and increases latency).

When `with_line_numbers` is `true` (the default), each line is prefixed with its 1-based line number followed by a pipe separator: `  42 | fn main() {`. This format is critical for `EditFileTool`, which uses line numbers to locate edit targets, and for the LLM, which needs to reference specific lines in its reasoning.

If `start_line` and `end_line` are both omitted, the tool reads from line 1 up to `MAX_LINES_DEFAULT`. If the file is shorter than the requested range, it reads to EOF without error. If the file does not exist, the tool returns `ToolResult::error("file not found: <path>")` — a soft error that the LLM can recover from by adjusting the path.

### Example

```json
{
  "path": "src/main.rs",
  "start_line": 10,
  "end_line": 25,
  "with_line_numbers": true
}
```

Output:

```
  10 | use agtrs_runtime::tool::Tool;
  11 |
  12 | pub struct MyTool;
  13 |
  14 | #[async_trait]
  15 | impl Tool for MyTool {
  16 |     fn name(&self) -> &str { "my_tool" }
  17 |     fn description(&self) -> &str { "Does something useful" }
  18 |     fn schema(&self) -> serde_json::Value { json!({...}) }
  19 |     fn requires_confirmation(&self) -> bool { false }
  20 |     async fn call(&self, input: Value, ctx: ToolContext)
  21 |         -> Result<ToolResult, AgtrsError>
  22 |     {
  23 |         // implementation
  24 |     }
  25 | }
```

---

## `WriteFileTool`

Creates or overwrites a file in the workspace. This is the most destructive file operation and is always treated with caution.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Path to write, relative to workspace root"
    },
    "content": {
      "type": "string",
      "description": "Full file content to write"
    },
    "destructive": {
      "type": "boolean",
      "description": "Allow overwriting an existing file. Defaults to false."
    }
  },
  "required": ["path", "content"]
}
```

### Behavior

`WriteFileTool` performs a full-file write. It is the tool of choice for creating new files or rewriting a file entirely. The `destructive` flag controls whether an existing file can be overwritten:

- **`destructive: false`** (default): If the file already exists, the tool returns `ToolResult::error("file already exists: <path>, set destructive=true to overwrite")`. This prevents accidental data loss when the LLM generates a `write_file` call without realizing the file exists.

- **`destructive: true`**: The file is overwritten without confirmation. Because `WriteFileTool` always returns `requires_confirmation() -> true`, the agent loop must obtain approval before this operation is dispatched regardless of the `destructive` flag. The two-gate design (confirmation + destructive flag) ensures that overwrites are always intentional and authorized.

The tool creates parent directories automatically using `tokio::fs::create_dir_all()`. If the write fails due to permissions or disk space, it returns `ToolResult::error(...)` with the underlying I/O error message.

### When to Use vs. `EditFileTool`

Use `WriteFileTool` when you are creating a new file from scratch or rewriting a file entirely (e.g., generating a complete source file). Use `EditFileTool` when you are making targeted changes to an existing file, as it is less error-prone and preserves the untouched portions of the file.

---

## `EditFileTool`

Performs targeted, in-place edits on an existing file using fuzzy-matched block replacement with atomic commit semantics.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Path to the file to edit, relative to workspace root"
    },
    "old_content": {
      "type": "string",
      "description": "The exact text block to find and replace"
    },
    "new_content": {
      "type": "string",
      "description": "The replacement text"
    },
    "occurrence": {
      "type": "string",
      "enum": ["first", "all"],
      "description": "Replace the first match or all matches. Defaults to 'first'."
    }
  },
  "required": ["path", "old_content", "new_content"]
}
```

### Behavior

`EditFileTool` delegates to `FileEditor::replace_block()`, which implements a fuzzy-matching strategy:

1. **Exact match attempt**: The tool first tries to find `old_content` as a literal substring in the file. If found, it performs a direct replacement.

2. **Fuzzy match fallback**: If the exact match fails (often because the LLM's whitespace or indentation doesn't perfectly match the file), the tool normalizes whitespace in both the file content and `old_content`, performs a fuzzy match with a configurable similarity threshold, and replaces the best-matching region.

3. **Occurrence handling**: When `occurrence` is `"first"` (default), only the first match is replaced. When `"all"`, every occurrence of `old_content` is replaced with `new_content`. The `"all"` option is useful for batch renames (e.g., changing a variable name across a file) but should be used carefully.

4. **Atomic commit**: The edit is written to a temporary file first, then atomically renamed over the original. If the write fails mid-operation, the original file remains intact. This prevents file corruption from partial writes, which is especially important when the agent is editing files that are part of a build pipeline.

### Error Cases

| Condition | Result |
|-----------|--------|
| File does not exist | `ToolResult::error("file not found: <path>")` |
| `old_content` not found (even with fuzzy matching) | `ToolResult::error("could not find old_content in file")` |
| Multiple matches when `occurrence="first"` is ambiguous | `ToolResult::error("multiple matches found, please provide more context")` |
| Write fails (permissions, disk) | `ToolResult::error(...)` with I/O details |

### Example

```json
{
  "path": "src/lib.rs",
  "old_content": "fn greet(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}",
  "new_content": "fn greet(name: &str, greeting: &str) -> String {\n    format!(\"{}, {}!\", greeting, name)\n}",
  "occurrence": "first"
}
```

---

## `ListFilesTool`

Enumerates files in the workspace, optionally filtered by prefix and suffix patterns.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "prefix": {
      "type": "string",
      "description": "Only include files whose path starts with this prefix"
    },
    "suffix": {
      "type": "string",
      "description": "Only include files whose path ends with this suffix (e.g., '.rs')"
    },
    "max_results": {
      "type": "integer",
      "description": "Maximum number of results. Defaults to 50."
    }
  }
}
```

### Behavior

`ListFilesTool` performs a recursive walk of the workspace directory, collecting file paths that match the optional `prefix` and `suffix` filters. The walk skips common non-source directories (`.git`, `target`, `node_modules`) to keep results relevant and avoid overwhelming the LLM with noise.

The `max_results` parameter caps the output size. Without it, a large codebase could return thousands of file paths, consuming a disproportionate share of the conversation context window. The default of 50 is sufficient for most exploratory queries; if the agent needs an exhaustive listing, it can issue multiple requests with different prefixes.

Results are returned as a newline-separated list of paths relative to the workspace root, sorted lexicographically for deterministic ordering.

### Example

```json
{
  "prefix": "src/tools/",
  "suffix": ".rs",
  "max_results": 20
}
```

Output:

```
src/tools/bash_exec.rs
src/tools/edit_file.rs
src/tools/grep.rs
src/tools/list_files.rs
src/tools/mod.rs
src/tools/read_file.rs
src/tools/write_file.rs
```

---

## `GrepTool`

Searches file contents for a regex pattern across the workspace, returning matched lines with file paths and line numbers.

### Input Schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Regular expression pattern to search for"
    },
    "path_prefix": {
      "type": "string",
      "description": "Only search files whose path starts with this prefix"
    },
    "path_suffix": {
      "type": "string",
      "description": "Only search files whose path ends with this suffix"
    },
    "case_sensitive": {
      "type": "boolean",
      "description": "Whether the search is case-sensitive. Defaults to true."
    },
    "max_matches": {
      "type": "integer",
      "description": "Maximum number of matches to return. Defaults to 50."
    }
  },
  "required": ["pattern"]
}
```

### Behavior

`GrepTool` compiles the `pattern` as a regular expression and scans all matching files in the workspace. It supports the full Rust regex syntax, including character classes, look-ahead (via the `fancy-regex` crate), and Unicode properties.

The tool applies `path_prefix` and `path_suffix` filters before opening any file, which means it can efficiently narrow searches to a subdirectory or file type (e.g., `path_suffix: ".rs"` for Rust source files). This is significantly faster than searching everything and then filtering results.

Each match is returned in a `ripgrep`-style format:

```
src/tools/edit_file.rs:45:    let old = normalize(&input.old_content);
src/tools/edit_file.rs:47:    let replaced = fuzzy_replace(&content, &old, &new);
```

The `max_matches` limit prevents runaway searches in large codebases. When the limit is reached, the tool appends a truncation notice: `... (truncated, max_matches reached. Increase max_matches for more results.)`.

If no matches are found, the tool returns `ToolResult::error("no matches found for pattern: <pattern>")` — a soft error that signals the LLM to broaden its search or adjust the pattern.

### Performance Characteristics

`GrepTool` is the most I/O-intensive file tool because it must read every matching file's contents. In workspaces with thousands of files, a broad search (e.g., `pattern: "fn "` without path filters) can take several seconds. The cancellation token is checked between files, so a cancelled workflow will not wait for the search to complete.

For best performance, agents should always provide `path_prefix` and/or `path_suffix` when they know the approximate location or type of the target. This reduces the file set from the entire workspace to a focused subset.

### Example

```json
{
  "pattern": "impl Tool for",
  "path_suffix": ".rs",
  "case_sensitive": true,
  "max_matches": 10
}
```

Output:

```
src/tools/bash_exec.rs:22:impl Tool for BashExecTool {
src/tools/edit_file.rs:18:impl Tool for EditFileTool {
src/tools/grep.rs:14:impl Tool for GrepTool {
src/tools/list_files.rs:12:impl Tool for ListFilesTool {
src/tools/read_file.rs:16:impl Tool for ReadFileTool {
src/tools/write_file.rs:20:impl Tool for WriteFileTool {
```

---

## Tool Selection Guide

When building agent prompts, it is important to guide the LLM toward the most appropriate file tool for each task:

| Goal | Tool | Why |
|------|------|-----|
| Understand existing code | `read_file` | Precise, line-ranged, with line numbers |
| Find where something is defined | `grep` | Regex across the whole workspace |
| See what files exist | `list_files` | Structured enumeration with filters |
| Create a new file | `write_file` | Full content, auto-creates directories |
| Rewrite a file entirely | `write_file` (destructive) | Safer than editing when the entire content changes |
| Make a targeted change | `edit_file` | Fuzzy-matched block replacement, preserves context |
| Rename across a file | `edit_file` (occurrence: all) | Batch replacement within a single file |
