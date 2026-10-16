# Coverage baseline

Status: **MEASURED — 85.96% (14189 of 16507 coverable lines), which MEETS the
85% target.** The gate is a percentage comparison, so it needs
`ceil(0.85 × 16507) = 14031` covered lines; the reading clears that by **158
lines**. The gate runs, the suite is green, and the coverage number clears it
too. An earlier revision of this page carried a header
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
        --out Json \
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
- `--out Json` — writes `tarpaulin-report.json`, which is what
  `scripts/uncovered.py`, `scripts/coverage-report.py` and
  `scripts/coverage-crate-summary.py` read. Without it tarpaulin writes **no
  report file at all** (the default output backend is stdout only), so this flag
  is what makes the per-line tooling usable. See "The report is written even
  when the gate fails" below.
- `--fail-under 85` — **the enforced threshold.** If total line coverage lands
  below 85%, tarpaulin exits non-zero and `just coverage` fails.

`--fail-under` is deliberately in the developer-facing recipe and not only in
CI, so that a coverage regression is caught locally rather than after a push.

### The report is written even when the gate fails

This was long assumed to be the other way round — that a failing gate suppresses
the JSON write and therefore forces a second full measurement run to find out
which lines a batch covered. It does not. Measured on cargo-tarpaulin 0.35.4,
the version actually installed here:

| Invocation | Exit | `tarpaulin-report.json` |
| --- | --- | --- |
| `--fail-under 100` | 1 | **absent** |
| `--fail-under 100 --out Json` | 1 | **present** |

The gate is not what suppresses the report; the missing `--out Json` is.
tarpaulin writes the report *before* it evaluates the threshold — in
`report_coverage_with_check` it calls `report_coverage` and only then
`check_fail_threshold`. So `just coverage` now leaves a usable
`tarpaulin-report.json` behind whether it passes or fails, and the per-line
tooling costs no extra run:

```bash
just coverage                                    # exits non-zero below 85%, report still written
python3 scripts/uncovered.py crates/xaft-tui/src/app.rs
```

The human-readable stdout summary is unaffected — `--out Json` adds a report
file, it does not replace the terminal output.

`just coverage-report` is the non-enforcing twin: same flags, no `--fail-under`.
While the workspace is below the target every `just coverage` exits non-zero,
which is correct for a gate but noisy in the measure/edit/re-measure loop. It
writes the same report and is the more convenient command between batches.
`coverage` remains the enforcing entry point and the Definition of Done.

**Superseded goal-list annotations.** Goals `9a07578f` (G11b-2) and `9b0b8502`
(G11b-4) both carry the same now-false hygiene rule: that a batch must be
measured a second time with the enforced gate dropped, because the gate was
thought to suppress the report. The table above falsifies that premise — the
missing `--out Json` was the cause, and the gate never was. Goal text is
immutable, so the correction lives here: one `just coverage` run yields both the
pass/fail verdict and the per-line report, so neither goal's batches owe a
second measurement run.

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

The reading named in the header above is the current one: **85.96%**
(14189 of 16507 coverable lines). It was taken from the `tarpaulin-report.json`
in the repository root (4,187,005 bytes, mtime 2026-10-14 15:03); that
report is git-ignored, so it is evidence rather than a committed artifact,
and a later run overwrites it. The copy on disk at the time of writing is a
subsequent resample of the same tree (4,187,007 bytes, mtime 15:32) reading
14180/16507 = 85.90% — inside the run-to-run spread described below, so it
refines this reading rather than revising it.
The per-crate table and the gap analysis are filled in at the end of this
page. The full per-directory and per-file breakdown that seeds each test
batch is also reproduced by `scripts/coverage-report.py`.

