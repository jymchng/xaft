# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- TUI feature parity with agenthicc:
  - `$` skill-only trigger picker (`SkillTriggerHandler`), backed by
    `xaft-skills`, registered alongside `/`, `@`, and `#`.
  - Safe → Plan → Yolo interactive mode cycle with agenthicc-compatible
    aliases (`yolo`→`auto`, `ask`/`guard`→`safe`, `review`→`plan`);
    `debug` rejected via `/mode`.
  - `...and N more tool calls` collapsed tool-group summary flushed to the
    scroll buffer at conversation boundaries and on interrupt.
  - Bounded resumed-transcript tail replay (default 20 turns) with a
    `Loading transcript…` label and chunked appending.
  - Bracketed-paste placeholder projection (`[Pasted text #N …]`) with
    Home/End, Backspace-after-`]` whole-delete, `Ctrl+V` reveal, and
    Esc-after-`]` whole-delete.
  - `✾ Total wall clock time since last IDLE` telemetry line after
    `✻ Worked for` in the exit summary.
  - `/usage` alias for the `/cost` token/cost table.
  - 6-row diff truncation (with `…` omission row) for `edit_file` results.
- Docs site mirror of agenthicc:
  - `mkdocs.yml` (Material theme), `docs/index.md`, `docs/guides/*`
    (15 pages), `docs/reference/*` (5 pages), `docs/contributing.md`.
  - `scripts/docs-site.cjs` — zero-dependency static-site builder + internal
    link checker (`--check` for CI).
  - `llms.txt` and `llms-full.txt` generated at the repo root.
- Repo hygiene:
  - `README.md` restyled to agenthicc's format (badges, capabilities table,
    quick start, screenshots, docs links).
  - `CHANGELOG.md` (Keep a Changelog), `CONTRIBUTING.md`, `LICENSE` (MIT),
    `LICENSE-APACHE` (Apache-2.0).

## [0.1.0] - 2026-05-24

### Added

- Initial workspace: `xaft-tui`, `xaft-runtime`, `xaft-cli`, `xaft-tools`,
  `xaft-agent`, `xaft-agents`, `xaft-config`, `xaft-session`, `xaft-memory`,
  `xaft-skills` (edition 2024, rust-version 1.86).
- Conversational streaming TUI (append-only transcript in the primary
  terminal buffer, no alternate screen).
- Event-driven runtime: three concurrent tasks (runtime loop, terminal event
  reader, tick spawner) feeding a single `mpsc` channel; lock-free single
  render loop.
- `EventBridge` subscribing to the runtime `SignalBus` and forwarding
  `TuiEvent`s.
- Multi-agent handoff orchestration: Planner → Coder → QA → Fixer with
  cycle detection and up to 14 handoffs.
- Git worktree isolation per session, fuzzy-anchor file edits,
  path-traversal protection, three-tier approval system.
- Provider abstraction (Anthropic, OpenAI, Ollama, LiteLLM) with streaming,
  thinking blocks, token accounting, and retry.
- Filesystem, git, and shell tool crates with a dynamic tool factory and
  registry.
- Typed configuration (TOML) with env overrides and hot-reload watcher.
- Durable session + conversation persistence (SQLite behind the `session`
  feature) with per-agent conversation keys and `--resume`.
- Memory tools (recall/remember/summarize/forget) with session/project/
  global tiers.
- Skill loading (`xaft-skills`) from `.xaft/skills/` and
  `~/.config/xaft/skills/` with YAML frontmatter.
- Slash-command system (`/help`, `/clear`, `/compact`, `/cost`, `/config`,
  `/mode`, `/model`, `/theme`, `/permissions`, `/resume`, `/rewind`,
  `/commit`, `/diff`, `/pr`, `/init`, `/doctor`, `/memory`, `/mcp`, `/bg`,
  `/vim`, `/emacs`, `/quit`).
- Trigger system: `/` command picker, `@` file mention picker, `#` history
  recall.
- Mode system: `auto`, `plan`, `ask`, `review`, `safe`, `debug` built-in
  modes with Shift+Tab cycling and `/mode`.
- Background pipelines with bounded mutation buffers and `/bg` re-attach.
- Cost/token tracking per agent (`/cost`), ephemeral status bar, exit
  summary with `✻ Worked for`.
- MCP server configuration and tool bridging.

[Unreleased]: https://github.com/jymchng/xaft/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jymchng/xaft/releases/tag/v0.1.0
