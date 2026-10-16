# CI status

**Status: CI is knowingly red — no job in this repository that builds or tests
xaft can run.** This is not a code defect, not a flaky test and not a
regression. It is the recorded consequence of leaving the sibling `agtrs`
repository unported, and it is checked mechanically so the claim cannot quietly
become false in either direction — see "How this record stays honest" below.

Measured at commit `726c56a`, on the committed tree, with the build sandbox
**off** (manifests pristine).

## The decision

The decision is **declined-with-reason**: no workflow is given a step that
provides the workspace graph, and xaft's CI stays red until `agtrs` is ported
for real.

It was taken by goal `252addb73cd943f7b69887b3222fdcd1`, whose scope was to ask
the operator which of four paths to take. The operator was asked and did not
answer before the deadline. Three of the four paths add a CI step or a
structural dependency the repository does not have today — i.e. they need
exactly the authorization that was not given — and the one that looked
cheapest cannot be written honestly either: the workflow would have to clone
`agtrs` (and `injectable`) into the runner, and the runner's access to those
repositories is *unverified* (`https://github.com/jymchng/agtrs` returns 404
unauthenticated; both remotes are configured as SSH). A workflow that clones a
repository it cannot reach converts a manifest error into an authentication
error, which is a strictly worse gate. So nothing was added, and this page
replaces the guess with the measurement.

## Every job, measured

```ci-status
# Every workflow below runs a cargo command that needs the FULL workspace graph,
# so it fails inside `cargo metadata` before a line is compiled.
# The jobs named are the ones that cannot pass; jobs in the same files that do
# not touch cargo are unaffected and are listed in the table below.
tests.yml: test, test-msrv
lint.yml: fmt, clippy
coverage.yml: coverage
codeql.yml: analyze
release.yml: build
```

| Workflow | Job | The command that needs the graph | On a fresh runner |
|---|---|---|---|
| `tests.yml` | `test` | `cargo test --workspace` | red — `cargo metadata` fails |
| `tests.yml` | `test-msrv` | `cargo check --workspace` on Rust 1.86 | red |
| `lint.yml` | `fmt` | `cargo fmt --all -- --check` | red — `--all` asks for the full metadata |
| `lint.yml` | `clippy` | `cargo clippy --workspace -- -D warnings` | red |
| `lint.yml` | `docs-check` | none — `node scripts/docs-site.cjs --check` | **green**; needs no cargo |
| `coverage.yml` | `coverage` | `cargo tarpaulin --workspace … --fail-under "${COVERAGE_MIN}"` | red **twice**: the pre-flight gate check fails on the gate's arming state, and `Measure line coverage` fails in `cargo metadata`; `COVERAGE_MIN` is `"0"`, so the threshold is not what fails |
| `codeql.yml` | `analyze` | `cargo build --workspace` | red |
| `release.yml` | `build` | `cargo test --workspace`, then `cargo build --release --workspace` | red — and `publish` needs `build`, so no release can be produced |
| `docs.yml` | `build`, `deploy` | none — `mkdocs build` | unaffected |
| `stale.yml` | `stale` | none — `actions/stale@v9` | unaffected |

Two greps that make the shape unambiguous, run over `.github/workflows/`:

* `grep -cE "sandbox|agtrs|injectable"` → `0` in all seven files. No workflow
  can reach `scripts/sandbox-build.sh`.
* `grep -cE "run:.*\bjust "` → `0` in all seven files. The `require-sandbox`
  preflight is a *justfile* preflight, so it never fires in CI: the runner gets
  the raw manifest error that the preflight exists to explain.

## What a runner actually does

Every one of those jobs checks out xaft alone and runs cargo in the repository
root, so the first thing cargo does is resolve the workspace graph:

```text
error: failed to get `injectable-runtime` as a dependency of package
  `agtrs-runtime v0.1.0 (/root/rust_projects/agtrs/agtrs-runtime)`
    ... which satisfies path dependency `agtrs-runtime` of package `xaft-runtime`
Caused by: failed to load source for dependency `injectable-runtime`
Caused by: failed to read `/root/rust_projects/injectable/injectable-runtime/Cargo.toml`
Caused by: No such file or directory (os error 2)
```

Reproduced here with `cargo metadata --format-version 1` → **exit 101** on the
pristine tree. Nothing downstream of that matters: no test is compiled, no
coverage is instrumented, and CodeQL never gets a build to analyse.

## Why the local sandbox does not carry into CI

`scripts/sandbox-build.sh` is the supported local workaround and it works: it
clones `agtrs` to a scratch directory **outside** the repo tree, applies the
already-proven `port-agtrs-injectable-rename.sh` port to the clone, flattens
the clone's nested workspace, and repoints xaft's 11 manifests at it. A CI
runner cannot reuse it as it stands:

* the clone lives outside the repo tree **on purpose** — cargo force-adds a path
  dependency inside a workspace's directory tree to that workspace's member
  list, so an in-tree clone would run agtrs' own test suite and report its
  results as xaft's. A runner cannot see a directory that is not in the
  checkout;
* the script hardcodes `/root/rust_projects/injectable` in two places (the
  `ln -sfn` that gives the clone its injectable sibling, and the `sed` that
  re-anchors the clone's own `path = "../../injectable/…"` dependencies to that
  absolute path). A runner has a different absolute layout;
* it clones `agtrs` from a **local directory** by default. The remote exists
  (`git@github.com:jymchng/agtrs.git`), which is what a CI version would have to
  use — with credentials.

## The three paths not taken, and what each needs

Recorded so the next decision is a decision and not a re-measurement.