**Which commit the number belongs to — the *tree*, not a commit.** The
reading is a **working-tree** reading: the gate was run with the build
sandbox engaged over the literal working tree at HEAD `5643836` plus the
G11b-2 closing batch
(`crates/xaft-tools/tests/git_tool_schema_contracts.rs`). That batch is now
committed as `28a9083`, so the reading corresponds to the committed tree at
`28a9083` rather than to uncommitted work. `28a9083` changed only
`crates/xaft-tools/tests/` and this page, so the last commit that changed
anything under `crates/*/src` or `src` before this reading is still
`5643836`.
Quote the figure as "85.96% on the tree at `28a9083`" rather than
attributing it to a commit it cannot be reproduced from. Note also that
`28a9083` is **not** HEAD. One commit after it, `084fdf1` — a repository-wide
rustfmt pass — rewrote thirteen production files under `crates/`, among them
`xaft-tools/src/registry.rs`, `xaft-tools/src/terminals/manager.rs`,
`xaft-tui/src/replay.rs` and `xaft-config/src/provider.rs`. A rustfmt pass is
not guaranteed to leave coverable line counts unchanged, so the figure belongs
to `28a9083` and **must be re-measured before it is asserted of HEAD's tree**.
The header's status statement should be read with that attached.

**Do not over-read a single run.** Two runs of an identical tree have been
measured to differ by 131-166 covered lines (goals `d9b3af32` and
`b6b64dee`), so no `+N` delta is meaningful without a fresh run on the same
tree, and no batch should be judged a regression from one reading. The same
caveat applies to the `Progress against the baseline` table below: each
`N → M` cell there is a pair of single readings, so read those rows as
direction and magnitude rather than as exact line counts.

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

> **Status of the agtrs fix: DECLINED by default after five unanswered
> authorization requests. The sandbox below is the supported build path.**
> The defect is unchanged and re-confirmed by measurement: `/root/rust_projects/
> agtrs/Cargo.toml` still declares `injectable`, `injectable-runtime` and
> `injectable-graph` against the pre-rename `../injectable/...` paths, and
> `cargo metadata` there exits **101** with `failed to load manifest for
> workspace member .../agtrs`, with `git status` clean at `8a26310` and zero
> modified files.
>
> This decision was put to the operator as an explicit three-way choice —
> (a) run the port against the real agtrs and commit on its `main`, (b) patch
> it but leave it uncommitted for review, (c) decline and make the sandbox the
> documented, supported build path. **No answer was recorded**, which is the
> fifth consecutive time the same question has gone unanswered (the earlier
> requests belong to the G0b, agtrs-port, build-baseline and agtrs-memory
> goals). A timeout is not an authorization, so (a) and (b) are both
> unavailable and (c) is what stands: **do not edit agtrs from this
> repository**, and build through the sandbox.
>
> Two consequences follow, and the second is the real cost of (c).
>
> 1. **xaft is not blocked locally.** The sandbox below builds and tests the
>    whole workspace against a *ported clone*, never the real repository, so
>    the suite is green either way. Every `just` recipe that resolves the
>    workspace graph is gated behind a `require-sandbox` preflight that stops
>    and prints the engage instructions rather than letting cargo emit the
>    manifest error, so an operator cannot silently hit the failure.
> 2. **CI cannot build xaft, and this option does not fix that.** Five of the
>    seven workflows (`tests`, `coverage`, `lint`, `codeql`, `release`) run
>    raw `cargo`. Every one of their jobs checks out xaft alone — a plain
>    `actions/checkout@v4`, with no agtrs sibling — and **not one of the five
>    mentions `sandbox`, `agtrs` or `injectable`, nor invokes `just`**, so
>    none of them can reach the sandbox and none even trips the
>    `require-sandbox` gate that local runs get. The sandbox cannot help them
>    anyway, because its clone deliberately lives **outside** the repository
>    tree — that placement is itself load-bearing, since cargo force-adds
>    in-tree path dependencies to the workspace member list and `exclude` does
>    not prevent it. So under (c) the "buildable workspace" exists for local
>    work but not for a CI runner. This is stated plainly rather than left for
>    a reader to discover.</new_string>
>
> Reversal is cheap and stays a one-liner if the decision ever changes:
> `./scripts/port-agtrs-injectable-rename.sh --apply`, then decide with the
> operator whether to commit on agtrs `main`. The script is already proven
> against a clone (see the verification table below), so authorizing it is a
> matter of consent, not of work. Rollback is
> `git -C /root/rust_projects/agtrs checkout -- .`.

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

### The local build sandbox — the supported build path

`scripts/sandbox-build.sh` makes xaft compile and test **without** the agtrs
fix, so coverage and parity work are not blocked on a repo we may not edit.
Because porting agtrs for real was declined (see the decision above), this
sandbox is the **supported way to build and test xaft**, not a temporary
workaround; `docs/contributing.md` §"Building and testing" and the `just
sandbox` / `sandbox-status` / `sandbox-off` recipes are the documented
entry points.

