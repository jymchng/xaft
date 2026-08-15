# Quickstart

Install xaft, point it at an LLM provider, and run your first task.

## Requirements

- Rust 1.86 or newer (edition 2024 workspace)
- An LLM provider API key: Anthropic (default), OpenAI, Ollama, or LiteLLM

## Build

```bash
git clone https://github.com/jymchng/xaft
cd xaft
cargo build --workspace
```

The `xaft` binary lands in `target/debug/xaft` (or use `cargo run --`).

## Configure a provider

```bash
# Anthropic (default)
export ANTHROPIC_API_KEY="sk-ant-..."

# OpenAI
xaft config set execution.provider openai
xaft config set execution.model gpt-4o

# Ollama (no key needed)
xaft config set execution.provider ollama
xaft config set execution.model llama3.2
```

Configuration persists in `~/.config/xaft/xaft.toml` (or `.xaft/xaft.toml` in
a project). See [Configuration](configuration.md) for the precedence rules.

## Run your first task

```bash
xaft run "Add error handling to all public functions in src/api/"
```

xaft will read the codebase, formulate a plan, edit files, run tests, and
commit — each step observable in the [TUI](tui.md) and reversible via the
per-session git worktree.

### Resume a session

```bash
xaft --resume <session-id>
```

The newest 20 turns are replayed into the transcript (see
[TUI resume](tui.md#resumed-transcript)).

## Next steps

- [Architecture](architecture.md) — how the runtime is put together
- [Modes](modes.md) — Safe → Plan → Yolo cycle
- [Workflows](workflows.md) — the plan→code→verify→commit pipeline
- [Tools](tools.md) — filesystem, git, and shell tools
