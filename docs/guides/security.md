# Security

xaft treats every mutation as reversible and every action as observable.

## Approval gates

- **Auto-approve policy**: Low-risk read-only tools auto-approve; medium-risk
  (edits, builds) auto-approve unless strict mode; high-risk (writes, network)
  require explicit approval.
- **Three-tier system**: per-tool confirmation, TUI approval dialogs, and
  auto-approve gates.
- `/permissions` lists the current tool permissions.

## Sandboxing

- Git worktree isolation per session — your working tree is never modified
  directly.
- Path-traversal protection on all file operations.
- Shell command sandboxing with configurable execution policy.
- `--dangerously-skip-permissions` requires a terminal confirmation dialog
  before the run starts.

## Modes

Mode selection (Safe/Plan/Yolo) is a first-line security control: Safe mode
hard-blocks writes, commands, and network; Plan mode is read-only. See
[Modes](modes.md).

## Secrets

API keys are read from environment variables; provider credentials never
appear in the transcript. Use `/config` to inspect the resolved config
(secrets are redacted).

## Trusting project extensions

A cloned repository can ship `.xaft/cli.toml`, whose `[[command]]` tables add
`xaft` subcommands that run programs with your permissions. xaft never loads a
project's table until you have reviewed and trusted its exact content:

```
xaft trust list          # every project extension and whether it is trusted
xaft trust add           # trust the current content of every project extension
xaft trust revoke .xaft/cli.toml
```

- Trust records live in `$XAFT_HOME/trust.json` (default `~/.xaft/trust.json`,
  mode `0600`) — never inside the repository, which could otherwise forge its
  own trust. A project's `[plugins] trust_file` is deliberately not honoured
  for this.
- A record binds the file's resolved path to its SHA-256: editing the file
  revokes its trust until you run `xaft trust add` again.
- Your own `~/.config/xaft/cli.toml` is trusted by location.
- An untrusted or edited table is reported on stderr on every run, and its
  commands do not exist. No plugin may redefine a built-in command.

## Installing skills

`xaft skills add` writes into the directories your sessions load skills from,
so it is an explicit trust decision: a skill's text reaches the system prompt.
Remote sources must use HTTPS, repository clones run with prompts disabled and
only the `https` protocol allowed, skills are capped at 1 MiB and must be
UTF-8, a skill containing a symlink is refused, and an existing skill is never
overwritten.

## Related

- [MCP](mcp.md) — external tools inherit the same gates
- [Reference: repository state](../reference/repository-state.md)