It clones agtrs into `/root/rust_projects/.xaft-agtrs-sandbox/` — **outside**
the repository tree, deliberately — applies the proven port, and repoints
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

Originals and the post-rewrite snapshots are backed up **outside** the clone,
in a sibling state directory `/root/rust_projects/.xaft-agtrs-sandbox.state/`
(`originals/` and `patched-manifests/` there), so deleting the clone cannot
destroy the restore path. Neither directory is inside the repository, so
neither can be committed. The sandbox edits to `Cargo.toml` are **not**
committed either — run `--off` before `git add`. `./scripts/sandbox-build.sh
--status` reports `ENGAGED` / `CLEAN` by inspecting the manifests themselves
and exits non-zero when they name the sandbox, so it is safe as a pre-commit
check.

`--off` (`just sandbox-off`) ends with a teardown summary rather than a bare
success, because "restored" and "restored without touching your work" are
different claims:

```
teardown summary:
  restored unchanged     : N manifest(s)
  edits preserved        : M manifest(s)
  sandbox removed
```

A clean no-edit cycle prints `restored unchanged: 11 / edits preserved: 0`.
A manifest you edited while engaged is **not** reverted: teardown rebases your
edit onto the pristine original (your bytes win, the sandbox's agtrs path
rewrite does not) and counts it under `edits preserved`, printing the resulting
diff. Two counters appear only when they are non-zero — `DELETED while engaged`
and `NEEDS HAND-REAPPLY` — and either one makes teardown print `REFUSING to
report a clean teardown` and **exit non-zero**; in the re-apply case your bytes
are parked beside the manifest as `<file>.sandbox-edit-backup`. Teardown also
refuses to exit 0 if any manifest still names the sandbox afterwards, because
the tree is then unbuildable and must not be committed.

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
>
> **This is now handled by the build tooling, not by memory.** The justfile
> exports `TMPDIR=/var/tmp/xaft-tmp` for every recipe, so `just test` and
> `just coverage` are covered without you doing anything; every test and
> coverage recipe also runs a `require-tmp-space` preflight that fails with a
> diagnosis instead of an ENOSPC link error. `just tmp-check` reports the
> space on demand. Only bare `cargo` invocations still need the explicit
> `TMPDIR=` above. See
> [Contributing § When a doctest fails with "No space left on device"](contributing.md).

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
| `xaft` (bin) | 40 | 73 | 54.79% | 33 |
| `xaft-agent` | 227 | 280 | 81.07% | 53 |
| `xaft-runtime` | 877 | 1064 | 82.42% | 187 |
| `xaft-memory` | 234 | 283 | 82.69% | 49 |
| `xaft-tools` | 3044 | 3611 | 84.30% | 567 |
| `xaft-cli` | 1538 | 1813 | 84.83% | 275 |
| `xaft-tui` | 5794 | 6756 | 85.76% | 962 |
| `xaft-agents` | 227 | 256 | 88.67% | 29 |
| `xaft-skills` | 134 | 151 | 88.74% | 17 |
| `xaft-config` | 1448 | 1556 | 93.06% | 108 |
| `xaft-session` | 626 | 664 | 94.28% | 38 |
| **workspace total** | **14189** | **16507** | **85.96%** | **2318** |

The rows sum to the workspace total exactly (14189 covered, 2318 missing),
and both equal the report's own top-level `covered`/`coverable` fields.

NOTE ON COMPARABILITY: the denominator has risen from 14415 to 15646 to
16507 across the recorded history, because the background-terminal subsystem
(`crates/xaft-tools/src/terminals/`, +1588 coverable) and the client-neutral
session service (`crates/xaft-session/src/service.rs`, +525 coverable) were
committed after the earlier readings were taken. Those are different trees,
so 85.96%/14189 is NOT a like-for-like improvement on 83.20%/13734,
79.54%/12445, 77.29%/11141 or the 14415-denominator reading 76.89%/11083
(measured during verification of G11b at `c88a3cf`, quoted there with a
"+1170 lines" gap) — the prior figures are retained only as history, and no
delta should be computed across them. Compare only readings that share a
denominator.

