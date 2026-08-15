#!/usr/bin/env bash
# ============================================================================
# xaft — CI/test runner (noxfile.py equivalent)
#
# Runs the full pre-release gate in one invocation, mirroring agenthicc's
# nox sessions for a Rust workspace:
#   1. cargo fmt --check          (formatting)
#   2. cargo clippy -D warnings   (lint)
#   3. cargo test --workspace     (tests)
#   4. node scripts/docs-site.cjs --check   (docs build + link check)
#
# Usage:
#   ./scripts/ci.sh             # run the full gate
#   ./scripts/ci.sh --quick     # skip the docs check (faster local iteration)
#   ./scripts/ci.sh --no-fmt    # skip formatting check
#   ./scripts/ci.sh --no-clippy # skip clippy
#
# Exit code 0 when everything passes; non-zero on the first failure.
# ============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QUICK=0
NO_FMT=0
NO_CLIPPY=0

for arg in "$@"; do
  case "$arg" in
    --quick)   QUICK=1 ;;
    --no-fmt)  NO_FMT=1 ;;
    --no-clippy) NO_CLIPPY=1 ;;
    *) echo "ci.sh: unknown option '$arg' (allowed: --quick, --no-fmt, --no-clippy)" >&2; exit 2 ;;
  esac
done

pass() { echo "  ✓ $*"; }
fail() { echo "  ✗ $*" >&2; exit 1; }

echo "==> xaft CI gate"

# ── 1. Formatting ───────────────────────────────────────────────────────────
if [[ "$NO_FMT" -eq 0 ]]; then
  echo "── fmt --check"
  if cargo fmt --all -- --check; then pass "formatting clean"; else fail "cargo fmt --check"; fi
else
  echo "── fmt (skipped)"
fi

# ── 2. Lint ─────────────────────────────────────────────────────────────────
if [[ "$NO_CLIPPY" -eq 0 ]]; then
  echo "── clippy -D warnings (xaft crates only)"
  # The sibling `agtrs` framework is a path dependency with its own lint
  # policy; `-D warnings` over the whole workspace would fail on agtrs's
  # pre-existing warnings. Lint xaft's own crates strictly instead.
  XAFT_P=""
  for d in "$ROOT"/crates/*/; do
    XAFT_P="$XAFT_P -p $(basename "$d")"
  done
  if cargo clippy $XAFT_P -- -D warnings --cap-lints warn; then pass "clippy clean"; else fail "cargo clippy (xaft crates)"; fi
else
  echo "── clippy (skipped)"
fi

# ── 3. Tests ────────────────────────────────────────────────────────────────
echo "── test --workspace"
if cargo test --workspace; then pass "tests pass"; else fail "cargo test"; fi

# ── 4. Docs build + link check ──────────────────────────────────────────────
if [[ "$QUICK" -eq 0 ]]; then
  echo "── docs-site --check"
  if node scripts/docs-site.cjs --check; then pass "docs clean"; else fail "docs-site check"; fi
else
  echo "── docs (skipped --quick)"
fi

echo "==> CI gate passed"
