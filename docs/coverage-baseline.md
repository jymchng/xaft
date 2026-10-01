# Coverage baseline

Status: **NOT YET MEASURED — blocked.**

This page records the coverage pipeline that enforces the project-wide
85% line-coverage target, and the measured baseline that
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

**Status: MEASURED — 70.62%** (9708 of 13746 coverable lines), recorded
2026-09-30 against commit `1710384`. The per-crate table and the gap analysis
are filled in at the end of this page. The full per-directory and per-file
breakdown that seeds each test batch is also reproduced by
`scripts/coverage-report.py`.

The measurement runs **with the build sandbox engaged**, which is how the
manifest-resolution failure described below is bypassed:

```bash
./scripts/sandbox-build.sh
cargo tarpaulin --workspace --engine llvm --timeout 300 --skip-clean --out Json
python3 scripts/coverage-report.py tarpaulin-report.json
./scripts/sandbox-build.sh --off        # MANDATORY before `git add`
```

For the record, the failure this page originally documented was:
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

> **Status of the agtrs fix: still unauthorized, still not needed for xaft.**
> It was re-confirmed unapplied during the pass that re-established the test
> baseline: `/root/rust_projects/agtrs/Cargo.toml` still declares
> `injectable`, `injectable-runtime` and `injectable-graph` against the
> pre-rename `../injectable/...` paths, and `cargo metadata` there exits
> **101** with `failed to load manifest for workspace member .../agtrs` — the
> original defect, unchanged, with `git status` clean at `8a26310`.
>
> Two independent things follow. First, **xaft is not blocked by it**: the
> sandbox below builds and tests the whole workspace against a *ported clone*,
> never the real repository, so the suite is green either way. Second, the fix
> itself remains **out of scope** — `/root/rust_projects/agtrs` is a separate
> git repo in a different workspace, and it is the subject of its own
> authorization-gated goal rather than this one. It was not edited here.
> Applying it for real is a one-liner once authorized:
> `./scripts/port-agtrs-injectable-rename.sh --apply`, then decide with the
> operator whether to commit on agtrs `main`.

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

Measured with the sandbox engaged (at commit `aee84fe`, `TMPDIR=/var/tmp/xaft-test` — see the note on `/tmp` below):

| Command | Result |
| --- | --- |
| `cargo metadata` | exit 0, exactly **11** workspace members |
| `cargo check --workspace` | **exit 0** |
| `cargo test --workspace` | **exit 0 — 2,230 passed, 0 failed, 9 ignored**, 89 test binaries |

This is the **real test baseline**, and it replaces the previously
unreproducible "1842 passed / 9 ignored across 77 binaries" figure that G6
recorded. That number could not be re-derived: it predates the sandbox and was
taken on a tree whose exact state is no longer recoverable. The figures above
were reproduced end to end on the committed tree and are backed by a run log.

Two properties of this measurement are worth stating, because the earlier
figures quietly got both wrong:

- **Exactly 11 workspace members, no agtrs leakage.** `cargo metadata` lists
  `xaft`, `xaft-cli`, `xaft-config`, `xaft-runtime`, `xaft-agent`,
  `xaft-tools`, `xaft-agents`, `xaft-memory`, `xaft-skills`, `xaft-tui`,
  `xaft-session` — and nothing else. Grepping the run log for any `agtrs-*`
  test binary returns **no matches**, so the flattened clone contributes no
  tests and cannot inflate the totals.
- **All 9 "ignored" are ignored *doctests*, not skipped unit tests.** They are
  the doc examples in `xaft-agent` (`agent`, `builder` ×2, `plan_mode`,
  `signals` ×2), `xaft-agents` (`lib`, `registry`) and
  `xaft-tools::fs::read_before_edit`. No `#[test]` or `#[tokio::test]` is
  disabled anywhere in the workspace, so "0 failed" is a complete pass count
  rather than a pass count minus a skip list.

