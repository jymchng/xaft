# Contributing

Thanks for contributing to xaft!

## Development setup

```bash
git clone https://github.com/jymchng/xaft
cd xaft
cargo build --workspace
cargo test --workspace
```

Requires Rust 1.86+ (edition 2024 workspace).

## Crate layout

Keep changes in the crate that owns the concern:

| Concern | Crate |
|---|---|
| Terminal UI | `crates/xaft-tui` |
| Runtime / orchestration | `crates/xaft-runtime` |
| CLI | `crates/xaft-cli` |
| Tools | `crates/xaft-tools` |
| Config | `crates/xaft-config` |
| Sessions | `crates/xaft-session` |
| Memory | `crates/xaft-memory` |
| Skills | `crates/xaft-skills` |

## Adding a feature

1. Write a PRD under `prds/` (numbered, mirroring the existing set) or a
   knowledge note under `knowledge/` when the change is investigative.
2. Implement with unit tests in the owning crate.
3. Add an integration test when it touches the runtime or tools.
4. Run `cargo clippy -D warnings` and `cargo fmt --check` on touched crates.
5. Update `docs/` (guides or reference) and `CHANGELOG.md`.

## Testing

See [Testing](guides/testing.md) for the layered strategy and commands.

## License

MIT or Apache-2.0, at your option.
