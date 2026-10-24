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

### Running the test suite: `run_tests`

`run_tests` runs the project's tests and returns structured results instead of
a wall of output: `passed`, `failed`, `skipped`, `errors`, `total`, and a
`failures` list with each failing test's name and message (plus the exit code,
duration, and the *tails* of stdout/stderr, where every runner prints its
summary). A count is `null` when the run never got far enough to report it —
a compile error is not "0 failed".

| Parameter | Default | Meaning |
|---|---|---|
| `framework` | `auto` | `cargo`, `pytest`, `unittest`, `npm`, `yarn`, `pnpm` or `go`; `auto` reads the project files (`Cargo.toml` → cargo, `go.mod` → go, Python markers → pytest, `package.json` → the package manager its lockfile names) |
| `path` | the runner's scope | a test file or directory; pytest/unittest use `tests/` when it exists. For cargo a package directory selects the package, `tests/foo.rs` selects `--test foo`, and a file under `src/` filters by its module path |
| `args` | — | extra runner arguments, appended after xaft's own |
| `timeout` | `120` | seconds; `0` means no tool deadline (still capped at 24 h) |
| `cwd` | workspace root | where to run, inside the workspace |

How each runner is read: cargo's libtest summaries (summed across test
binaries, `--no-fail-fast`), panic messages and rustc diagnostics; pytest's
`--json-report` file when the `pytest-json-report` plugin is installed, else
its summary line and short test summary (the run is retried without the flag
when the plugin is missing); `unittest`'s `Ran N tests` / `FAILED (…)`;
jest, vitest, mocha and `node --test` output for the npm family (run with
`CI=true`); and `go test -json`'s event stream. Like `run_command`, it needs
approval, runs through the shared execution policy with no shell, and refuses
`..` in `path` and `cwd`.

## Outlook mail and calendar

`list_emails`, `read_email`, `search_emails`, `list_folders`,
`calendar_events`, `send_email`, `reply_email`, `move_email` and
`create_event` work on a Microsoft 365 / Outlook.com mailbox through Microsoft
Graph, with agenthicc's names, parameters and result shapes. Export a Graph
access token with the `Mail.ReadWrite`, `Mail.Send` and `Calendars.ReadWrite`
scopes:

```bash
export MSGRAPH_TOKEN=eyJ0eXAi...
```

The token is read on every call, so exporting it later works without a
restart. The four tools that change something (`send_email`, `reply_email`,
`move_email`, `create_event`) ask for approval first; all nine are `network`
capability tools. A network failure, a timeout or an HTTP error comes back as
`{"ok": false, "error": "...", "recoverable": true}` instead of ending the
turn, so the agent can retry or move on. The read timeout is
`[tools] http_timeout_s` (default 30 s, `0` disables it); connecting is capped
at 10 s. `send_email` attaches workspace files (up to 3 MiB in total, Graph's
inline limit). agenthicc's Windows-only Outlook COM backend is not ported:
xaft uses Graph on every platform. Remove the tools with
`[tools] denied = ["*_email*", "list_folders", "*_event*"]` if you do not use
them.

## Self-introspection tools

`list_xaft_docs`, `read_xaft_doc`, `search_xaft_docs`, `inspect_xaft_source`
and `search_xaft_source` let the agent read *this* xaft — its `docs/` tree,
`llms.txt`, `llms-full.txt`, `README.md`, and every `.rs` file under `crates/`
and `src/` — instead of trusting what it remembers. They are agenthicc's
`*_agenthicc_*` tools with the same parameters, bounds (2000 lines per read,
200 matches per search, 120 000 bytes of source) and result shapes:

```text
list_xaft_docs(section="guides")
read_xaft_doc(path="guides/workflows.md", start_line=1, max_lines=400)
search_xaft_docs(query="browser_backend")
inspect_xaft_source(target="xaft_tools::registry:ToolRegistryBuilder::build_coder")
inspect_xaft_source(target="xaft_runtime::phased", include_source=false)
inspect_xaft_source(target="crates/xaft-tools/tests/tool_parity_reachability.rs:coder_registry")
search_xaft_source(query="fn build_coder", module="xaft_tools")
```

`inspect_xaft_source` takes a module path (`xaft_runtime::phased`), an item
(`module:Item`, `module:Type::method`, or the all-`::` form; `.` works too) or
a source file path, and returns the file, line range, signature, doc comment,
source and — for a module or a type — an outline of its items (a type's
fields or variants plus the methods of its `impl` blocks). Nothing is
compiled: a small Rust outliner reads the text.

The corpus is **compiled into the binary**: the root crate's `build.rs` packs
it into a per-file-DEFLATE archive that `main.rs` embeds, so the answers always
describe the code the binary was built from, and there is no filesystem path
to traverse. The cost is binary size — about 2.5 MiB for the ~9 MB of docs and
source (536 files) it packs today (`Corpus::archive_bytes`; the binary's test
suite fails if it passes 8 MiB). The tools are read-only, need no approval, and stay available in Plan
and Safe modes. A build without the embedded corpus (a library test, an
embedder) still has the tools; they say that no corpus is available.

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

To have the agent write one for you, run the `make_xaft_tool` workflow
(`/workflow make_xaft_tool`, or its agenthicc name `make_agenthicc_tool`): it
plans the tool, writes it under `.xaft/tools/_drafts/`, validates it and runs
it once with sample arguments (with your approval), then publishes it to
`.xaft/tools/`.

## Tool registry

Every tool is registered in `xaft-tools/src/registry.rs` with schema, risk
level, and permission metadata. `/permissions` in the TUI lists them; `/tools`
lists the session's tools, custom ones included.

## MCP tools

Every MCP server the session connects contributes its tools to every agent
turn and workflow phase as `mcp__<server>__<tool>`, with the `network`
capability (Plan never offers them; Safe asks before each call) — see
[MCP](mcp.md). `/tools` lists them with the source `mcp`.

## Subagents

Every agent turn can also call `spawn_subagents`, which runs typed workers
(explorer, planner, implementer, executor, tester, reviewer, documenter,
verifier, researcher) in parallel over the turn's own tools, and the
communication tools that go with it (`answer_subagent`,
`send_subagent_message`, `collect_subagent_results`; workers get
`ask_parent`, `send_parent_message`, `poll_agent_messages`,
`send_peer_message`, `ask_peer`, `answer_peer`) — see
[Subagents](subagents.md).

## Related

- [Security](security.md)
- [MCP](mcp.md)
- [Subagents](subagents.md)
- [Browser automation](browser.md)
- [Reference: kernel](../reference/kernel.md)
