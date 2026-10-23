# CLI Reference

The `xaft` binary is the entry point (see `crates/xaft-cli`). The exact set of
command paths the parser accepts is snapshotted in
[`docs/cli-command-tree.txt`](../cli-command-tree.txt), regenerated from clap
itself.

## Commands

| Command | Purpose |
|---|---|
| `xaft run "<task>"` | Run a task through the plan→code→verify→commit pipeline |
| `xaft --resume <id>` | Resume a session (with transcript replay) |
| `xaft init` | Scaffold the current directory: empty `AGENTS.md` + an all-comment `.xaft/xaft.toml` |
| `xaft config ...` | Show, initialise and validate configuration; list provider profiles |
| `xaft session ...` | List/inspect/export/resume sessions; the client-neutral session service |
| `xaft jobs ...` | Manage durable background sessions |
| `xaft agents` | Open the background-sessions manager (`--json` to list, `--run RUN` for one goal run) |
| `xaft attach <id>` | Attach the TUI to one exact background session (`--run RUN` for a goal run) |
| `xaft runs ...` | Inspect and control durable goal runs |
| `xaft worktrees ...` | Inspect, integrate and recover parallel worker orchestrations |
| `xaft workflows ...` | List and run workflows |
| `xaft mcp ...` | Configure MCP servers; `connect`, `refresh` and `doctor` talk to them |
| `xaft skills add <source>` | Install a skill from a path, an HTTPS URL or a GitHub repository |
| `xaft trust ...` | Review and trust this repository's extensions (`.xaft/cli.toml`) |
| `xaft version` | Print version and build info |
| `xaft completions <shell>` | Generate shell completions |

## Project scaffold — `xaft init`

```
xaft init [--force] [--write]
```

Creates `AGENTS.md` (empty) and `.xaft/xaft.toml`, printing one line per path:

```
Initialized /home/me/project
Created .xaft
Created AGENTS.md
Created .xaft/xaft.toml
```

Every line of the generated `xaft.toml` is a comment — it lists every setting
with its default, derived from the typed configuration, and changes nothing
until you uncomment a line. Existing files are kept (`Preserved …`) unless
`--force` replaces them (`Overwrote …`). `--write` is accepted for
compatibility. A refused scaffold (for example a symlinked target) prints
`error: …` and exits 1.

## Skills — `xaft skills add`

```
xaft skills add <SOURCE> [--global | --project] [--name NAME] [--skill a,b] [--all]
```

`SOURCE` is one of:

- a local skill directory (installed whole) or a single Markdown file;
- an HTTPS URL to a `SKILL.md` — GitHub `…/blob/<rev>/…/SKILL.md` and
  `…/tree/<rev>/<dir>` links are rewritten to raw downloads;
- a GitHub repository (`owner/repo`, `https://github.com/owner/repo`, or a
  `…/tree/<rev>/<dir>` link) or a local repository directory. The repository
  is scanned for every directory holding a `SKILL.md` (root first, then
  `skills/`, then conventional containers such as `.claude/skills`); all are
  installed unless `--skill a,b` names some.

Skills are installed as `.xaft/skills/<name>/` (or `~/.config/xaft/skills/<name>/`
with `--global`) — the directories every session loads skills from. An
existing skill is never overwritten; remote sources must use HTTPS; a skill
containing a symlink is refused. Each install prints
`Installed skill <name> (<scope>) at <path>`; a failure prints `error: …` and
exits 1.

## Trust and project CLI plugins — `xaft trust`

A repository may ship `.xaft/cli.toml`, which adds commands to `xaft`:

```toml
[[command]]
path = ["deploy", "staging"]
help = "Deploy the current branch to staging"
run = "./scripts/deploy.sh {region} --dry-run={dry_run}"
args = [
  { name = "region", default = "eu-west-1" },  # --region VALUE
  { name = "dry_run", type = "bool" },         # --dry-run
  { name = "ticket" },                         # positional TICKET
]
```

