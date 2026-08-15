# Tools

xaft exposes a trait-based tool system covering filesystem, git, and shell
operations, plus dynamic (scripted) tools and MCP integration. This guide
documents the tool surface verified against `crates/xaft-tools/src`.

## Filesystem tools

| Tool | Purpose |
|---|---|
| `read_file` | Read a file (with bounded output) |
| `read_many` | Read several files in one call |
| `read_before_edit` | Read the exact region before an edit |
| `write_file` | Write a new file |
| `edit_file` | In-place edit with fuzzy anchor matching |
| `patch_file` | Apply a patch |
| `append_to_file` | Append content to a file |
| `copy_file` | Copy a file |
| `move_file` | Move/rename a file |
| `delete_file` | Delete a file |
| `create_directory` | Create a directory |
| `remove_directory` | Remove a directory |
| `list_files` | List directory entries |
| `search_files` | Search for files by pattern |
| `glob` | Glob expansion |
| `tree` | Recursive directory tree |
| `grep` | Grep file contents |
| `file_stat` | File metadata |
| `diff_files` | Diff two files |

All fs tools enforce **path-traversal protection** and bounded output (see
[Security](10-security.md)).

## Git tools

| Tool | Purpose |
|---|---|
| `git_status` | Working tree status |
| `git_diff` | Diff (working tree / staged / refs) |
| `git_log` | Commit history |
| `git_show` | Show a commit |
| `git_blame` | Line-by-line authorship |
| `git_branch` / `git_create_branch` | List / create branches |
| `git_checkout_files` | Checkout files |
| `git_add` / `git_unstage` | Stage / unstage |
| `git_commit` | Commit |
| `git_push` | Push |
| `git_stash` / `git_stash_list` / `git_stash_pop` | Stash management |
| `git_merge` | Merge |
| `git_tag` | Tags |
| `git_remote` | Remotes |
| `git_grep` | Grep in git history |

Git tools integrate with the per-session **worktree isolation** — the agent
never mutates your main working tree directly.

## Shell tool

| Tool | Purpose |
|---|---|
| `bash_exec` | Run a shell command |

`bash_exec` runs under a configurable execution policy (`blocked_commands`,
`timeout_secs`, `max_output_bytes` — see [Configuration](02-configuration.md)).
By default, shell commands require **approval** (`guardrail.command_approval =
true`).

## Dynamic / scripted tools

`crates/xaft-tools/src/dynamic/` provides a factory for creating tools at
runtime:

- `scripted` — define a tool from a script (language + command + schema)
- `factory` — register dynamic tools into the registry

Dynamic tools inherit the same permission/approval system. Guard with
`[agent.<name>] allow_dynamic_tools / max_dynamic_tools /
dynamic_tool_approval` as needed.

## MCP tools

External MCP servers contribute tools through the same registry. Configure
servers under `[mcp]` in `xaft.toml` (see [MCP guide](../guides/mcp.md)) and
manage them in the TUI with `/mcp`. MCP tools are capability-gated like
built-ins.

## Tool registry

Every tool is registered in `crates/xaft-tools/src/registry.rs` with schema,
risk level, and permission metadata. `/permissions` in the TUI lists them;
the active mode's tool filter gates which are callable (see
[Modes](05-modes.md)).

## Related

- [Security →](10-security.md)
- [Modes →](05-modes.md)
- [MCP guide](../guides/mcp.md)
