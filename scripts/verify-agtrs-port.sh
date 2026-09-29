#!/usr/bin/env bash
#
# verify-agtrs-port.sh — prove that scripts/port-agtrs-injectable-rename.sh works,
#                        WITHOUT touching the real agtrs repository.
#
# WHY
#   port-agtrs-injectable-rename.sh rewrites 170 lines across 62 files in
#   /root/rust_projects/agtrs. That repository belongs to a different workspace,
#   so the edit cannot be applied without explicit authorization. Authorizing an
#   unvalidated 170-file mechanical rewrite is poor practice, so this harness
#   validates the exact same script against a throwaway clone first.
#
# HOW
#   1. `git clone` agtrs into a scratch directory INSIDE this repository. A clone
#      is cheap: only tracked files, no target/, no build cache.
#   2. Symlink ./injectable -> ../injectable so the clone's `path = "../injectable/..."`
#      dependencies resolve with the same relative depth they have in the real
#      layout. This keeps the manifests byte-identical to a real apply, so the
#      test is faithful rather than a rewritten approximation.
#   3. Temporarily add the scratch dir to this workspace's `exclude` list, so cargo
#      does not reject the nested package. Reverted unconditionally on exit.
#   4. A/B the two states:
#        unported clone -> `cargo metadata` MUST fail (reproduces the real bug)
#        ported  clone  -> `cargo metadata` MUST succeed
#   5. `cargo check --workspace --all-targets` the ported clone. This is the real
#      assertion: it proves the renamed dependency keys AND the renamed `::` paths
#      in every lib, bin, test and example target actually compile.
#   6. Clean up scratch dir and symlink.
#
# The real /root/rust_projects/agtrs is only ever READ (cloned from). It is never
# written, never stashed, never checked out.
#
# USAGE
#   ./scripts/verify-agtrs-port.sh              # metadata A/B + full cargo check (~5 min)
#   ./scripts/verify-agtrs-port.sh --fast       # metadata A/B only (~10 s)
#
# EXIT CODES
#   0  the port is proven good
#   1  the port did NOT fix the workspace, or the control case behaved unexpectedly
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
AGTRS="${AGTRS_REPO:-/root/rust_projects/agtrs}"
SCRATCH="$ROOT/.tmp-agtrs-check"
LINK="$ROOT/injectable"
PORT="$ROOT/scripts/port-agtrs-injectable-rename.sh"
FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

[ -d "$AGTRS/.git" ] || { echo "error: $AGTRS is not a git repository" >&2; exit 1; }
[ -f "$PORT" ]       || { echo "error: $PORT not found" >&2; exit 1; }
case "$SCRATCH" in
  *"/.tmp-agtrs-check") ;;
  *) echo "error: refusing to use scratch path $SCRATCH" >&2; exit 1 ;;
esac

cleanup() {
  # Undo the temporary workspace exclude.
  if [ -f /tmp/.agtrs-port-verify.Cargo.toml.bak ]; then
    cp /tmp/.agtrs-port-verify.Cargo.toml.bak "$ROOT/Cargo.toml" 2>/dev/null || true
    rm -f /tmp/.agtrs-port-verify.Cargo.toml.bak
  fi
  rm -rf "$SCRATCH" "$LINK" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cd "$ROOT"
cleanup   # clear any leftovers from an interrupted previous run
cp Cargo.toml /tmp/.agtrs-port-verify.Cargo.toml.bak
ln -sfn /root/rust_projects/injectable "$LINK"

echo "==> 1/6  Cloning $AGTRS (read-only; the real repo is never written)"
git clone --quiet "$AGTRS" "$SCRATCH"
echo "    HEAD: $(git -C "$SCRATCH" log --oneline -1)"
echo "    tracked files: $(git -C "$SCRATCH" ls-files | wc -l)"

python3 - "$ROOT/Cargo.toml" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if "exclude" not in s:
    p.write_text(s.replace('resolver = "2"\n',
                           'resolver = "2"\nexclude = [".tmp-agtrs-check"]\n', 1))
PY
echo "    workspace exclude set for the scratch dir"

echo
echo "==> 2/6  Control: the UNPORTED clone must FAIL to resolve"
if ( cd "$SCRATCH" && cargo metadata --format-version 1 >/dev/null 2>/tmp/.ab.err ); then
  echo "    UNEXPECTED: unported clone resolved. Is agtrs already ported?"
  exit 1
fi
echo "    confirmed broken, as expected:"
grep -m1 '^error' /tmp/.ab.err | sed 's/^/      /'

echo
echo "==> 3/6  Applying the port to the scratch clone"
AGTRS_REPO="$SCRATCH" "$PORT" --apply | sed 's/^/    /'

echo
echo "==> 4/6  The PORTED clone must resolve"
( cd "$SCRATCH" && cargo metadata --format-version 1 >/dev/null ) \
  || { echo "    FAILED: still unresolvable after the port" >&2; exit 1; }
echo "    resolved OK"

echo
echo "==> 5/6  Diff shape — a pure 1:1 rename must not add or remove lines"
cd "$SCRATCH"
ins=$(git diff --numstat | awk '{s+=$1} END {print s+0}')
del=$(git diff --numstat | awk '{s+=$2} END {print s+0}')
files=$(git diff --numstat | wc -l)
echo "    $files files, $ins insertions, $del deletions"
[ "$ins" -eq "$del" ] || { echo "    FAILED: not a 1:1 line substitution" >&2; exit 1; }
cd "$ROOT"

if [ "$FAST" -eq 1 ]; then
  echo
  echo "==> 6/6  Skipped (--fast). Port validated at the manifest level."
  echo "RESULT: PASS (metadata A/B only; re-run without --fast to compile)"
  exit 0
fi

echo
echo "==> 6/6  Compiling the ported clone: cargo check --workspace --all-targets"
echo "    (this is the real assertion: libs, bins, tests and examples)"
if ( cd "$SCRATCH" && cargo check --workspace --all-targets >/tmp/.agtrs-port-verify.log 2>&1 ); then
  echo "    compiled clean"
  echo
  echo "    any diagnostics mentioning injectable:"
  grep -inE 'injectable' /tmp/.agtrs-port-verify.log | sed 's/^/      /' || echo "      none"
  echo
  echo "RESULT: PASS — the port produces a fully compiling agtrs workspace."
  echo "        It is now safe to authorize:"
  echo "          cd $ROOT && ./scripts/port-agtrs-injectable-rename.sh --apply"
  echo "        pointing at the real repo, then commit (or not) as the user prefers."
  exit 0
else
  echo "    FAILED to compile. Diagnostics:"
  grep -E '^(error|warning: unused)' /tmp/.agtrs-port-verify.log | head -40 | sed 's/^/      /'
  exit 1
fi
