# Repository State

Current xaft repository layout (as of this writing):

## Workspace crates

```text
crates/xaft-agent    Agent loop: streaming, signals, plan mode
crates/xaft-agents   Planner/Coder/QA/Fixer + registry + handoff
crates/xaft-cli      CLI entry, dispatch, config/session/run commands
crates/xaft-config   Typed config, TOML load/merge, validation, watcher
crates/xaft-memory   Recall/remember/summarize/forget tools
crates/xaft-runtime  Kernel: runtime, event loop, providers, orchestration
crates/xaft-session  Durable session + conversation persistence
crates/xaft-skills   Loadable agent knowledge files
crates/xaft-tools    fs/git/shell tools + dynamic factory + registry
crates/xaft-tui      Conversational streaming terminal renderer
```

## Docs layout

```text
docs/
├── index.md
├── guides/          # quickstart, architecture, configuration, modes, tui,
│                    # workflows, subagents, memory, mcp, security, testing,
│                    # tools, background-sessions, session-service, commands
├── reference/       # cli, kernel, storage, repository-state, code-plan
├── contributing.md
└── assets/          # logos + screenshots
```

## Tooling

- `justfile` — `just build`, `just test`, `just fmt`, `just lint`, …
- `mkdocs.yml` — docs site config (Material theme)
- `scripts/docs-site.cjs` — local static-site build + link checker
- `llms.txt` / `llms-full.txt` — LLM-facing doc indexes

## CI

`.github/workflows/` mirrors agenthicc: lint, tests, docs build + GitHub Pages
deploy, CodeQL, release, stale.

## Related

- [Architecture](../guides/architecture.md)
