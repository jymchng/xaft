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
cannot see. The consequence is recorded rather than discovered:
[CI status](ci-status.md) measures that **no** job in `.github/workflows/` that
builds or tests xaft can run, and `just ci-status-check` fails if that stops
being true.

### Testing

See [Testing](guides/testing.md) for the layered strategy and commands.

```bash
just test                       # the whole suite
just test-crate xaft-tools      # one crate
just test-one dry_run           # one test by substring
just tmp-check                  # free space on the temp dir rustc links in
```

### When a doctest fails with "No space left on device"

> **This is an environmental fault, not a code defect.** Do not edit the
> doctest, do not add `#[ignore]` or `no_run` to it, and do not delete other
> projects' scratch data under `/tmp` to make room.

`rustc` compiles every doctest binary under `TMPDIR`. When that filesystem is
exhausted, the **link step** dies, and cargo reports the failure against the
doctest it happened to be building:

```text
test crates/xaft-cli/src/jobs/mod.rs - jobs::JobStore (line 357) ... FAILED

error: linking with 'cc' failed: exit status: 1
LLVM ERROR: IO failure on output stream: No space left on device
```

A healthy test, reported as a failure, under a name that points at real code.
On this host the same doctest passed with 586 MiB free on `/tmp` and failed to
link a few hundred MB less — identical tree, opposite outcome.

**The `just` recipes are already protected.** The justfile exports
`TMPDIR=/var/tmp/xaft-tmp` (a directory on the large filesystem) for every
recipe, and every test and coverage recipe additionally runs
`require-tmp-space` first. An explicit `TMPDIR` in your environment always wins
over that default.

So the failure means you ran **bare `cargo`**:

```bash
export TMPDIR=/var/tmp/xaft-tmp
mkdir -p "$TMPDIR"
cargo test --workspace        # now passes
```

**Preflight.** To see the space before a long run, or to get the full diagnosis:

```bash
just tmp-check                 # free space, plus a warning under 2 GiB
just tmp-check-self-test       # exercise the checker's own logic
./scripts/check-tmp-space.sh   # same, without just
```

Below 256 MiB it exits non-zero and explains the symptom above, names the
doctest that will be blamed for it, and lists the largest entries under the
temp directory. Raise the thresholds with `XAFT_TMP_MIN_KIB` and
`XAFT_TMP_WARN_KIB` if your needs differ.

Note that `crates/xaft-cli/src/jobs/mod.rs` line 357 is a **live** doctest that
must pass and must never be marked `ignore`/`no_run`. If it ever reports FAILED
with a linker error, that is this section, not a regression.

## License

MIT or Apache-2.0, at your option.
