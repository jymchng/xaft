# Coverage baseline

Status: **MEASURED — 77.29% (11141 of 14415 coverable lines), below the
85% target.** The gate runs and the suite is green; what is not yet met is
the coverage number itself, and the gap is tracked below rather than
described as blocked. An earlier revision of this page carried a header
declaring the *measurement* blocked, which contradicted the passing
measurements further down the same page; the blocker it meant was the agtrs
manifest failure, and `scripts/sandbox-build.sh` resolves that without
editing that repository. This header is the page's only status statement —
no other section declares one.

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

The reading named in the header above is the current one: **77.29%**
(11141 of 14415 coverable lines). It was taken from the `tarpaulin-report.json`
in the repository root at 2026-10-02 11:26, against commit `49e7556`; that
report is git-ignored, so it is evidence rather than a committed artifact.
The per-crate table and the gap analysis are filled in at the end of this
page. The full per-directory and per-file breakdown that seeds each test
batch is also reproduced by `scripts/coverage-report.py`.

**Which commit the number belongs to.** `49e7556` is the last commit that
changed anything under `crates/*/src`. The three commits since it
(`4bed280`, `46fdfbb`, `b631881`) add test files and rustfmt them; `46fdfbb`
also added `scripts/coverage-crate-summary.py` and refreshed this page. So
the 77.29% reading is the correct figure for HEAD `b631881` as well, and
neither the per-crate nor the per-file table below is stale — but the
attribution is to the *tree*, not to a commit that was HEAD when it was
recorded. Quote it as "77.29% at `b631881` (measured on the `49e7556` tree)".

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

### The recorded baseline

Re-running `just coverage` from the repository root reproduces the table
below. It records the **total line-coverage percentage**, the **per-crate
percentage** for each of the eleven members (`xaft`, `xaft-config`,
`xaft-cli`, `xaft-runtime`, `xaft-tools`, `xaft-agent`, `xaft-agents`,
`xaft-tui`, `xaft-session`, `xaft-memory`, `xaft-skills`), and the
**lowest-covered crates**, which seed the targeted test batches that close
the gap to 85%.

| Crate | covered | coverable | Baseline line coverage | missing |
| --- | --- | --- | --- | --- |
| `xaft` (bin) | 38 | 71 | 53.52% | 33 |
| `xaft-tools` | 1583 | 2291 | 69.10% | 708 |
| `xaft-tui` | 5014 | 6712 | 74.70% | 1698 |
| `xaft-agent` | 227 | 280 | 81.07% | 53 |
| `xaft-runtime` | 854 | 1064 | 80.26% | 210 |
| `xaft-memory` | 234 | 283 | 82.69% | 49 |
| `xaft-cli` | 1392 | 1680 | 82.86% | 288 |
| `xaft-config` | 819 | 963 | 85.05% | 144 |
| `xaft-agents` | 227 | 256 | 88.67% | 29 |
| `xaft-skills` | 134 | 151 | 88.74% | 17 |
| `xaft-session` | 619 | 664 | 93.22% | 45 |
| **workspace total** | **11141** | **14415** | **77.29%** | **3274** |

The rows sum to the workspace total exactly (11141 covered, 3274 missing).

`xaft` (bin) is four files at the repository root — `src/main.rs` 38/52,
`src/lifecycle.rs` 0/8, `src/query.rs` 0/11, and an empty `src/lib.rs`. An
earlier revision of this table counted only `src/main.rs` and so showed
38/52 (73.08%) with 14 missing; that omitted 19 coverable lines and made the
column sum to 14396 rather than the 14415 the report actually reports. The
53.52% above is the honest figure, and it makes `xaft` the *lowest*-covered
member of the workspace — which it is, not the 73.08% a reader would infer.

Reaching the 85% gate means covering **+1112** of the 3274 uncovered lines
without adding new uncovered source.

### Lowest-covered areas, which seed the test batches

By directory:

| Directory | covered/coverable | % | missing |
| --- | --- | --- | --- |
| `src` (root binary crate) | 38/71 | 53.5% | 33 |
| `crates/xaft-tui/src` | 5014/6712 | 74.7% | 1698 |
| `crates/xaft-tools/src` | 1583/2291 | 69.1% | 708 |
| `crates/xaft-cli/src` | 1392/1680 | 82.9% | 288 |
| `crates/xaft-runtime/src` | 854/1064 | 80.3% | 210 |
| `crates/xaft-config/src` | 819/963 | 85.0% | 144 |
| `crates/xaft-agent/src` | 227/280 | 81.1% | 53 |
| `crates/xaft-memory/src` | 234/283 | 82.7% | 49 |
| `crates/xaft-session/src` | 619/664 | 93.2% | 45 |
| `crates/xaft-agents/src` | 227/256 | 88.7% | 29 |
| `crates/xaft-skills/src` | 134/151 | 88.7% | 17 |

Worst individual files by missing lines (reproduced by
`scripts/coverage-crate-summary.py tarpaulin-report.json`):

