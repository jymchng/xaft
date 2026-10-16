# xaft — Justfile
# Run `just` to see available recipes.
# Install just: cargo install just

set dotenv-load := true

export RUSTFLAGS := "-Awarnings"

# Target directory — override to avoid root-owned target/
export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", "/root/root-local")

# Temp directory for rustc's scratch space.
#
# rustc compiles every DOCTEST binary under TMPDIR, and on this host /tmp is a
# 12G tmpfs routinely filled by other projects' scratch data. When it fills,
# the link step dies and cargo attributes the failure to the doctest it happened
# to be building:
#
#     test crates/xaft-cli/src/jobs/mod.rs - jobs::JobStore (line 357) ... FAILED
#     error: linking with 'cc' failed: exit status: 1
#     LLVM ERROR: IO failure on output stream: No space left on device
#
# which reads exactly like a regression in working code. MEASURED: the same
# doctest on the same tree passed with 586 MiB free on /tmp and failed to link
# with a few hundred MB less. See scripts/check-tmp-space.sh for the diagnosis.
#
# So point TMPDIR at a directory on the large filesystem. This is an EXPORT, so
# it applies to every recipe: cargo, rustc and rustdoc all read it, and so do
# the test binaries via `tempfile`. An explicit TMPDIR in the environment always
# wins; this only supplies a default and never overrides a deliberate choice.
export TMPDIR := env_var_or_default("TMPDIR", "/var/tmp/xaft-tmp")

# `-p xaft -p xaft-cli ...` for this workspace's members, read from cargo.
# Evaluated once per `just` invocation. Scoped rather than `--all` on purpose:
# cargo-fmt also walks path dependencies, which under the agtrs build sandbox
# means the throwaway clone. See the `fmt` recipe and scripts/xaft-members.py.
members := `python3 scripts/xaft-members.py`

# Default: show all recipes
default:
    @just --list

# ── The agtrs build sandbox ───────────────────────────────────────────────
#
# xaft's crates path-depend on the sibling repo ../agtrs, which is a separate
# workspace that has NOT been ported to the injectable crate rename. Cargo
# therefore aborts inside `cargo metadata` before compiling anything:
#
#     error: failed to get `injectable-runtime` as a dependency of package
#     `agtrs-runtime v0.1.0 (/root/rust_projects/agtrs/agtrs-runtime)`
#
# `scripts/sandbox-build.sh` is the SANCTIONED way to work around this. It
# clones agtrs outside this repo, applies the already-proven
# injectable-rename port to the CLONE, and repoints xaft's 11 manifests at
# it. The real ../agtrs is only ever READ — never edited, stashed or
# committed. The clone lives outside the tree deliberately: cargo force-adds
# an in-tree path dep to the workspace member list, and `exclude` cannot
# stop it, so a sandbox inside the repo would pull agtrs' own (failing)
# tests into `cargo test --workspace` and report them as ours.
#
# THE LOOP:
#
#     just sandbox        # engage: clone, port, repoint, cargo check
#     just test           # ... or build / coverage / ci / run / lint
#     just sandbox-off    # MANDATORY before `git add` — see the warning below
#
# *** While the sandbox is engaged, 11 TRACKED Cargo.toml files carry      ***
# *** rewritten agtrs paths. `--off` restores them. Running `git add`      ***
# *** without tearing down first COMMITS those local paths.               ***
#
# Manifests you edit while engaged are PRESERVED on teardown (it rebases
# your edit rather than discarding it) and anything ambiguous is reported
# loudly instead of quietly reverted.
#
# Full rationale: docs/contributing.md § Building and testing.

# Engage the build sandbox: clone + port agtrs, repoint manifests, cargo check.
sandbox:
    ./scripts/sandbox-build.sh

# Engage without the initial `cargo check` (faster when you know it builds).
sandbox-no-check:
    ./scripts/sandbox-build.sh --no-check

# Report whether the sandbox is engaged. Changes nothing.
sandbox-status:
    ./scripts/sandbox-build.sh --status