A **stale report** is the other way this table goes wrong, and it is the
failure the denominator history above actually produced.
`tarpaulin-report.json` is git-ignored, so a copy written against an earlier
tree survives a checkout and is read as though it described this one. Check the
report's own top-level `coverable` before trusting it: this page's reading is
the 16507-line tree, and a report with a smaller denominator predates the
commits that added `crates/xaft-tools/src/terminals/` (+1588) and
`crates/xaft-session/src/service.rs` (+525). The 14415-line denominator the
77.29% reading quotes is the sharper case. That tree carried
`crates/xaft-session/src/service.rs` as **untracked** working-tree content,
wired in by an uncommitted `pub mod service;` at
`crates/xaft-session/src/lib.rs:59`, and neither the file nor that line was
committed at the HEAD the reading was attributed to — so it was a working-tree
reading in the strict sense, the distinction drawn under "Baseline
measurement" above, and no clean checkout could reproduce it. That hazard is
historical now: `crates/xaft-session/src/service.rs` is committed (`08353db`),
tracked and wired, so the current 16507-line denominator is the committed
tree's. For any report found on disk, re-measure rather than re-reading it.

That last reading is also the one goal `b6b64dee` (G11b-3) was created to
write into this page, and it is **superseded, not pending**: it was taken
before the background-terminal subsystem and the client-neutral session
service existed, and its "+1170" gap was computed against 14415 coverable
lines, a denominator this tree no longer has. Do not re-baseline this page
onto it. The header above is the page's only status statement, and it is the
16507-denominator reading.

That is not the only stale coverage premise this page has had to retire. Goal
`9b0b8502` (G11b-4) was created to "continue closing the coverage gap" from the
77.29% (11141/14415) reading and quotes a **"+1112 covered lines remaining"**
gap; that premise is **void**. The lines it budgeted were closed by the batches
that landed after it (`3506e04`, `5643836`, `28a9083`, `e7b690d`), and the
enforced gate now passes: re-measured on this tree at HEAD `e7b690d`,
`cargo tarpaulin --workspace --engine llvm --fail-under 85` exits **0** at
85.90% (14180/16507) — the same resample this page already records above. So
G11b-4's ranked batch list is not outstanding work, and its "+1112" is computed
against a denominator (14415) this tree no longer has. Do not re-open it as a
test-batch goal; the remaining lever is the run-to-run margin, not a line count.

Another stale premise is retired here rather than acted on. Goals `9a07578f`
(G11b-2) and `9b0b8502` (G11b-4) both carry a now-false hygiene rule that a
batch must be measured a second time with the enforced gate dropped, on the
premise that the gate suppresses the report. It never did — one `just coverage`
run writes `tarpaulin-report.json` whether the gate passes or fails; see "The
report is written even when the gate fails" above.

Two further clauses are recorded as **void** rather than satisfied, because the
arithmetic they were written against describes a tree this one is not. Goal
`901d0665`'s TEST clause required this page's remaining-gap line to be computed
as 85% of the retired 13746-line denominator minus the documented covered
count; goal `5751e5d1`'s TEST clause asked the same of a 14415-line equivalent.
Neither can be made true by editing this page. Those denominators are three
tree states old, and, more decisively, the gate **passes** on the tree this
page records — 14189 covered against a requirement of 14031 — so there is no
remaining-gap line left for a subtraction to describe. They are retired here as
unsatisfiable rather than left to read as outstanding work, and the retired
numerators are described rather than quoted for exactly that reason: quoting
them would restate the arithmetic this paragraph exists to withdraw.

A third pending goal is retired for the *opposite* reason to the two above.
Goal `d5665b2510ac494db81680f2f5d39d91` asks to re-baseline this page onto "the
gate run measured at HEAD `3506e04` — 84.35% (13923/16507)", on the premise
that "108 covered lines remain". `3506e04` is **older** than `28a9083`:
`git merge-base --is-ancestor 3506e04 28a9083` succeeds and the reverse does
not. Working that goal as written would therefore move this page *backwards* —
from the passing 85.96% (14189/16507) recorded above to a failing 84.35%
(13923/16507) — and would re-assert a remainder the batches after it closed. Its
one durable finding, that a failing gate still writes `tarpaulin-report.json`,
is recorded under "The report is written even when the gate fails" above and is
independently reconfirmed, so nothing is lost by retiring the rest. Do not work
it as a re-baseline.