> **`/tmp` on this host is a full tmpfs.** `rustc` compiles every doctest
> binary under `TMPDIR`, and `/tmp` is a 12G tmpfs already filled to ~99% by
> *other* projects' scratch directories. Left alone, the suite exits 101 with
> `LLVM ERROR: IO failure on output stream: No space left on device` on a
> named doctest — which reads exactly like a real regression. Redirect to a
> filesystem with room and the same command passes:
>
> ```bash
> mkdir -p /var/tmp/xaft-test
> TMPDIR=/var/tmp/xaft-test cargo test --workspace   # exit 0
> ```

Earlier revisions of this page warned that a bare `cargo test --workspace`
also runs *agtrs's* own suites once the clone is flattened, and that
`agtrs-git` contributes a pre-existing failure
(`find_git_root_errors_on_non_repo`) — which is why they recommended scoping
with an explicit `-p` list. **That warning is wrong for the current tree**, and
the explicit `-p` list is not needed:

- `cargo metadata --no-deps` reports exactly **11** members, so the flattened
  clone does not become an implicit workspace member;
- the run log contains **no** `agtrs-*` test binary;
- the suite exits **0**, with none of agtrs's tests in the totals.

The clone is still only ever *read*; xaft's own test counts are unaffected by
it either way.

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

| Crate | covered | coverable | Baseline line coverage | missing |
| --- | --- | --- | --- | --- |
| `xaft` (bin) | 0 | 70 | 0.00% | 70 |
| `xaft-agent` | 174 | 280 | 62.14% | 106 |
| `xaft-cli` | 1013 | 1634 | 62.00% | 621 |
| `xaft-runtime` | 676 | 1055 | 64.08% | 379 |
| `xaft-tui` | 4641 | 6632 | 69.98% | 1991 |
| `xaft-tools` | 1672 | 2291 | 72.98% | 619 |
| `xaft-memory` | 224 | 275 | 81.45% | 51 |
| `xaft-config` | 819 | 963 | 85.05% | 144 |
| `xaft-agents` | 227 | 256 | 88.67% | 29 |
| `xaft-skills` | 134 | 151 | 88.74% | 17 |
| `xaft-session` | 128 | 139 | 92.09% | 11 |
| **workspace total** | **9708** | **13746** | **70.62%** | **4038** |

Reaching the 85% gate means covering **+1977** of the 4038 uncovered lines
without adding new uncovered source.

### Lowest-covered areas, which seed the test batches

By directory:

| Directory | covered/coverable | % | missing |
| --- | --- | --- | --- |
| `xaft-tui/src` | 2514/3886 | 64.7% | 1372 |
| `xaft-cli/src/commands` | 379/856 | 44.3% | 477 |
| `xaft-runtime/src` | 676/1055 | 64.1% | 379 |
| `xaft-tools/src/fs` | 831/1131 | 73.5% | 300 |
| `xaft-tui/src/slash/commands` | 457/658 | 69.5% | 201 |
| `xaft-tools/src/git` | 495/691 | 71.6% | 196 |
| `xaft-tui/src/menu` | 510/665 | 76.7% | 155 |
| `xaft-config/src` | 819/963 | 85.0% | 144 |
| `xaft-tui/src/slash` | 277/411 | 67.4% | 134 |
| `xaft-agent/src` | 174/280 | 62.1% | 106 |

Worst individual files by missing lines:

| missing | covered/coverable | file |
| --- | --- | --- |
| 355 | 835/1190 | `xaft-tui/src/state.rs` |
| 316 | 2/318 | `xaft-tui/src/app.rs` (0.6%) |
| 268 | 303/571 | `xaft-tui/src/renderer.rs` |
| 132 | 0/132 | `xaft-cli/src/commands/jobs.rs` |
| 130 | 191/321 | `xaft-runtime/src/orchestrator.rs` |
| 126 | 0/126 | `xaft-cli/src/commands/mcp.rs` |
| 118 | 232/350 | `xaft-tui/src/approval.rs` |
| 102 | 460/562 | `xaft-tui/src/menu/config_menu.rs` |
| 99 | 27/126 | `xaft-runtime/src/runtime.rs` |