# Restore every manifest and delete the clone. RUN THIS BEFORE `git add`.
sandbox-off:
    ./scripts/sandbox-build.sh --off

# Gate for every recipe that resolves the workspace graph. Exits 0 when the
# graph builds (sandbox engaged, or agtrs permanently ported) and prints the
# engage/teardown instructions when it does not, instead of letting cargo
# fail with a manifest error naming a crate in an unfamiliar repo.
#
# Deliberately NOT a dependency of `fmt` / `fmt-check` / `members`: those
# read the member list from `cargo metadata --no-deps`, which succeeds even
# when the graph is broken, so they work with the sandbox off and gating
# them would only add a failure mode.
require-sandbox:
    #!/usr/bin/env bash
    ./scripts/require-sandbox.sh

# ── Scratch space ─────────────────────────────────────────────────────────────
# Report free space on the temp directory rustc links doctest binaries in, and
# fail with a diagnosis instead of letting an exhausted filesystem masquerade as
# a failing doctest. Changes nothing; safe to run at any time.
tmp-check:
    ./scripts/check-tmp-space.sh

# Same, but exercise the decision logic rather than the real filesystem.
tmp-check-self-test:
    ./scripts/check-tmp-space.sh --self-test

# Gate for every recipe that links a test binary. Its companion preflight.
require-tmp-space:
    #!/usr/bin/env bash
    ./scripts/check-tmp-space.sh --quiet

# ── Build ─────────────────────────────────────────────────────────────

# Build the workspace (debug)
build: require-sandbox
    cargo build --workspace

# Build the xaft binary in release mode
build-release: require-sandbox
    cargo build --workspace --release

# Check compilation without artifacts (faster than build)
check: require-sandbox
    cargo check --workspace

# ── Format & Lint ─────────────────────────────────────────────────────

# Format all source files.
#
# Scoped to this workspace's own members rather than `cargo fmt --all`. cargo-fmt
# walks PATH DEPENDENCIES too, so `--all` would additionally format the throwaway
# agtrs clone when scripts/sandbox-build.sh is engaged -- and `fmt-check` would
# then go red because a repo we do not control is not rustfmt-clean under the
# installed rustfmt. It would also make the gate check 21 crates locally but 11 in
# CI. The member list comes from cargo, never hardcoded, so a new crate cannot
# silently escape formatting. See scripts/xaft-members.py.
fmt:
    cargo fmt {{ members }} --

# Check formatting without modifying (CI-safe)
fmt-check:
    cargo fmt {{ members }} -- --check

# Print the member packages the fmt recipes use — useful when debugging a fmt
# failure, or to confirm a new crate was picked up.
members:
    @./scripts/xaft-members.py --check

# Run clippy on the whole workspace
lint: require-sandbox
    cargo clippy --workspace -- -D warnings

# NOTE ON THE `require-tmp-space` DEPENDENCY
# Not ceremony. A doctest that fails to LINK because TMPDIR filled up is
# reported by cargo as:
#
#     test crates/xaft-cli/src/jobs/mod.rs - jobs::JobStore (line 357) ... FAILED
#     error: linking with 'cc' failed: exit status: 1
#     LLVM ERROR: IO failure on output stream: No space left on device
#
# which reads exactly like a regression and invites someone to edit working code
# or mark the doctest `ignore`d. Neither is ever the fix. The preflight only
# catches the outright-empty case; the sneakier one is a temp directory with
# just enough room to start and not enough to finish. That is why the top of
# this file exports TMPDIR onto a large filesystem rather than relying on the
# preflight alone. Measure with `just tmp-check`.

# ── Test ──────────────────────────────────────────────────────────────

# Run all tests
test: require-sandbox require-tmp-space
    cargo test --workspace

# Run lib (unit) tests only
test-unit: require-sandbox require-tmp-space
    cargo test --workspace --lib

# Run integration tests only
test-integration: require-sandbox require-tmp-space
    cargo test --workspace --test '*'

