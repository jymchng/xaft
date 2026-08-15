# Using xaft — the complete user manual

This manual is written for the **end user** of xaft: someone who wants to
install it, point it at an LLM provider, and use it to get code written —
safely and predictably. It is practical: every command, flag, and config key
has been verified against the current source (`crates/xaft-cli`,
`crates/xaft-config`, `crates/xaft-tui`).

## What xaft is

xaft is a **Rust-native runtime for autonomous coding agents**. You give it a
task in plain English; it reads your codebase, plans, edits files, runs
verification, and commits the result — all inside an isolated git worktree
with approval gates and full observability.

```bash
xaft run "Add error handling to all public functions in src/api/"
```

## Contents

| Guide | What it covers |
|---|---|
| [Installation](01-installation.md) | Requirements, building from source, completions |
| [Configuration](02-configuration.md) | `xaft.toml`, env vars, providers, agents, guardrails |
| [Your first task](03-first-task.md) | Running `xaft run`, planning, approvals, dry-run |
| [The TUI](04-tui.md) | Interactive workspace, triggers, paste, modes, telemetry |
| [Modes](05-modes.md) | Safe → Plan → Yolo cycle, aliases, `/mode` |
| [Slash commands](06-commands.md) | Every `/command` in the TUI |
| [Sessions](07-sessions.md) | Persistence, `--resume`, `xaft session` |
| [Memory](08-memory.md) | `remember` / `recall` / `summarize` / `forget` |
| [Tools](09-tools.md) | Filesystem, git, shell, MCP tools |
| [Security](10-security.md) | Approval gates, guardrails, secrets |
| [Troubleshooting](11-troubleshooting.md) | Common issues and fixes |
| [FAQ](12-faq.md) | Frequently asked questions |

## Quick orientation

- Bare `xaft` opens the interactive TUI.
- `xaft run "<task>"` runs a task and exits (or stays in the TUI).
- `xaft config` manages configuration; `xaft session` manages sessions.
- `xaft completions <shell>` generates shell completions.
- `xaft version` shows the version.

All commands and flags in this manual were verified against
`crates/xaft-cli/src/args.rs` and `crates/xaft-config/src/types.rs`.