### Progress against the baseline

| Change | Effect |
| --- | --- |
| baseline (commit `1710384`) | 70.62% (9708/13746) |
| `crates/xaft-cli/tests/jobs_handlers.rs` (23 tests) | `commands/jobs.rs` 0/132 → 128/132 (97.0%); `xaft-cli` 62.00% → 70.07% |
| `crates/xaft-cli/tests/mcp_handlers.rs` (24 tests) | `commands/mcp.rs` 0/126 → 107/126 |
| `crates/xaft-cli/tests/workflows_handlers.rs` (12 tests) | `commands/workflows.rs` 21/79 → most of the `list`/`run` handlers |
| **current (commit `e7565b9`)** | **72.79% (10006/13746)** — still **+1680** short of 85% |

Writing the `jobs` tests also found three rows marked ✅ that were actually
unreachable; see §1b of [docs/parity-matrix.md](parity-matrix.md).

### The suite is green — the blocker is narrower than it looked

Re-measured on the **literal working tree** (nothing moved aside, nothing
restored) at commit `aee84fe`, sandbox engaged:

```text
cargo metadata --no-deps                            exit 0, 11 members
cargo test  --workspace                             2230 passed / 0 failed / 9 ignored
```

So `cargo test --workspace` **is** green over the working tree as it stands.
It was previously reported as blocked on agtrs; that was wrong. The agtrs
blocker is real for the committed tree but is fully bypassed by
`scripts/sandbox-build.sh`, which clones agtrs, applies the already-proven port
to the CLONE (the real repo is only ever read), and repoints the manifests.

The earlier claim that the untracked G9 `terminals/` directory hangs
`cargo test` is **no longer true** and has been removed: no `terminals`
directory exists in the tree, no test hangs, and the run completes without
being killed. Nothing has to be moved aside to measure the suite. Goal 48
still owns writing and registering those four tools, but it no longer blocks
measurement.

One operational caveat remains, and it is environmental rather than
structural: on this host `/tmp` is a full tmpfs, so `TMPDIR` must be
redirected (see the note above) or the doctest link step fails with
`No space left on device`.

### What the next batches should target

Ranked by uncovered lines, from the `e7565b9` report:

| Missing | Covered | File |
| --- | --- | --- |
| 355 | 835/1190 | `xaft-tui/src/state.rs` |
| 316 | 2/318 | `xaft-tui/src/app.rs` |
| 268 | 303/571 | `xaft-tui/src/renderer.rs` |
| 130 | 191/321 | `xaft-runtime/src/orchestrator.rs` |
| 118 | 232/350 | `xaft-tui/src/approval.rs` |
| 102 | 460/562 | `xaft-tui/src/menu/config_menu.rs` |
| 99 | 27/126 | `xaft-runtime/src/runtime.rs` |
| 68 | 56/124 | `xaft-tools/src/fs/tree.rs` |

`xaft-tui` alone holds 1991 of the 3740 uncovered lines, so the target is
overwhelmingly the TUI: `app.rs` is essentially untested (2/318).

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

- The parity work tracked in [docs/parity-matrix.md](parity-matrix.md) needs a
  compiling workspace, and it now has one: the sandbox supplies a buildable
  dependency graph without editing agtrs. It is **not** blocked on the
  manifest-resolution failure above.
- Once the baseline is recorded, targeted tests are added to the
  lowest-covered crates until `cargo tarpaulin --workspace --engine llvm
  --fail-under 85` exits 0.
- A GitHub Actions job runs the same command with the same `--fail-under 85`
  threshold so the gate is enforced on every push.