# Run a single test by substring match — e.g. just test-one dry_run
test-one name: require-sandbox require-tmp-space
    cargo test --workspace {{ name }}

# Run tests with stdout visible
test-verbose: require-sandbox require-tmp-space
    cargo test --workspace -- --nocapture

# Run tests for a single crate — e.g. just test-crate xaft-runtime
test-crate crate: require-sandbox require-tmp-space
    cargo test -p {{ crate }}

# ── Full CI gate ──────────────────────────────────────────────────────

# Format check + lint + test (mirrors CI)
# Note: CI runs raw `cargo`, not these recipes, so a CI runner has no
# sandbox. That is why the gate below prints instructions rather than trying
# to auto-engage a clone it cannot see.
ci: fmt-check lint test

# ── Coverage ──────────────────────────────────────────────────────────

# Generate coverage report with tarpaulin, enforcing the 85% line-coverage gate.
# The gate is the same one CI and the Definition of Done assert, so `just coverage`
# must fail the command when the workspace drops below the target.
#
# `--out Json` is what makes `scripts/uncovered.py` and
# `scripts/coverage-crate-summary.py` usable: without it tarpaulin writes NO
# report file at all, because the default output backend is stdout only.
#
# MEASURED (cargo-tarpaulin 0.35.4), because this was long assumed to work the
# other way round -- i.e. that a failing gate suppresses the JSON write and
# forces a second ~12-minute run per batch:
#
#     --fail-under 100                 -> exit 1, NO tarpaulin-report.json
#     --fail-under 100 --out Json      -> exit 1, tarpaulin-report.json present
#
# So the gate is NOT what suppresses the report; the missing `--out Json` is.
# tarpaulin writes the report before it evaluates the threshold
# (report_coverage_with_check -> report_coverage, then check_fail_threshold in
# cargo-tarpaulin's lib.rs). This recipe therefore produces a report whether it
# passes or fails. The human-readable stdout summary is unaffected.
#
# Gated: tarpaulin resolves the whole graph, so with the sandbox off this is
# the same `cargo metadata` abort -- only buried under a long instrumentation
# build first.
#
# Enforce the 85% line-coverage gate; write tarpaulin-report.json either way.
coverage: require-sandbox require-tmp-space
    cargo tarpaulin \
        --workspace \
        --engine llvm \
        --timeout 300 \
        --skip-clean \
        --out Json \
        --fail-under 85

# Same measurement, WITHOUT the gate. This is the one to run between test
# batches: while the workspace is below 85% `just coverage` always exits
# non-zero, which is correct for a gate but noisy in the measure/edit/re-measure
# loop. It is the non-enforcing twin of `coverage`, not a replacement for it --
# `coverage` remains the enforcing entry point and the Definition of Done.
#
# Both recipes write tarpaulin-report.json, so neither one costs a second
# run to inspect which lines a batch covered:
#
#     just coverage-report
#     python3 scripts/uncovered.py crates/xaft-tui/src/app.rs
#
# That report is gitignored, so a leftover copy from an earlier run can
# silently contradict the current tree. Delete it before measuring if you are
# unsure -- the mtime is the only thing distinguishing the two.
#
# Measure coverage and write tarpaulin-report.json, WITHOUT the 85% gate.
coverage-report: require-sandbox require-tmp-space
    cargo tarpaulin \
        --workspace \
        --engine llvm \
        --timeout 300 \
        --skip-clean \
        --out Json

