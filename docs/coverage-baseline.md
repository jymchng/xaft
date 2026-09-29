# Coverage baseline

Status: **NOT YET MEASURED — blocked.**

This page records the coverage pipeline that enforces the project-wide
85% line-coverage target, and the (currently unmeasured) baseline that
[docs/parity-matrix.md](parity-matrix.md) delivery work is measured against.

## The gate

`just coverage` is the single entry point for the coverage gate:

```just
# Generate coverage report with tarpaulin, enforcing the 85% line-coverage gate.
coverage:
    cargo tarpaulin \
        --workspace \
        --engine llvm \
        --timeout 300 \
        --skip-clean \
        --fail-under 85
```

- `--workspace` — every member crate, not just the `xaft` binary, is instrumented.
- `--engine llvm` — uses `rustc`'s LLVM source-based coverage, so no
  `RUSTFLAGS`/`ptrace` interaction is needed and the numbers match what
  `cargo-llvm-cov` would report.
- `--timeout 300` — a test that hangs for five minutes is treated as a failure
  rather than blocking the pipeline forever.
- `--skip-clean` — keeps the instrumented build cache between runs, which is
  what makes a full-workspace run bearable on a repeat run.
- `--fail-under 85` — **the enforced threshold.** If total line coverage lands
  below 85%, tarpaulin exits non-zero and `just coverage` fails.

`--fail-under` is deliberately in the developer-facing recipe and not only in
CI, so that a coverage regression is caught locally rather than after a push.

## Generated artifacts

Tarpaulin writes its reports into the repository root. These are ignored by
`.gitignore` and are never committed:

| Artifact | Format |
| --- | --- |
| `/tarpaulin-report.*` | HTML, JSON, plaintext renderings |
| `/cobertura.xml`, `/XmlCobertura.xml` | Cobertura XML |
| `/lcov.info` | LCOV |

Only report artifacts are ignored. `CARGO_TARGET_DIR` (and therefore the
instrumented build cache) is covered by the pre-existing `/target` entry and by
the `CARGO_TARGET_DIR` override at the top of the `justfile`.

## Baseline measurement

**Status: not yet recorded.**

`just coverage` currently aborts before instrumentation begins, so no
percentage has been observed yet. The failure is *not* a coverage failure — it
is a Cargo manifest-resolution failure that happens upstream of tarpaulin:

```
error: failed to get `injectable-runtime` as a dependency of package
       `agtrs-runtime v0.1.0 (/root/rust_projects/agtrs/agtrs-runtime)`
  ... which satisfies path dependency `agtrs-runtime` (locked to 0.1.0) of
  package `xaft-runtime v0.1.0 (/root/rust_projects/xaft/crates/xaft-runtime)`
  ... which satisfies path dependency `xaft-runtime` (locked to 0.1.0) of
  package `xaft v0.1.0 (/root/rust_projects/xaft)`

Caused by:
  failed to read `/root/rust_projects/injectable/injectable-runtime/Cargo.toml`
Caused by:
  No such file or directory (os error 2)
```

The sibling repository `/root/rust_projects/agtrs` (at commit `8a26310`) still
declares the pre-rename dependency names `injectable`, `injectable-runtime` and
`injectable-graph`, which were renamed to `injectable-rs`, `injectable-rs-runtime`
and `injectable-rs-graph`. `xaft` itself was already repointed (commit `2544f68`).
Cargo's `[patch]` and `[replace]` cannot repair this, because the broken
dependencies are *path* dependencies declared by another workspace.

Because `cargo metadata` must resolve the whole graph before tarpaulin can
start, this blocks measurement even for a single crate:

```
$ cargo tarpaulin -p xaft-config --engine llvm --timeout 60
Error: `cargo metadata` exited with an error: ...
```

of the eleven workspace members, only `xaft-config` and `xaft-skills` are
free of an `agtrs` dependency, and neither is reachable until the graph
resolves.

### How to unblock

A prepared, dry-run-verified port script is committed in this repository:

```bash
./scripts/port-agtrs-injectable-rename.sh            # dry run, reports the plan
./scripts/port-agtrs-injectable-rename.sh --apply    # write the 170 substitutions
./scripts/port-agtrs-injectable-rename.sh --check    # verify only
```

It rewrites the 3 workspace dependencies, the 20 member manifest dependency
keys and feature references, the 138 `::`-path uses in 46 `.rs` files, and the
11 `injectable/axum` occurrences in agtrs' CI workflows. It is idempotent,
enumerates files with `git ls-files`, and refuses to write unless `--apply` is
given. Applying it to `/root/rust_projects/agtrs` is out of scope for this
repository without explicit authorization, since that repository belongs to a
different workspace; rollback is `git -C /root/rust_projects/agtrs checkout -- .`.

#### The port is verified, not just written

`scripts/verify-agtrs-port.sh` proves the port works **without touching the real
agtrs repository**. It clones agtrs to a scratch directory inside this repo,
symlinks `./injectable -> ../injectable` so the clone's relative path
dependencies resolve at their true depth, applies the port, and compiles:

```bash
./scripts/verify-agtrs-port.sh            # full: clone, A/B, cargo check --all-targets
./scripts/verify-agtrs-port.sh --fast     # manifest A/B only, ~10 s
```

Result of the full run:

