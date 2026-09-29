#!/usr/bin/env bash
#
# port-agtrs-injectable-rename.sh — repair the agtrs ↔ injectable dependency names.
#
# WHY THIS EXISTS
#   The sibling repository /root/rust_projects/injectable renamed its three
#   published crates:
#       injectable        ->  injectable-rs
#       injectable-runtime ->  injectable-rs-runtime
#       injectable-graph   ->  injectable-rs-graph
#   The sibling repository /root/rust_projects/agtrs was never updated: its root
#   Cargo.toml still points at ../injectable/injectable{, -runtime, -graph}, which
#   no longer exist. Cargo cannot resolve the graph, so `cargo check --workspace`,
#   `cargo test --workspace` and `cargo tarpaulin` all fail inside `cargo metadata`
#   — in agtrs AND in this workspace (xaft), which depends on 9 agtrs crates.
#
#   Cargo's [patch] and [replace] cannot repair this: the broken entries are *path*
#   dependencies declared by another workspace, and the paths do not exist at all.
#
# WHAT IT CHANGES
#   Build-critical files only, enumerated from `git ls-files` so nothing untracked
#   and nothing under target/ is ever touched:
#
#     Cargo.toml (root + members)   3 workspace deps, 17 member dep keys,
#                                   1 feature reference "injectable/axum"
#     *.rs                          injectable_runtime:: -> injectable_rs_runtime::
#                                   injectable_graph::   -> injectable_rs_graph::
#                                   injectable::         -> injectable_rs::
#     .github/workflows/*.{yml,yaml}  --features injectable/axum -> injectable-rs/axum
#
#   Cargo.lock is NOT edited; cargo regenerates it on the next build.
#
#   Prose (README, guides/, PRDs/, skills/, agtrs' own justfile) is NOT touched by
#   default — pass --with-docs to include it. Note that agtrs/justfile is a copy of
#   injectable's own justfile and still contains `cargo package -p injectable-*`
#   recipes; those are pre-existing cruft, unrelated to the build.
#
# USAGE
#   ./scripts/port-agtrs-injectable-rename.sh                 # dry run (default)
#   ./scripts/port-agtrs-injectable-rename.sh --apply         # write the changes
#   ./scripts/port-agtrs-injectable-rename.sh --with-docs     # also rewrite prose
#   ./scripts/port-agtrs-injectable-rename.sh --check         # verify only, no writes
#
# ROLLBACK
#   git -C /root/rust_projects/agtrs checkout -- .
#
set -euo pipefail

AGTRS="${AGTRS_REPO:-/root/rust_projects/agtrs}"
APPLY=0
CHECK_ONLY=0
WITH_DOCS=0

for arg in "$@"; do
  case "$arg" in
    --apply)     APPLY=1 ;;
    --check)     CHECK_ONLY=1; APPLY=0 ;;
    --with-docs) WITH_DOCS=1 ;;
    -h|--help)   sed -n '2,45p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

[ -d "$AGTRS/.git" ] || { echo "error: $AGTRS is not a git repository" >&2; exit 1; }
[ -d "$AGTRS/../injectable/injectable-rs-runtime" ] || {
  echo "error: ../injectable/injectable-rs-runtime not found next to $AGTRS" >&2; exit 1; }

# Literal (non-regex) replacement pairs, longest key first so that the
# `injectable` rule can never chew into `injectable-runtime`.
rs_pairs=(
  'injectable_runtime::|injectable_rs_runtime::'
  'injectable_graph::|injectable_rs_graph::'
  'injectable::|injectable_rs::'
)
toml_pairs=(
  'injectable-runtime = { path = "../injectable/injectable-runtime" }|injectable-rs-runtime = { path = "../injectable/injectable-rs-runtime" }'
  'injectable-runtime = { workspace = true }|injectable-rs-runtime = { workspace = true }'
  'injectable-graph = { path = "../injectable/injectable-graph" }|injectable-rs-graph = { path = "../injectable/injectable-rs-graph" }'
  'injectable-graph = { workspace = true }|injectable-rs-graph = { workspace = true }'
  'injectable = { path = "../injectable/injectable" }|injectable-rs = { path = "../injectable/injectable-rs" }'
  'injectable = { workspace = true }|injectable-rs = { workspace = true }'
  '"injectable/axum"|"injectable-rs/axum"'
)
# One generic rule: it covers the 9 `--features injectable/axum` cargo
# invocations and the 2 "(injectable/axum)" CI step labels in one pass.
yml_pairs=(
  'injectable/axum|injectable-rs/axum'
)
# Prose-only, opt-in. Bare `injectable` becomes `injectable-rs`; the two
# suffixed rules must run first so they are not rewritten again.
doc_pairs=(
  'injectable-runtime|injectable-rs-runtime'
  'injectable-graph|injectable-rs-graph'
  'injectable-macros|injectable-rs-macros'
  'injectable|injectable-rs'
)