# Check that CI's coverage gate and `just coverage` mean the same thing.
#
# The gate lives in three places — the recipe above, the CI workflow's
# env.COVERAGE_MIN, and that recipe as quoted in docs/coverage-baseline.md —
# and nothing re-derives them from each other. A threshold hardcoded twice, or
# a CI job left reporting-only after the coverage work closes the gap, is
# invisible until the Definition of Done quietly stops meaning anything.
#
# This is seconds of work and reads no build artifacts, so it needs neither the
# sandbox nor free temp space. It is also the tripwire that forces the gate to
# be armed: when docs/coverage-baseline.md records coverage at or above the
# target, this fails and names the one line to change. CI runs it too, as a
# pre-flight step in .github/workflows/coverage.yml.
#
# --self-test runs first because a checker that cannot fail is worse than no
# checker: it asserts that every drift mode this script claims to police is
# actually detected.
coverage-gate-check:
    ./scripts/coverage-gate-check.py --self-test
    ./scripts/coverage-gate-check.py --check

# ── Parity matrix ─────────────────────────────────────────────────────────
# Recompute docs/parity-matrix.md's subtotals and Summary table from its own
# rows. They used to be maintained by hand, and drifted: commit ba4aa21
# flipped 14 `jobs` rows to ✅ without updating them, so the document claimed
# 62 ✅ while its rows summed to 76. Run this after flipping any row's status.
parity-matrix:
    ./scripts/parity-matrix-stats.py

# Fail if the matrix's hand-written aggregates have drifted from its rows.
# Suitable for CI: exits 1 on any disagreement.
#
# The --self-test runs first because a checker that cannot fail is worse than
# no checker: it asserts that each kind of drift this script claims to police
# (a row's status, a section subtotal, a Summary row, the Summary TOTAL, the
# prose count and percentage, a class header) is actually detected. That is not
# theoretical — the per-section Summary check used to be dead code that never
# ran while the docstring claimed it did.
parity-matrix-check:
    ./scripts/parity-matrix-stats.py --self-test
    ./scripts/parity-matrix-stats.py --check

# ── CI status ────────────────────────────────────────────────────────────────
# Print what .github/workflows/ can actually do on a fresh runner.
#
# xaft path-depends on the sibling `agtrs` repo, which has not been ported to
# the injectable crate rename, so every workflow job that runs a raw cargo
# command dies inside `cargo metadata` before a line is compiled, and no
# workflow can reach scripts/sandbox-build.sh (its clone lives outside the repo
# tree on purpose, so a runner cannot see it). That is a recorded decision --
# docs/ci-status.md -- not a flaky-test or a code regression, and this recipe
# re-derives it from the workflow files rather than from prose.
ci-status:
    ./scripts/ci-graph-status.py

# Fail if the CI record and .github/workflows/ disagree: a cargo job missing
# from the inventory block, an inventory entry naming a file that no longer
# exists, a workflow that starts providing the workspace graph while the page
# still says none does, or -- the interesting one -- the graph starting to
# resolve, at which point CI is no longer red for the recorded reason and the
# page has to be rewritten rather than left in the tree.
#
# The --self-test runs first because a checker that cannot fail is worse than no
# checker: it asserts that each drift mode above is actually detected, and that
# the two negative controls stay quiet (a job that runs no cargo is not drift;
# a skipped graph probe is not a stale record).
#
# Needs no sandbox: with the sandbox engaged the graph probe is skipped with a
# printed note, because a local clone is a reason a CI runner cannot use.
ci-status-check:
    ./scripts/ci-graph-status.py --self-test
    ./scripts/ci-graph-status.py --check

# Fail if a Rust doc header cites a test name that does not exist.
#
# A module doc saying "covered by the tests in tests/foo.rs", or naming a test,
# is a claim about the tests. That claim can rot silently: the background
# terminal subsystem's mod.rs asserted its four safety invariants were covered
# by a `tests/terminal_security.rs` that was never written (they are inline
# `mod tests` blocks), and terminals/tools.rs cited a tool-level test name that
# existed nowhere in the tree. A reader asking "is this property pinned?" gets
# "yes" from a name that resolves to nothing.
#
# Scope is the whole workspace — every crate under crates/ plus the root
# binary. It used to be only the terminal subsystem, because widening it put
# five legitimate citations and one real defect on the gate at once. The script
# now recognises those classes structurally (a clap-attribute / macro-attribute
# name, a struct field or emitted string literal, an explicitly-marked foreign
# `agenthicc` API or removed path, and a prose fragment), and the one real
# defect — `auth_ignores_the_scope_flags`, a truncated citation in
# crates/xaft-cli/tests/mcp_handlers.rs — was fixed at the citation itself.
# Do not widen the allowance by growing an allow-list in the script.
doc-test-names:
    ./scripts/doc-test-name-check.py