| Step | Expected | Observed |
| --- | --- | --- |
| unported clone, `cargo metadata` | fail (reproduces the bug) | **exit 101**, `failed to load manifest for workspace member .../agtrs` |
| ported clone, `cargo metadata` | succeed | **exit 0**, 297 packages, all four `injectable-rs*` crates resolved |
| ported clone, `cargo check --workspace --all-targets` | succeed | **exit 0** in 4 m 14 s, 0 errors |
| diff shape | pure 1:1 rename | 62 files, **170 insertions, 170 deletions** |
| diagnostics mentioning `injectable` | none but progress lines | 4 lines, all `Checking injectable-rs*` |

`--all-targets` is what makes this conclusive: it compiles every lib, bin,
integration test and example in agtrs, so the renamed dependency keys *and*
every renamed `::` path are proven to build. The remaining `agtrs-runtime`
warnings are pre-existing and unrelated to the rename.

The real `/root/rust_projects/agtrs` is only ever read (cloned from); it is
never written, stashed or checked out. After the run the scratch directory,
the symlink and the temporary workspace `exclude` are all removed, leaving
agtrs at `8a26310` with zero modified files.

### The local build sandbox

`scripts/sandbox-build.sh` makes xaft compile and test **without** the agtrs
fix, so coverage and parity work are not blocked on a repo we may not edit.

It clones agtrs into `.agtrs-sandbox/`, applies the proven port, and repoints
xaft's `path = "../../../agtrs/..."` deps at the clone. Two Cargo subtleties had
to be solved along the way, both found empirically:

1. **A nested workspace is re-parented.** A path dependency inside another
   workspace's directory tree is attributed to the *ancestor* workspace, even
   though it declares its own `[workspace]` and the ancestor lists it in
   `exclude`. `scripts/flatten_agtrs_workspace.py` therefore removes the nested
   workspace entirely: it inlines every `workspace = true` inheritance
   (`version.workspace`, `dep = { workspace = true }`, and the
   `{ workspace = true, features = [...] }` union) into each member manifest and
   re-anchors relative paths. After flattening there is no nested workspace to
   mis-attribute.
2. **Symlinks do not help for the same reason.** A local `./injectable` symlink
   still makes cargo walk *up* to xaft's root, where it then tries to inherit
   `authors` from xaft. The sandbox rewrites those paths to the real absolute
   `/root/rust_projects/injectable/...`, which is the same dependency xaft
   already declares directly.

```bash
./scripts/sandbox-build.sh      # engage, then cargo check --workspace
./scripts/sandbox-build.sh --status
./scripts/sandbox-build.sh --off  # restore all 11 manifests, delete the clone
```

Originals are backed up under `.agtrs-sandbox/originals/` and `.agtrs-sandbox/`
is git-ignored, but the sandbox edits to `Cargo.toml` are **not** committed —
run `--off` before `git add`.

Measured with the sandbox engaged:

| Command | Result |
| --- | --- |
| `cargo metadata` | exit 0 |
| `cargo check --workspace --all-targets` | **exit 0**, 4 m 34 s |
| `cargo test` over the 11 xaft packages | **exit 0 — 1,630 passed, 0 failed, 9 ignored**, 71 binaries |

Note that once agtrs is flattened its crates become implicit members of xaft's
workspace, so a bare `cargo test --workspace` also runs *agtrs's* own suites
(and `agtrs-git` has one pre-existing failure,
`find_git_root_errors_on_non_repo`). Scope xaft runs with explicit
`-p xaft -p xaft-config -p xaft-cli -p xaft-runtime -p xaft-tools -p xaft-agent
-p xaft-agents -p xaft-tui -p xaft-session -p xaft-memory -p xaft-skills`.

The real agtrs remains unported and untouched; this only removes the local
blocker so work can continue.

### What to record once unblocked

Run `just coverage` from the repository root and record, in the table below:

1. the **total line-coverage percentage** reported at the end of the run;
2. the **per-crate percentage** for every one of the eleven members:
   `xaft`, `xaft-config`, `xaft-cli`, `xaft-runtime`, `xaft-tools`,
   `xaft-agent`, `xaft-agents`, `xaft-tui`, `xaft-session`, `xaft-memory`,
   `xaft-skills`;
3. the **lowest-covered crates**, which seed the targeted test batches that
   close the gap to 85%.

| Crate | Baseline line coverage |
| --- | --- |
| `xaft` (bin) | _unmeasured_ |
| `xaft-config` | _unmeasured_ |
| `xaft-cli` | _unmeasured_ |
| `xaft-runtime` | _unmeasured_ |
| `xaft-tools` | _unmeasured_ |
| `xaft-agent` | _unmeasured_ |
| `xaft-agents` | _unmeasured_ |
| `xaft-tui` | _unmeasured_ |
| `xaft-session` | _unmeasured_ |
| `xaft-memory` | _unmeasured_ |
| `xaft-skills` | _unmeasured_ |
| **workspace total** | _unmeasured_ |

## Reproducing

```bash
just coverage                       # enforced; non-zero exit below 85%
cargo tarpaulin --workspace --engine llvm --timeout 300 --skip-clean \
                 --out Stdout        # same run, no HTML report written
```

Requires the `llvm-tools-preview` rustup component:

```bash
rustup component add llvm-tools-preview
```

## Relationship to the rest of the plan

- The parity work tracked in [docs/parity-matrix.md](parity-matrix.md) is
  gated on a compiling workspace, so the manifest-resolution failure above
  blocks that work as well as this measurement.
- Once the baseline is recorded, targeted tests are added to the
  lowest-covered crates until `cargo tarpaulin --workspace --engine llvm
  --fail-under 85` exits 0.
- A GitHub Actions job runs the same command with the same `--fail-under 85`
  threshold so the gate is enforced on every push.