| missing | covered/coverable | file |
| --- | --- | --- |
| 331 | 864/1195 | `xaft-tui/src/state.rs` |
| 275 | 2/277 | `xaft-tui/src/app.rs` (0.7%) |
| 268 | 303/571 | `xaft-tui/src/renderer.rs` |
| 102 | 460/562 | `xaft-tui/src/menu/config_menu.rs` |
| 67 | 57/124 | `xaft-tools/src/fs/tree.rs` |
| 60 | 67/127 | `xaft-runtime/src/runtime.rs` |
| 60 | 61/121 | `xaft-tui/src/slash/mod.rs` |
| 54 | 108/162 | `xaft-tui/src/bridge.rs` |
| 49 | 383/432 | `xaft-tui/src/render/markdown.rs` |
| 46 | 253/299 | `xaft-tui/src/mention.rs` |
| 46 | 142/188 | `xaft-cli/src/commands/config.rs` |
| 42 | 81/123 | `xaft-runtime/src/provider.rs` |
| 41 | 278/319 | `xaft-tui/src/input_bar.rs` |
| 41 | 39/80 | `xaft-tools/src/fs/file_stat.rs` |

`commands/jobs.rs` and `commands/mcp.rs` no longer appear: they were at
0/132 and 0/126 until the handler tests landed, and are now largely covered.

### Progress against the baseline

| Change | Effect |
| --- | --- |
| baseline (commit `1710384`) | 70.62% (9708/13746) |
| `crates/xaft-cli/tests/jobs_handlers.rs` (23 tests) | `commands/jobs.rs` 0/132 → 128/132 (97.0%) |
| `crates/xaft-cli/tests/mcp_handlers.rs` (24 tests) | `commands/mcp.rs` 0/126 → 107/126 |
| `crates/xaft-cli/tests/workflows_handlers.rs` (12 tests) | `commands/workflows.rs` 21/79 → most of the `list`/`run` handlers |
| git-tool batches (`git_grep`/`git_diff`/`git_log`/`git_merge`, 20 tests) | `xaft-tools/src/git` option handling, error paths and schema contracts |
| bootstrap batch (`src/main.rs`, 6 tests) | `src/main.rs` 0/52 → 38/52 — the binary crate had no tests at all (`lifecycle.rs` and `query.rs` are still 0/8 and 0/11) |
| `crates/xaft-tui/tests/app_state_bookkeeping.rs` (21 tests) | `AppState::push_separator`, `take_mode_notification`, `check_ctrl_c_timeout`, `flush_tool_group`, `init_skills` — five methods with **zero** prior references |
| `crates/xaft-tui/tests/approval_queue_bulk.rs` (21 tests) | `ApprovalQueue::approve_all_up_to`, `reject_all`, focus wrap-around, `ToolPreview::content_height` |
| `crates/xaft-tui/tests/ephemeral_region.rs` (18 tests) | `xaft-tui/src/ephemeral.rs` 0/34 → 34/34 — the spinner line, status line and mode badge had no test at all |
| **current (HEAD `b631881`)** | **77.29% (11141/14415)** — still **+1112** short of 85% |

Writing the `jobs` tests also found three rows marked ✅ that were actually
unreachable; see §1b of [docs/parity-matrix.md](parity-matrix.md).

### The suite is green — the blocker is narrower than it looked

Re-measured on the **literal working tree** (nothing moved aside, nothing
restored) at commit `49e7556`, sandbox engaged:

```text
cargo metadata --no-deps                            exit 0, 11 members
cargo test  --workspace                             2333 passed / 0 failed / 9 ignored
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

Ranked by uncovered lines, from the same report as the table above:

| Missing | Covered | File |
| --- | --- | --- |
| 331 | 864/1195 | `xaft-tui/src/state.rs` |
| 275 | 2/277 | `xaft-tui/src/app.rs` |
| 268 | 303/571 | `xaft-tui/src/renderer.rs` |
| 102 | 460/562 | `xaft-tui/src/menu/config_menu.rs` |
| 67 | 57/124 | `xaft-tools/src/fs/tree.rs` |
| 60 | 67/127 | `xaft-runtime/src/runtime.rs` |
| 60 | 61/121 | `xaft-tui/src/slash/mod.rs` |
| 54 | 108/162 | `xaft-tui/src/bridge.rs` |

`xaft-tui` alone holds 1698 of the 3274 uncovered lines, so the target is
overwhelmingly the TUI: `app.rs` is essentially untested (2/277) because it
is the terminal event loop and needs a real terminal, and `renderer.rs`
needs the incremental-renderer cursor state driven through a sequence of
mutations. Both are worth a deliberate harness rather than scattered unit
tests.

`state.rs` remains the largest single file, but it is now well covered
relative to its size (72.3%); the remaining misses are the
`handle_event` / `handle_key` match arms, which are best reached through
the event stream above.

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
- Targeted tests are being added to the lowest-covered crates until
  `cargo tarpaulin --workspace --engine llvm --fail-under 85` exits 0; the
  baseline above is what those batches are measured against.
- `.github/workflows/coverage.yml` runs the same measurement on every push,
  but it is a **reporting** gate, not an enforcing one. Its `COVERAGE_MIN`
  is `0`, passed straight through as `--fail-under`, so it cannot fail while
  the tree is below target. The threshold is wired and unarmed deliberately:
  arming it at 85 today would mean a red check on every commit, which trains
  contributors to ignore CI. `just coverage` is the enforcing gate locally
  and is what has to exit 0.
