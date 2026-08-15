# Contributing to xaft

The canonical contributor guide is [docs/contributing.md](docs/contributing.md).
Read it together with [CLAUDE.md](CLAUDE.md) and [AGENTS.md](AGENTS.md).

## Setup

```bash
git clone https://github.com/jymchng/xaft
cd xaft
cargo build --workspace
cargo test --workspace
```

Requires Rust 1.86+ (edition 2024 workspace).

## Development checks

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -D warnings
cargo fmt --check
node scripts/docs-site.cjs --check     # docs build + link check
```

Report failures from a clean checkout rather than silently changing the
environment.

## Design invariants

- The TUI render loop is single-threaded and lock-free; all state mutations
  happen on the main thread.
- The runtime emits signals on the `SignalBus`; the TUI bridges them via
  `EventBridge` and never reaches into the runtime directly.
- Tools use capability, path, approval, timeout, and retry contracts.
- Platform-specific terminal calls stay in the dedicated terminal backends.
- Durable conversation and tool replay must not duplicate side effects.

## Adding a feature

1. Write a PRD under `prds/` (numbered, mirroring the existing set) or a
   knowledge note under `knowledge/` when the change is investigative.
2. Implement with unit tests in the owning crate.
3. Add an integration test when it touches the runtime or tools.
4. Run `cargo clippy -D warnings` and `cargo fmt --check` on touched crates.
5. Update `docs/` (guides or reference), `llms.txt`/`llms-full.txt` (via
   `node scripts/docs-site.cjs`), and `CHANGELOG.md`.

For tests, keep unit tests in each crate's `#[cfg(test)]` modules and
integration tests in `crates/*/tests/`. For new public exports, update
`llms-full.txt`.

## License

MIT or Apache-2.0, at your option.
