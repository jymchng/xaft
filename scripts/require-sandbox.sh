#!/usr/bin/env bash
#
# require-sandbox.sh — fail with INSTRUCTIONS instead of a cargo manifest error.
#
# WHY THIS EXISTS
#   xaft's crates path-depend on ../agtrs, which is a separate workspace still
#   unported to the injectable crate rename. So a plain `cargo test --workspace`
#   does not fail with "a test failed" — it fails before any test is even
#   compiled, inside `cargo metadata`:
#
#       error: failed to get `injectable-runtime` as a dependency of package
#       `agtrs-runtime v0.1.0 (/root/rust_projects/agtrs/agtrs-runtime)`
#       Caused by: failed to load source for dependency `injectable-runtime`
#
#   That message names a crate in a repo the reader has probably never heard of,
#   mentions no remedy, and looks like a corrupt checkout. It is the single
#   most confusing failure a new contributor can hit. `just test` gates on this
#   script so the first thing they see is the fix.
#
# WHAT IT CHECKS
#   Two independent conditions, because either one alone gives a wrong answer:
#
#     1. Is the workspace graph actually broken?  Probed with the real command
#        (`cargo metadata`, full -- not --no-deps, which succeeds even when the
#        graph is broken). This is the source of truth.
#     2. Are the manifests already repointed at the sandbox? Probed with a grep
#        for the sandbox marker. This is what makes the message accurate: a
#        broken graph that is NOT sandbox-pointed is the case we can fix.
#
#   Checking only (1) would also catch the DIRTY-but-not-engaged state that
#   goal 29c338b6 tracks, where a manifest still names the sandbox but the
#   clone is gone. We deliberately do not try to repair that here: recovering
#   it is that goal's subject, and silently re-cloning on someone's behalf
#   would hide a tree that is one `git checkout` away from being clean.
#
# EXIT CODES
#   0  the graph resolves — carry on (sandbox engaged, or agtrs permanently fixed)
#   1  the graph is broken and the sandbox is not engaged — message above says why
#
# NOTE ON THE SANDBOX PATH
#   Read from XAFT_SANDBOX_DIR to match sandbox-build.sh, so a relocated
#   sandbox does not make this report a false problem.
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SANDBOX="${XAFT_SANDBOX_DIR:-/root/rust_projects/.xaft-agtrs-sandbox}"

# (2) Cheap and instant: are we already pointed at the sandbox?
if grep -rqs "xaft-agtrs-sandbox" --include=Cargo.toml .; then
  exit 0
fi

# (1) Source of truth. ~0.2s when broken, ~0.5s warm when healthy.
if cargo metadata --format-version 1 >/dev/null 2>&1; then
  exit 0
fi

cat >&2 <<'EOF'
error: xaft's workspace graph does not resolve, so no cargo command can run.

  xaft path-depends on the sibling repo ../agtrs, which is a separate
  workspace that has not been ported to the injectable crate rename. Cargo
  therefore fails inside `cargo metadata`, before compiling anything.

  Fix it with the build sandbox, which ports a CLONE of agtrs and never
  writes to the real repo:

      ./scripts/sandbox-build.sh          # engage (clones, ports, repoints)
      just test                           # or any other cargo recipe
      ./scripts/sandbox-build.sh --off    # MANDATORY before `git add`

  While the sandbox is engaged, 11 tracked Cargo.toml files carry rewritten
  agtrs paths. Running `--off` before `git add` is what keeps those local.

  To see the current state at any time:  ./scripts/sandbox-build.sh --status

EOF
exit 1