**(a) Clone and port in the runner.** Everything but the plumbing already
exists: the proven port script, `scripts/flatten_agtrs_workspace.py`, the
manifest rewrite and its reversible teardown, and the check that the workspace
has exactly 11 members. What it needs: `sandbox-build.sh` refactored so the
injectable sibling path is injected rather than hardcoded; two pinned clones
(measured pins: `agtrs` `8a26310`, `injectable` `d824127`); and confirmed
runner access to both repositories — a token/deploy-key secret, since the
public HTTPS probe 404s.

**(b) Vendor a ported `agtrs` into the tree.** Must first deal with the nested
workspace: a path dependency that lives inside another workspace's directory
tree is mis-attributed by cargo, which is exactly why
`flatten_agtrs_workspace.py` exists (`error inheriting 'agtrs-runtime' from
workspace root manifest's workspace.dependencies`). It also changes workspace
membership, which is the thing the out-of-tree rule avoids. Structural, and it
creates a fork of a separate workspace.

**(c) Vendor a patched `agtrs` as a git submodule.** Same in-tree membership and
nesting questions, plus xaft's build would then depend on a submodule push
landing first — and the submodule would have to contain the *ported* paths,
which means `agtrs`' own `main` has to change first. That is goal
`7056fa3fcf0946fba62bceefd49f3e37`'s authorization, which has not been given
either.

Neither (a), (b) nor (c) may be started without that authorization.

## The one job that could be green today

`lint.yml`'s `fmt` job runs `cargo fmt --all -- --check`, which asks cargo for
the full metadata and therefore fails on the graph. The justfile's `fmt-check`
deliberately does not do that: it lists the members with `cargo metadata
--no-deps` (measured **exit 0** even with the graph broken) and then runs
`cargo fmt -p <member> … -- --check`. Measured on the pristine tree:

```text
cargo fmt --all -- --check   -> exit 1   (`cargo metadata` exited with an error)
just fmt-check               -> exit 0   (11 -p flags)
```

So the fmt job is graph-free capable today, and it has **not** been changed
here: the goal's scope forbids editing a workflow to add steps without the
operator's answer, and that answer did not come. It is recorded so that the
decision costs one line of YAML rather than a fresh measurement.

## Interaction with the coverage gate

`coverage.yml` carries `COVERAGE_MIN: "0"` and passes it to `--fail-under`, so
the coverage job is a *reporting* gate. Arming it is goal
`8f6205128c44471b848df707497d59f7`'s scope, and arming it while the job cannot
resolve the workspace graph would produce a red check for a reason that has
nothing to do with coverage — the threshold would look like a coverage
regression when the real failure is the manifest error above. Decide the CI
path first; `COVERAGE_MIN` stays `"0"` until then.

The preflight step of that job is Python-only, so the graph failure never
reaches it \u2014 but it is **not green either**, for an unrelated and pre-existing
reason. Measured on the same pristine tree:

```text
python3 scripts/coverage-gate-check.py --check  -> exit 1
  coverage is AT OR ABOVE the gate (85.96% >= 85%) but CI still runs
    `--fail-under 0`. Set `COVERAGE_MIN: "85"` \u2026
  CI is armed, but the comment above COVERAGE_MIN still says the value is 0
```

`docs/coverage-baseline.md` records 85.96%, at or above the 85 target, so the
checker that exists to stop the gate staying wired-but-unarmed fires. That is
arming paperwork, and arming is goal `8f6205128c44471b848df707497d59f7`'s
scope \u2014 it is not a fix this page's decision can make, because arming the gate
while the job cannot resolve the workspace graph would trade that failure for
the manifest failure above. So the coverage job has **two** independent red
steps today; only the second is this page's subject.

## How this record stays honest

`scripts/ci-graph-status.py` re-derives every fact on this page from
`.github/workflows/` and from a real `cargo metadata` probe, and fails when the
two disagree. Run it with `just ci-status-check`. It fails when:

1. a workflow runs a cargo command that needs the graph but is missing from the
   inventory block above, or the job names in the block no longer match the
   workflow's own `jobs:` keys (add a job to `tests.yml` and the record must be
   updated);
2. the inventory block names a workflow that does not exist;
3. any workflow gains a graph-providing step (`sandbox`, `agtrs` or
   `injectable`) while this page still says none has — i.e. if a later pass
   implements (a), (b) or (c), the record must be rewritten in the same change;
4. the workspace graph **resolves** on a pristine checkout. Then the reason this
   page exists is gone, the red jobs may be green, and leaving "CI is knowingly
   red" in the tree would be the stale-claim defect this repository keeps
   rediscovering — so the check fails and asks for the workflows to be
   re-evaluated;
5. this page loses the `declined-with-reason` decision it records.

The probe is **skipped**, with a printed note, while the sandbox is engaged:
then the graph resolves locally for a reason a CI runner cannot use, and a
check that could not tell those apart would fire on every sandboxed run.

## What this means for the goal that asked for it

The goal that produced this page carried a TEST clause of the form "on a clean
runner, the tests job exits 0". Under this decision that clause is **unmet**,
and it cannot be met without authorization that was not given; the honest
outcome is this record rather than a workflow that cannot work. The parity
checks the same goal requires were run at `726c56a` and are unchanged by this
page: `parity-matrix-stats.py --check`, `cli-row-ownership.py --check` and
`config-key-audit.py --check` exit 0. `parity-row-reachability.py --check`
exits 1 with 51 unreachable ✅ rows — a pre-existing defect, unrelated to CI and
owned by goal `032b00d5f6b5473bb0a7758eaa81d5d1`; no parity row's status
changed here.

See also: [contributing.md](contributing.md) for the local sandbox loop, and
[coverage-baseline.md](coverage-baseline.md) for the coverage measurement this
page's gate discussion refers to.