Those commands run programs with your permissions, so xaft loads a project's
`cli.toml` only after you trust its exact content:

```
xaft trust list            # each project extension and whether it is trusted
xaft trust add [PATH]      # trust the current content (default: every project extension)
xaft trust revoke PATH     # forget a trust record
```

Trust records live in `$XAFT_HOME/trust.json` (default `~/.xaft/trust.json`),
never in the repository, and bind the file's path to its SHA-256 — editing the
file revokes the trust. Your own `~/.config/xaft/cli.toml` is trusted by
location. An untrusted table is reported on stderr and its commands do not
exist. A plugin cannot redefine a built-in command, a `{name}` value is always
exactly one argument (never re-split by a shell), and the program's exit
status is the command's.

## MCP servers — `xaft mcp`

`list`, `add`, `get`, `remove`, `auth`, `disconnect` and `logout` edit or read
the `[[mcp.client]]` stanzas without connecting. Three commands connect:

```
xaft mcp connect <NAME> [-c PATH]           # status record as JSON
xaft mcp refresh <NAME> [-c PATH]           # catalog snapshot as JSON
xaft mcp doctor [NAME] [--json] [-c PATH]   # per-server diagnosis
```

See [Connecting MCP servers](../guides/mcp.md).

## Parallel orchestrations — `xaft worktrees`

An orchestration splits work into tasks, each done on its own branch in its own
Git worktree, then merged back into the parent branch. Manifests live in
`$XAFT_HOME/orchestrations/`, worktrees in `$XAFT_HOME/worktrees/<repo>/`.

```
xaft worktrees list [--json]                              # <id16>  <status>  <n> task(s)  parent=<session>
xaft worktrees show <ID> [--json]                         # the manifest as JSON
xaft worktrees integrate <ID> <TASK> [--repository DIR]   # merge a completed task
xaft worktrees recover <ID> [--repository DIR]            # reconcile with `git worktree list`
```

`integrate` refuses a task without completion evidence or a coordinator that
left the recorded parent branch; a conflicting merge is aborted (the worker
branch is preserved) and reported as `"ok": false` with the conflicting paths.

## Background sessions — `xaft agents` and `xaft attach`

```
xaft agents                       # interactive manager: ↑/↓, enter attach, c cancel, r resume, a archive, d delete, q quit
xaft agents --json                # current background sessions
xaft agents --run RUN [--json]    # the agents linked to one goal run
xaft attach <SESSION_ID> [--json] # hand one background session to the TUI
xaft attach --run RUN [--json]    # hand a goal run's main session to the TUI
```

Attaching an active session first moves it to `cancelled` with the reason
`foreground handoff requested`, so the background record and the TUI never both
own it. `--json` stops there and prints
`{"background_status": …, "cwd": …, "session_id": …, "status": "handoff_ready"}`.

## Key flags

| Flag | Purpose |
|---|---|
| `--mode <name>` | Start in a specific mode (safe/plan/auto/…; aliases accepted) |
| `--workflow <name>` | Select a workflow |
| `--headless` | Stdin JSON-lines interface for pipelines/CI |
| `--dry-run` | Plan only, no mutations |
| `-y`, `--auto-approve` | Approve every approval request of a headless run (denied otherwise) |
| `--dangerously-skip-permissions` | Skip permission gates (requires terminal confirmation in the TUI; approves every request headless) |
| `--mode MODE` | Start in `safe`, `plan` or `auto` (TUI and headless) |
| `--config <path>` | Use an explicit config file |

## Exit codes

- `0` — success
- `1` — task failed or cancelled
- `2` — usage/argument error

## Related

- [Quickstart](../guides/quickstart.md)
- [Configuration](../guides/configuration.md)
- [Connecting MCP servers](../guides/mcp.md)
- [Background sessions](../guides/background-sessions.md)
- [Security](../guides/security.md)