This page is also where the repository retires pending-goal premises that no
longer describe the tree, whatever their subject, and four goal-list entries
need that treatment for a non-coverage reason: the background-terminal
subsystem's formatting and commit state. Measured on the committed tree at
HEAD `b5f16c4`, with the sandbox **off** (manifests pristine):

* `cargo fmt -p xaft-tools --check` exits **0**, and `just fmt-check` exits
  **0** while expanding to all eleven members. The two formatting debts those
  entries name are both gone: goal `97f98d53`'s single defect
  (`crates/xaft-tools/tests/tool_parity_reachability.rs:178`, reformatted by
  `99ed876`) and goal `7330295c`'s nineteen hunks across `src/lib.rs`,
  `src/registry.rs`, `src/shell/run_command.rs`,
  `src/terminals/{manager,mod,tools}.rs` and `tests/terminal_tools.rs`.
* `git status --porcelain crates/xaft-tools/` is **empty**, so the committed
  tree equals the working tree and both entries' staging lists would produce an
  EMPTY COMMIT. That is the trap goal `688e95b1` predicted for the wiring's own
  commit; it applies equally to `97f98d53` and `7330295c`.
* The subsystem is tracked and wired, which supersedes goal `688e95b1`'s
  remaining premises: all five files under `crates/xaft-tools/src/terminals/`
  are reachable from `git log --all`, `pub mod terminals;` is at
  `crates/xaft-tools/src/lib.rs:45`, `libc` and `regex` are committed in
  `crates/xaft-tools/Cargo.toml`, and `cargo fmt -p xaft-tools --check`
  reports zero hunks rather than the six that goal's TEST clause still requires
  of the tree.

So four entries — `4ce1cc94a1d4471da3e6fe4d41c57e31`, which writes this record,
`97f98d53485b4862bc281cf487398cb7`, `688e95b1576c404e942073d8bd9b7071` and
`7330295c16e9412a8f8b8c750855b68a` — are retired here rather than worked, and
goal `c87e0ed92d0f4206870deddac08d9daf`, which asks for exactly this record, is
answered by it. Do not make an empty commit for any of them, and do not re-run
`cargo fmt` looking for hunks that no longer exist. The subsystem's other
retired premises — the untracked/missing directory and the `cargo test` hang —
are recorded under "The suite is green — the blocker is narrower than it
looked" below, which is the same correction reached from the test side.