tracked() { git -C "$AGTRS" ls-files "$@"; }

# apply_pairs FILE "pairs..."  -> prints number of substitutions made
apply_pairs() {
  local file="$1"; shift
  local pair old new n=0
  for pair in "$@"; do
    old="${pair%%|*}"; new="${pair##*|}"
    c=$(grep -cF -- "$old" "$AGTRS/$file" || true)
    if [ "$c" -gt 0 ]; then
      n=$((n + c))
      if [ "$APPLY" -eq 1 ]; then
        python3 - "$AGTRS/$file" "$old" "$new" <<'PY'
import pathlib, sys
p, old, new = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
p.write_text(p.read_text().replace(old, new))
PY
      fi
    fi
  done
  echo "$n"
}

verify() {
  local fail=0 pattern label hits
  for pattern in \
    'injectable_runtime::' \
    'injectable_graph::' \
    'injectable::' \
    'injectable-runtime =' \
    'injectable-graph =' \
    'injectable = {' \
    'injectable/axum' \
    '--features injectable/axum' \
  ; do
    label="no '$pattern'"
    # git grep searches tracked files only, so target/ and build junk are
    # skipped without a --exclude and without a slow directory walk.
    # `git grep` exits 1 on no-match, which must not trip `set -e`/pipefail.
    # The pathspecs must be basename globs: a bare `Cargo.toml` pathspec is
    # resolved literally against the repo root and would miss every member.
    hits=$( { git -C "$AGTRS" grep -IF -e "$pattern" \
                   -- '*.rs' '*.toml' '.github/workflows/*' 2>/dev/null || true; } | wc -l )
    if [ "$hits" -gt 0 ]; then
      echo "  FAIL  $label: $hits occurrence(s) remain"
      fail=1
    else
      echo "  ok    $label"
    fi
  done
  return $fail
}

if [ "$CHECK_ONLY" -eq 1 ]; then
  echo "==> Checking $AGTRS (no writes)"
  verify
  exit $?
fi

echo "==> Target: $AGTRS"
echo "==> Mode:   $([ "$APPLY" -eq 1 ] && echo APPLY || echo 'DRY RUN (pass --apply to write)')"
[ "$WITH_DOCS" -eq 1 ] && echo "==> Prose files included"
echo

total=0
touched=0

for f in $(tracked '*/Cargo.toml' 'Cargo.toml'); do
  n=$(apply_pairs "$f" "${toml_pairs[@]}")
  [ "$n" -gt 0 ] && { echo "  Cargo.toml  $f  ($n)"; total=$((total+n)); touched=$((touched+1)); }
done

for f in $(tracked '*.rs'); do
  n=$(apply_pairs "$f" "${rs_pairs[@]}")
  [ "$n" -gt 0 ] && { echo "  .rs         $f  ($n)"; total=$((total+n)); touched=$((touched+1)); }
done

for f in $(tracked '.github/workflows/*.yml' '.github/workflows/*.yaml'); do
  n=$(apply_pairs "$f" "${yml_pairs[@]}")
  [ "$n" -gt 0 ] && { echo "  workflow    $f  ($n)"; total=$((total+n)); touched=$((touched+1)); }
done

if [ "$WITH_DOCS" -eq 1 ]; then
  for f in $(tracked '*.md' 'justfile' '*/justfile'); do
    case "$f" in target/*) continue ;; esac
    n=$(apply_pairs "$f" "${doc_pairs[@]}")
    [ "$n" -gt 0 ] && { echo "  prose       $f  ($n)"; total=$((total+n)); touched=$((touched+1)); }
  done
fi

echo
echo "==> $total substitution(s) across $touched file(s)"

if [ "$APPLY" -eq 1 ]; then
  echo
  echo "==> Post-flight verification"
  verify || { echo; echo "verification failed — run: git -C $AGTRS checkout -- ."; exit 1; }
  echo
  echo "==> Next steps"
  echo "  1. cd $AGTRS && cargo check --workspace"
  echo "  2. cd $(dirname "$AGTRS")/xaft && cargo check --workspace"
  echo "  3. Review with: git -C $AGTRS diff"
else
  echo
  echo "==> Dry run only. Re-run with --apply to write, or --check to verify."
fi
