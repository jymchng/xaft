# Contributing

Thanks for contributing to xaft!

## Development setup

```bash
git clone https://github.com/jymchng/xaft
cd xaft
```

Requires Rust 1.86+ (edition 2024 workspace).

Because xaft path-depends on the sibling repo `../agtrs`, which is not yet
ported, a bare `cargo build`/`cargo test` fails in `cargo metadata`. Engage the
build sandbox first — see [Building and testing](#building-and-testing):

```bash
./scripts/sandbox-build.sh   # clone + port agtrs, repoint manifests
cargo build --workspace
cargo test --workspace
./scripts/sandbox-build.sh --off   # MANDATORY before `git add`
```

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

## Building and testing

> **Read this before running `cargo test --workspace`.** A plain cargo command
> does not work in this repo yet, and the error it produces names a crate in a
> repository you have probably never heard of. One command fixes it.

### Why the sandbox exists

xaft's crates path-depend on the sibling repo `../agtrs`, which is a **separate
workspace** that has not been ported to the injectable crate rename. Cargo
therefore aborts inside `cargo metadata`, before compiling a single line:

```text
error: failed to get `injectable-runtime` as a dependency of package
  `agtrs-runtime v0.1.0 (/root/rust_projects/agtrs/agtrs-runtime)`
Caused by: failed to load source for dependency `injectable-runtime`
```

This is **not** a corrupt checkout and **not** something to fix by editing a
`Cargo.toml`. `scripts/sandbox-build.sh` is the supported way to work around
it: it clones `../agtrs` to a scratch directory **outside this repo**, applies
the already-proven `port-agtrs-injectable-rename.sh` port to the **clone**, and
repoints xaft's 11 manifests at it. The real `../agtrs` is only ever *read* —
never edited, stashed or committed.

The clone lives outside the repo tree on purpose. Cargo force-adds any path
dependency inside a workspace's directory tree to that workspace's member list,
and `exclude` cannot prevent it — so an in-tree sandbox would pull agtrs' own
crates (and their failing tests) into `cargo test --workspace` and report them
as xaft's failures. Outside the tree, the member count stays at exactly 11.

### The loop

```bash
just sandbox        # engage: clone, port, repoint, then cargo check
just test           # ... or build / coverage / ci / lint / run
just sandbox-status # report state at any time, changing nothing
just sandbox-off    # restore all 11 manifests, delete the clone
```

> **⚠ Run `just sandbox-off` BEFORE `git add`.**
> While the sandbox is engaged, 11 **tracked** `Cargo.toml` files carry
> rewritten `agtrs` paths. Staging without tearing down first commits those
> local paths, which breaks the build for everyone else. Teardown is safe to
> run even if you edited a manifest while engaged: it rebases your edit onto
> the pristine file rather than discarding it, prints the resulting diff, and
> fails loudly instead of silently reverting anything.

If you edit a `Cargo.toml` while engaged, the teardown preserves your change.
What it will **not** do is let you `cargo add` a dependency and assume it
survived — run `just sandbox-off` and confirm with
`git diff` before you commit.

### What the gate does

Every recipe that resolves the workspace graph (`test`, `build`, `check`,
`lint`, `coverage`, `ci`, `run`, `doc`, `tree`, `outdated`, `audit`, `watch`)
depends on a `require-sandbox` preflight. With the sandbox off it stops the
recipe and prints these instructions instead of letting cargo emit the
manifest error above:

```bash
$ just test
error: xaft's workspace graph does not resolve, so no cargo command can run.
  ...
      ./scripts/sandbox-build.sh          # engage
      just test                           # or any other cargo recipe
      ./scripts/sandbox-build.sh --off    # MANDATORY before `git add`
```

`fmt`, `fmt-check` and `members` are deliberately **not** gated: they read the
member list from `cargo metadata --no-deps`, which succeeds even when the graph
is broken, so they work with the sandbox off and gating them would only add a
failure mode.

CI runs raw `cargo` rather than these recipes, so a CI runner has no sandbox.
That is why the gate prints instructions instead of auto-engaging a clone it
cannot see.

### Testing

See [Testing](guides/testing.md) for the layered strategy and commands.

```bash
just test                       # the whole suite
just test-crate xaft-tools      # one crate
just test-one dry_run           # one test by substring
```

If you hit `LLVM ERROR: IO failure on output stream: No space left on device`
during a doctest link step, that is the host's `/tmp` being full rather than a
defect in the code — set `TMPDIR` to a directory on a larger filesystem and
re-run.

## License

MIT or Apache-2.0, at your option.