The parity scripts are unaffected: `parity-matrix-stats.py --check`,
`config-key-audit.py --check` and `cli-row-ownership.py --check` all exit 0 on
this tree, and no parity row's status changes. `parity-row-reachability.py
--check` still exits 1 with the 51 pre-existing unreachable rows it reports on
any tree — the defect owned by goal `032b00d5f6b5473bb0a7758eaa81d5d1` and
already recorded in `docs/ci-status.md` — which this record neither causes nor
can fix.

`xaft` (bin) is four files at the repository root — `src/main.rs`, 
`src/lifecycle.rs` 0/8, `src/query.rs` 0/11, and an empty `src/lib.rs`; the
crate totals 40/73. An earlier revision of this table counted only
`src/main.rs` and so showed 38/52, omitting 19 coverable lines. The 54.79%
above is the honest figure, and it makes `xaft` the *lowest*-covered member
of the workspace — which it is, not the ~73% a reader would infer from
`main.rs` alone.

The 85% gate is **met**: 14189 covered lines against a requirement of 14031,
a headroom of **158** lines. Before the G11b-2 closing batch the same tree read
14000 (84.81%), i.e. 31 lines short; the batch added
`crates/xaft-tools/tests/git_tool_schema_contracts.rs` (12 tests), which took
the git-tool schema/description/confirmation bodies and error paths from
untested to covered. The headroom is thin relative to the 131-166-line
run-to-run spread recorded above, so the gate is met but not comfortably: a
future reading on the same tree could land just under 14031. Treat the margin
as a floor to keep widening, not as settled.

### Lowest-covered areas, which seed the test batches

By directory:

| Directory | covered/coverable | % | missing |
| --- | --- | --- | --- |
| `src` (root binary crate) | 40/73 | 54.8% | 33 |
| `crates/xaft-agent/src` | 227/280 | 81.1% | 53 |
| `crates/xaft-runtime/src` | 877/1064 | 82.4% | 187 |
| `crates/xaft-memory/src` | 234/283 | 82.7% | 49 |
| `crates/xaft-tools/src` | 3044/3611 | 84.3% | 567 |
| `crates/xaft-cli/src` | 1538/1813 | 84.8% | 275 |
| `crates/xaft-tui/src` | 5794/6756 | 85.8% | 962 |
| `crates/xaft-agents/src` | 227/256 | 88.7% | 29 |
| `crates/xaft-skills/src` | 134/151 | 88.7% | 17 |
| `crates/xaft-config/src` | 1448/1556 | 93.1% | 108 |
| `crates/xaft-session/src` | 626/664 | 94.3% | 38 |

Worst individual files by missing lines (reproduced by
`scripts/coverage-crate-summary.py tarpaulin-report.json`, except that the
script currently drops the root binary crate's four files — see the note
under the per-crate table):

| missing | covered/coverable | file |
| --- | --- | --- |
| 268 | 927/1195 | `xaft-tui/src/state.rs` |
| 130 | 117/247 | `xaft-tui/src/app.rs` (47.4%) |
| 90 | 481/571 | `xaft-tui/src/renderer.rs` |
| 81 | 508/589 | `xaft-tools/src/terminals/manager.rs` |
| 57 | 70/127 | `xaft-runtime/src/runtime.rs` |
| 49 | 383/432 | `xaft-tui/src/render/markdown.rs` |
| 46 | 557/603 | `xaft-tui/src/menu/config_menu.rs` |
| 46 | 142/188 | `xaft-cli/src/commands/config.rs` |
| 42 | 81/123 | `xaft-runtime/src/provider.rs` |
| 37 | 196/233 | `xaft-tools/src/shell/run_python.rs` |
| 37 | 49/86 | `xaft-cli/src/commands/sessions.rs` |
| 30 | 135/165 | `xaft-tui/src/transcript.rs` |
| 30 | 107/137 | `xaft-config/src/loader.rs` |
| 28 | 493/521 | `xaft-session/src/service.rs` |

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
| G11b-2 batch (`crates/xaft-tui/src/app.rs` tests; `xaft-cli` session-service handler tests) | `app.rs` 2/277 → 12/247; `commands/session_service.rs` 22/86 covered by 18 handler tests; the committed compile breakage in that file is fixed in the tree |
| G11b-2 closing batch (`crates/xaft-tools/tests/git_tool_schema_contracts.rs`, 12 tests) | `xaft-tools/src/git` **375/691 → 564/691**; 15 of the 19 git tools had no test that ever called `schema()`, `description()` or `requires_confirmation()`, so their whole published interface to the model was unverified. `git/mod.rs` 0/16 → 16/16; `branch.rs` 7/25 → 18/25; `create_branch.rs` 16/41 → 35/41; `stash_pop.rs` 10/25 → 19/25 |
| **current (tree at HEAD `28a9083`)** | **85.96% (14189/16507)** — **meets** the 85% gate, +158 over the 14031 requirement |

Writing the `jobs` tests also found three rows marked ✅ that were actually
unreachable; see §1b of [docs/parity-matrix.md](parity-matrix.md).

### The suite is green — the blocker is narrower than it looked

Re-measured on the **literal working tree** (nothing moved aside, nothing
restored) at HEAD `5643836` plus the G11b-2 closing batch — i.e. the tree now
committed as `28a9083` — sandbox engaged:

```text
cargo metadata --no-deps                            exit 0, 11 members
cargo test  --workspace                             2954 passed / 0 failed / 9 ignored
```

So `cargo test --workspace` **is** green over the working tree as it stands.
It was previously reported as blocked on agtrs; that was wrong. The agtrs
blocker is real for the committed tree but is fully bypassed by
`scripts/sandbox-build.sh`, which clones agtrs, applies the already-proven port
to the CLONE (the real repo is only ever read), and repoints the manifests.

The earlier claim that the untracked G9 `terminals/` directory hangs
`cargo test` is **no longer true**: nothing has to be moved aside to measure
the suite, and no test hangs. Two things changed since that claim was written,
so the correction is recorded here rather than left implicit:

* The directory is no longer untracked or missing. `crates/xaft-tools/src/terminals/`
  is committed (recovered in `b790a11`, wired in `bc616ac`/`99ed876`) and its
  three inline `mod tests` blocks — not a `tests/terminal_security.rs`, which
  was named in an earlier draft of `mod.rs` and never existed — run as part of
  the ordinary suite. `cargo test -p xaft-tools` is green with no hang.
* The four tools are written and registered, so goal 48 no longer owns them;
  goal `df972384d` closed them and section-6 rows 36/43/44/45 are ✅. What
  remains is the TUI half (slash rows 20/22, keybinding 11), which is goal
  `010f6df6`. None of it blocks measurement.

`scripts/doc-test-name-check.py --check` asserts that every test name cited in
the `terminals/` doc headers resolves to a real `fn`, so the "is this property
pinned?" question a finishing pass actually asks cannot be answered by a name
that was never written.

One operational caveat remains, and it is environmental rather than
structural: on this host `/tmp` is a full tmpfs, so `TMPDIR` must be
redirected (see the note above) or the doctest link step fails with
`No space left on device`. `just test` and `just coverage` now do this
automatically via the justfile's `TMPDIR` export plus a `require-tmp-space`
preflight, so the redirection only needs spelling out for bare `cargo`.

### What the next batches should target

Ranked by uncovered lines, from the same report as the table above:

| Missing | Covered | File |
| --- | --- | --- |
| 268 | 927/1195 | `xaft-tui/src/state.rs` |
| 130 | 117/247 | `xaft-tui/src/app.rs` |
| 90 | 481/571 | `xaft-tui/src/renderer.rs` |
| 81 | 508/589 | `xaft-tools/src/terminals/manager.rs` |
| 57 | 70/127 | `xaft-runtime/src/runtime.rs` |
| 49 | 383/432 | `xaft-tui/src/render/markdown.rs` |
| 46 | 557/603 | `xaft-tui/src/menu/config_menu.rs` |
| 46 | 142/188 | `xaft-cli/src/commands/config.rs` |

`xaft-tui` alone holds 962 of the 2318 uncovered lines, so the target is
overwhelmingly the TUI. `app.rs` is the least covered of them (117/247), and
`renderer.rs` needs the incremental-renderer cursor state driven through a
sequence of mutations. Both are worth a deliberate harness rather than
scattered unit tests.

`app.rs` was previously budgeted as a single blocked block that "needs a real
terminal". That has now been measured, and it is only true of 16 of its 275
uncovered lines — see the section below.

`state.rs` remains the largest single file, but it is now well covered
relative to its size (72.3%); the remaining misses are the
`handle_event` / `handle_key` match arms, which are best reached through
the event stream above.

### `app.rs`: measured, and 219 of its 235 uncovered lines need no tty

**Superseded goal-list annotations.** The goal-list entries for goals `9a07578f`
(G11b-2) and `9b0b8502` (G11b-4) both read `app.rs 275 (BLOCKED on goal
d5d1607f's pty/test-seam decision)` and `BLOCKED on goal d5d1607f's tty
decision`. That annotation is superseded by this section: the correct scope is
the 16 tty-bound `run()` lines only, and the other 259 are reachable from the
inline `#[cfg(test)] mod tests` in `app.rs` (`use super::*;`) with no production
change.

Two earlier passes left `app.rs`'s reachability open. It is now measured. The
report breaks the file's uncovered lines into the regions below; the per-region
counts were taken **before** the G11b-2 batch, which drove `app.rs` from 2/277
to 12/247 and spread the remainder rather than clearing any single region, so
treat the counts as an upper bound:

| Region | Lines | Uncovered | Needs a tty? |
| --- | --- | --- | --- |
| `TuiApp::run` | 56-84 | 16 | **yes** — dies at `surface.init()?` |
| `TuiApp::run_inner` | 87-451 | 180 | no — takes an injected renderer |
| `apply_mutation` | 457-472 | 11 | no |
| `show_danger_confirmation_terminal` | 477-506 | 21 | no — reads stdin |
| `replay_history_lines` | 517-521 | 2 | no |
| `find_handoff_split` | 527-560 | 16 | no |
| `replay_history_lines_multi` | 568-627 | 29 | no |

Re-measured after the batch: `run()` is **still exactly 16 uncovered lines**
(56-58, 61, 63-65, 70-71, 74, 76-78, 81-82, 84), so its tty-boundary status is
unchanged, and the other 219 are scattered across `run_inner` and the helpers
rather than concentrated in one region.

The two claims that made the whole file look unreachable were measured
directly, with a temporary probe test reporting `is_terminal(stdin)`,
`enable_raw_mode()`, `terminal::size()` and `open("/dev/tty")`. The probe was
deleted in the same commit that recorded this result.

**`run()` is blocked, exactly as reported.** Without a tty the probe reports
`enable_raw_mode_ok=false` and `open_devtty=false` (`/dev/tty` fails ENXIO),
so `run()` returns `Err` at `surface.init()?` and `run_inner` is never
entered. `run_inner` is also a private `async fn`, so no integration test can
call it. So 16 lines are genuinely unreachable from an ordinary test.

**But `run_inner` itself does not need a tty.** It takes
`&mut IncrementalRenderer` as a parameter, and `IncrementalRenderer<W>` is
generic over `TermWriter` with a public `with_writer` constructor and a
public `TestCapture` writer — the same seam `renderer.rs`'s own tests already
use. Its remaining tty touchpoint, the `EventStream::new()` reader at line
237, is spawned into its own task, so its failure breaks only that task.

**A pty harness works, and is not required.** Under
`script -qec "cargo test ..."` the same probe reports
`is_terminal(stdin)=true enable_raw_mode_ok=true open_devtty=true`, so option
(a) is genuinely available. It is deliberately **not** adopted: it would make
coverage depend on an exotic harness and on how the runner spawns test
binaries, to reach 16 lines. The pty also reports `size=(0, 0)`, which is
itself a reason not to build geometry assertions on it.

**Decision: (b)-free.** No production change is made — `run_inner` stays
private and nothing is threaded through `TuiApp`. The 219 lines outside
`run()` are reachable from a `#[cfg(test)]` module inside `app.rs`, which can
already see the private `run_inner`, `apply_mutation`,
`find_handoff_split` and `show_danger_confirmation_terminal`. The 16 lines of
`run()` stay uncovered and are recorded here as an honest residual rather than
excluded from the denominator: the workspace target remains 85% of **all**
lines. No test was written that merely reaches `run()` and lets it error out.

**Recorded outcome for those 16 lines: accepted as a permanent residual**, since
they cannot be covered without the declined pty harness. Arithmetic consequence:
they are a permanent floor on the uncovered set, so the attainable maximum
workspace percentage is `(16507 - 16) / 16507 = 99.90%`, and the 85% gate — which
needs `14031` covered lines — is unaffected by them.

Consequence for planning: budget these 219 lines as ordinary testable code,
not as a blocked batch — reaching them needs no harness, no new dependency and
no production diff.

## Reproducing

```bash
just coverage                       # enforced; non-zero exit below 85%; report still written
just coverage-report                # same measurement, no gate; also writes the report
```

Either one leaves a `tarpaulin-report.json` for the per-line tooling:

```bash
python3 scripts/uncovered.py crates/xaft-tui/src/app.rs
```

Do not pay for a second measurement run to get one — see "The report is written
even when the gate fails" above.

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
  and is what has to exit 0. The threshold is not what fails that job on the
  current tree: its `Measure line coverage` step dies in `cargo metadata` like
  every other cargo job, because the runner has no workspace graph \u2014 see
  [CI status](ci-status.md).
- The disarm above is policed, not remembered. `scripts/coverage-gate-check.py`
  (run by `just coverage-gate-check`, and as a pre-flight step in the coverage
  workflow itself) asserts that the workflow's tarpaulin flags match
  `just coverage`, that the threshold is set in exactly one place, and that
  the gate's armed state is *justified by this page's measurement*. So the day
  the number recorded at the top of this file reaches 85%, the check fails and
  prints the one line to change — the armed-but-unreached gate and the
  reached-but-unarmed gate are both errors, and the failure is loud.
