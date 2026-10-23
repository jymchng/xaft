# Tools

xaft exposes a trait-based tool system covering filesystem, git, shell and
browser operations, plus dynamic tool creation.

## Filesystem tools

Read, write, edit, move, copy, delete, list, search, glob, tree, diff, and
file metadata tools — all with path-traversal protection and bounded output
(see [Security](security.md)).

## Git tools

Status, diff, log, show, blame, branch, add, commit, push, stash, merge, tag,
remote, grep — integrated with the per-session worktree isolation.

## Shell tools

`bash_exec` runs commands under a configurable execution policy; output is
bounded and stderr is preserved.

## Browser tools

`playwright_*` and `cloakbrowser_*` — open, snapshot, click, fill, press,
wait_for, screenshot, close, status — drive a real Chromium over the DevTools
protocol. One family per session, chosen by `tools.browser_backend`; see
[Browser automation](browser.md).

## Dynamic tools

`xaft-tools/src/dynamic/` provides a factory for creating tools at runtime
(scripted tools) and the registry that routes tool names to implementations.

## Persistent custom tools

You can give every session your own tools — the xaft counterpart of
agenthicc's `.agenthicc/tools/*.py` plugins. A custom tool is one TOML file
that names a program to run:

```toml
# .xaft/tools/recent_commits.toml   (or ~/.config/xaft/tools/recent_commits.toml)
name = "recent_commits"          # optional; must equal the file stem
description = "List the most recent commit subjects touching a path."
capability = "git_read"          # read | search | write | execute | git_read | git_write | network
command = "git log -n {count} --format=%s -- {path}"
timeout_s = 30                   # 1..3600, default 60

[[params]]
name = "path"
description = "File or directory to inspect."

[[params]]
name = "count"
description = "How many commits to list."
default = "10"                   # a param with a default is optional
```

How a call runs:

* `command` is split into arguments **before** the values are substituted
  (POSIX-shell quoting, `'...'` and `"..."`), then each `{param}` is replaced
  inside its word. A value is therefore always exactly one argument — it can
  never add a flag, a second command or a redirection. There is no shell: no
  pipes, globs, `$VARS` or `&&` (put such logic in a script and call the
  script). The program word itself may not contain a placeholder, so a call
  can choose arguments but never *which* program runs. `{{` and `}}` are
  literal braces. This is the same rule as `.xaft/cli.toml` plugins.
* The program runs in the session's working directory with stdin closed and
  the declared timeout. Its stdout (the first 64 KiB) is the result; a
  non-zero exit is reported as an error with stderr.
* Every call asks for your approval first (`requires_confirmation`).
* The declared `capability` classifies the tool, so workflow phases and modes
  that restrict capabilities treat it like a built-in of that class (a
  `read` tool is available in plan mode, an `execute` one is not).

Where tools come from:

| Directory | Trust |
|---|---|
| `~/.config/xaft/tools/<name>.toml` (user) | trusted by location |
| `.xaft/tools/<name>.toml` (project) | loaded **only once trusted**: a repository can ship these and they run programs with your permissions. Review the file, then `xaft trust add .xaft/tools/<name>.toml` (or `xaft trust add` for every project extension). Editing the file revokes the trust. |

A project tool shadows a user tool of the same name; a tool that would shadow
a built-in tool is refused; files starting with `_` or `.` and anything in a
subdirectory (`_drafts/`, `_backups/`) are never loaded. Tools are re-read on
every run, so a new or newly trusted tool is available on your next message.
`/tools` lists them with their source (`project`, `user`) and reports every
file that is not loaded and why.

## Tool registry

Every tool is registered in `xaft-tools/src/registry.rs` with schema, risk
level, and permission metadata. `/permissions` in the TUI lists them; `/tools`
lists the session's tools, custom ones included.

## MCP tools

External MCP servers contribute tools through the same registry — see
[MCP](mcp.md).

## Related

- [Security](security.md)
- [Browser automation](browser.md)
- [Reference: kernel](../reference/kernel.md)