doc-test-names-check:
    ./scripts/doc-test-name-check.py --check

# ── Documentation ─────────────────────────────────────────────────────

# Build and open rustdoc
doc: require-sandbox
    cargo doc --workspace --no-deps --open

# Build docs without opening (CI)
doc-build: require-sandbox
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps

# ── Run ───────────────────────────────────────────────────────────────

# Run xaft with a task — e.g. just run "fix the type error in src/"
run task: require-sandbox
    cargo run --bin xaft -- run "{{ task }}"

# Dry-run a task (no file changes)
dry-run task: require-sandbox
    cargo run --bin xaft -- run --dry-run "{{ task }}"

# Show xaft version
version: require-sandbox
    cargo run --bin xaft -- version

# ── Examples ──────────────────────────────────────────────────────────

# Run xaft on the bundled python-password-generator example
example-fix:
    cd xaft-examples/python-password-generator && \
        cargo run --bin xaft --manifest-path ../../Cargo.toml -- \
        run "Replace all uses of random.choice with secrets.choice for cryptographic security"

# ── Version Management ────────────────────────────────────────────────

# Show current workspace version
version-show:
    #!/usr/bin/env bash
    python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['package']['version'])"

# Bump version: just bump patch | minor | major
bump part:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 - "{{ part }}" <<'PY'
    import pathlib, re, subprocess, sys, tomllib

    cargo = pathlib.Path("Cargo.toml")
    data = tomllib.loads(cargo.read_text())
    cur = data["workspace"]["package"]["version"]
    part = sys.argv[1]
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", cur)
    if not m:
        sys.exit(f"unsupported version format: {cur}")
    maj, minor, patch = map(int, m.groups())
    match part:
        case "major": new = f"{maj+1}.0.0"
        case "minor": new = f"{maj}.{minor+1}.0"
        case "patch": new = f"{maj}.{minor}.{patch+1}"
        case _: sys.exit("Usage: just bump [major|minor|patch]")

    text = cargo.read_text()
    text = re.sub(r'(?<=\[workspace\.package\]\n)(?:.*\n)*?version\s*=\s*"[^"]*"',
                  lambda m: m.group().replace(cur, new), text, count=1)
    # simpler: replace first occurrence in [workspace.package]
    text = text.replace(f'version = "{cur}"', f'version = "{new}"', 1)
    cargo.write_text(text)
    print(f"Bumped {cur} → {new}")
    PY

# ── Workspace Utilities ───────────────────────────────────────────────

# Show dependency tree
tree: require-sandbox
    cargo tree -p xaft

# Clean build artifacts
clean:
    cargo clean

# Rebuild from scratch
rebuild: clean build

# Check required tools are installed
doctor:
    #!/usr/bin/env bash
    ok=0
    check() { command -v "$1" >/dev/null 2>&1 && echo "  ✓ $1" || { echo "  ✗ $1  ($2)"; ok=1; }; }
    echo "Checking required tools..."
    check cargo   "install rustup"
    check rustfmt "rustup component add rustfmt"
    check just    "cargo install just"
    check python3 "install python 3.11+"
    [[ $ok -eq 0 ]] && echo "All tools present." || echo "Some tools missing — see above."
    exit $ok

# Show outdated dependencies
outdated: require-sandbox
    cargo outdated --workspace

# Audit for security vulnerabilities
audit: require-sandbox
    cargo audit

# Watch for changes and re-run tests (requires cargo-watch)
# Engage the sandbox first — `watch` re-resolves the graph on every rebuild.
watch: require-sandbox
    cargo watch -x "test --workspace"
