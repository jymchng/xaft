# FAQ

## What is xaft?

xaft is a **Rust-native runtime for autonomous coding agents**. You describe a
coding task in plain English; it reads your codebase, plans, edits files, runs
verification, and commits the result — inside an isolated git worktree with
approval gates and full observability.

## How is xaft different from other coding agents?

xaft is a runtime, not a thin wrapper around an LLM API. It provides
transactional safety (git worktree isolation, fuzzy-anchor edits,
path-traversal protection), real-time observability (SignalBus, TUI, tracing,
cost tracking), and multi-agent orchestration (Planner → Coder → QA → Fixer
with handoff), all as a Rust-native binary.

## Is my working tree safe?

Yes. xaft runs each session in an **isolated git worktree** — your main
working tree is never modified directly. File edits go through a
transactional workspace; on success changes are committed in the session
worktree, and on failure it is rolled back.

## How do approvals work?

Every tool carries a risk level: LOW auto-approves, MEDIUM auto-approves
unless strict mode is on, HIGH/CRITICAL always asks you. Shell commands,
deletions, and network access are gated by default. See
[Security](10-security.md).

## Can xaft run without a TUI?

Yes. `xaft run "task" --headless` disables the TUI; `--json` emits
newline-delimited JSON events for pipelines and CI.

## Does xaft cost money?

xaft itself is open source. You pay only your LLM provider's usage. Use
`/cost` (or `xaft run ... --json` and inspect the cost field) to track spend,
and set `[guardrail.cost_limit_config] max_spend` to cap it.

## Which LLM providers are supported?

Anthropic (default), OpenAI, Ollama, and LiteLLM. Configure them under
`[provider.<name>]` in `xaft.toml`. See [Configuration](02-configuration.md).

## What are modes and why does Safe matter?

Modes gate what the agent may do. **Safe** is a read-only sandbox, **Plan**
produces a plan without executing, **Yolo/Auto** has full capabilities.
Shift+Tab cycles them. See [Modes](05-modes.md).

## Can I add my own tools?

Yes. xaft has a trait-based tool system; you can add tools to the registry in
`crates/xaft-tools`, or define **scripted dynamic tools** at runtime. MCP
servers also contribute tools. See [Tools](09-tools.md).

## How do sessions and resume work?

Every run creates a session. `xaft session list` shows them; `--resume <id>`
(or `--continue` for the most recent) reloads prior context and replays the
newest 20 turns. See [Sessions](07-sessions.md).

## Does xaft remember things between sessions?

Yes, via the memory system: `remember`/`recall`/`summarize`/`forget`, stored
in a project-scoped SQLite backend by default. See [Memory](08-memory.md).

## Can xaft work on my existing git repo without a separate checkout?

Yes — it uses a git worktree so your checkout stays clean; the agent's edits
land in the session worktree and are committed there.

## What if a run goes wrong?

- Use `--dry-run` first to review the plan without changes.
- Roll back: the session worktree is discarded on failure.
- Cancel a stuck session with `xaft session cancel <id> -f`.

## How do I contribute?

Read [CONTRIBUTING.md](../contributing.md) and the
[contributing guide](../contributing.md). Write a PRD under `prds/`, add
tests, run `./scripts/ci.sh`, and update `CHANGELOG.md`.

## Related

- [Troubleshooting →](11-troubleshooting.md)
- [Quickstart](../guides/quickstart.md)
