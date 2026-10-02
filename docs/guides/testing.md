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

Every `cargo` command here requires the agtrs build sandbox to be engaged
first — xaft path-depends on the sibling repo `../agtrs`, which is not ported,
so cargo otherwise aborts in `cargo metadata`. See
[Contributing](../contributing.md#building-and-testing).

```bash
just sandbox                    # engage once, at the start of a session
just test                       # all tests
just test-crate xaft-tui        # TUI unit tests
just test-one dry_run           # one test by substring
just lint                       # cargo clippy --workspace -- -D warnings
just fmt-check                  # formatting
just sandbox-off                # MANDATORY before `git add`
```

The raw equivalents are `cargo test --workspace`, `cargo test -p xaft-tui`,
`cargo clippy -p xaft-tui -D warnings` and `cargo fmt --check`.

> ### `LLVM ERROR: IO failure on output stream: No space left on device`
>
> An exhausted **temp filesystem**, not a code defect. `rustc` links every
> doctest binary under `TMPDIR`, so when it fills up the link fails and cargo
> blames the doctest — e.g. `jobs::JobStore (line 357) ... FAILED` — which
> reads exactly like a regression.
>
> Do not edit or `#[ignore]` the doctest, and do not delete other projects'
> data under `/tmp`. Redirect your own temp usage instead:
>
> ```bash
> export TMPDIR=/var/tmp/xaft-tmp && mkdir -p "$TMPDIR"
> ```
>
> `just test` and `just coverage` already do this — the justfile exports
> `TMPDIR=/var/tmp/xaft-tmp`, and an explicit `TMPDIR` of yours still wins — so
> hitting this means you ran bare `cargo`. Check the space any time with
> `just tmp-check`; the full write-up is in
> [Contributing § When a doctest fails with "No space left on device"](../contributing.md).

## Related

- [Contributing](../contributing.md)
- [Coverage baseline](../coverage-baseline.md)
