# Testing

xaft follows a layered testing strategy across the workspace.

## Unit tests

Each crate ships `#[cfg(test)]` modules covering pure logic:

- `xaft-tui`: triggers, paste placeholder, tool-group collapse, resume-tail
  bounds, mode cycle, telemetry, diff truncation
- `xaft-config`: load/merge/validate/interpolate, watcher
- `xaft-tools`: fs/git/shell tool schemas + path guards
- `xaft-agent`: plan mode, prompts, streaming

## Integration tests

`tests/` dirs per crate exercise the real APIs without network:

- `xaft-tools/tests/*_integration.rs` — fs/git tool executor against temp dirs
- `xaft-config/tests/*` — config file round-trips
- `xaft-runtime/tests/*` — session, handoff, exploration pool, resume

## E2E tests

`xaft-agent/tests/e2e_tests.rs` and `xaft-tui/tests/` run full journeys with a
recorded provider cassette where possible.

## Commands

```bash
cargo test --workspace          # all tests
cargo test -p xaft-tui          # TUI unit tests
cargo clippy -p xaft-tui -D warnings
cargo fmt --check
```

## Related

- [Contributing](../contributing.md)
