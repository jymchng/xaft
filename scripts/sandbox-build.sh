#!/usr/bin/env bash
#
# sandbox-build.sh — let xaft compile and be tested WITHOUT touching /root/rust_projects/agtrs.
#
# WHY THIS EXISTS
#   xaft's crates depend on agtrs via `path = "../../../agtrs/..."`, which resolves
#   to /root/rust_projects/agtrs. That repo is a separate workspace and is still
#   unported to the injectable crate rename, so `cargo check --workspace`,
#   `cargo test --workspace` and `cargo tarpaulin` all abort inside
#   `cargo metadata` — which makes every test, coverage and parity goal
#   unverifiable.
#
#   We have already PROVEN the rename is safe: port-agtrs-injectable-rename.sh
#   applies 170 literal 1:1 substitutions, and verify-agtrs-port.sh compiles the
#   result with `cargo check --workspace --all-targets` (exit 0). This script
#   reuses that exact, proven port — but clones it INTO this repository and
#   repoints xaft's manifests at the clone.
#
# WHAT IT CHANGES (and how to be sure it is all reverted)
#   * .agtrs-sandbox/            — the ported agtrs clone (new, git-ignored)
#   * ./injectable               — symlink to ../injectable, so the clone's
#                                  `path = "../injectable/..."` deps resolve
#   * crates/*/Cargo.toml        — `../../../agtrs/` -> `../../.agtrs-sandbox/`
#   * Cargo.toml                 — adds .agtrs-sandbox to workspace `exclude`
#
#   Every original file is backed up under .agtrs-sandbox/originals/ before it is
#   touched, and `--off` restores them. `--status` reports whether the sandbox is
#   currently engaged.
#
#   *** The sandbox edits are LOCAL ONLY and must never be committed. ***
#   Run `./scripts/sandbox-build.sh --off` before `git add`. `.agtrs-sandbox/`
#   is in .gitignore so the clone itself cannot be committed by accident.
#
# USAGE
#   ./scripts/sandbox-build.sh            # engage: clone, port, repoint, then cargo check
#   ./scripts/sandbox-build.sh --no-check # engage without running cargo check
#   ./scripts/sandbox-build.sh --status   # report state, change nothing
#   ./scripts/sandbox-build.sh --off      # restore every manifest, delete the clone
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SANDBOX="$ROOT/.agtrs-sandbox"
ORIG="$SANDBOX/originals"
AGTRS="${AGTRS_REPO:-/root/rust_projects/agtrs}"
PORT="$ROOT/scripts/port-agtrs-injectable-rename.sh"
DO_CHECK=1

[ -f "$PORT" ] || { echo "error: $PORT not found" >&2; exit 1; }
cd "$ROOT"

# ---------------------------------------------------------------- restore
restore() {
  local n=0 f
  if [ -d "$ORIG" ]; then
    while IFS= read -r f; do
      local rel="${f#$ORIG/}"
      cp "$f" "$ROOT/$rel" && n=$((n+1))
    done < <(find "$ORIG" -type f)
  fi
  rm -rf "$SANDBOX" "$ROOT/injectable"
  echo "restored $n manifest(s); sandbox removed"
}

case "${1:-}" in
  --off)
    [ -d "$SANDBOX" ] || { echo "sandbox not engaged; nothing to do"; exit 0; }
    restore
    exit 0 ;;
  --status)
    if [ -d "$SANDBOX" ]; then
      echo "sandbox: ENGAGED at $SANDBOX"
      echo "  patched manifests:"
      ( cd "$ORIG" 2>/dev/null && find . -type f | sed 's|^\./|  |' ) || true
      echo "  DO NOT COMMIT while engaged."
    else
      echo "sandbox: not engaged (manifests are pristine)"
    fi
    exit 0 ;;
  "") ;;
  --no-check) DO_CHECK=0 ;;
  *) sed -n '30,45p' "$0"; exit 2 ;;
esac

[ -d "$AGTRS/.git" ] || { echo "error: $AGTRS is not a git repository" >&2; exit 1; }

# Refuse to clobber an already-engaged sandbox with a possibly-modified clone.
[ -d "$SANDBOX" ] && { echo "error: sandbox already engaged; run --off first" >&2; exit 1; }

echo "==> 1/5  Backing up every manifest this will touch"
mkdir -p "$ORIG/crates"
cp Cargo.toml "$ORIG/Cargo.toml"
for m in crates/*/Cargo.toml; do
  mkdir -p "$(dirname "$ORIG/$m")"
  cp "$m" "$ORIG/$m"
done
echo "    backed up: Cargo.toml + $(find "$ORIG/crates" -name Cargo.toml | wc -l) crate manifests"

echo
echo "==> 2/5  Cloning + porting agtrs into $SANDBOX (real agtrs is only READ)"
mkdir -p "$SANDBOX"
git clone --quiet "$AGTRS" "$SANDBOX/agtrs"
ln -sfn /root/rust_projects/injectable "$SANDBOX/injectable"
AGTRS_REPO="$SANDBOX/agtrs" "$PORT" --apply >/dev/null
echo "    ported: $(git -C "$SANDBOX/agtrs" diff --numstat | wc -l) files, $(
        git -C "$SANDBOX/agtrs" diff --numstat | awk '{i+=$1} END {print i+0}') insertions"

# Cargo attributes a path dep that lives inside another workspace's directory
# tree to the ANCESTOR workspace, ignoring the nested [workspace] and the
# ancestor's `exclude`. So the nested workspace must go: flatten every agtrs
# crate into a standalone package that inherits nothing.
python3 "$ROOT/scripts/flatten_agtrs_workspace.py" "$SANDBOX/agtrs"

# The injectable crates are themselves a workspace. Left pointing at the local
# ./injectable symlink, cargo would walk UP from the symlinked path, find
# xaft's Cargo.toml, and try to inherit `authors` from it:
#     error inheriting `authors` from workspace root manifest's
#     `workspace.package.authors` / `workspace.package.authors` was not defined
# Rewriting to the real absolute path makes cargo find injectable's OWN root.
# This is the same dependency relationship xaft already declares directly in
# crates/*/Cargo.toml, so nothing new is depended upon.
find "$SANDBOX/agtrs" -name Cargo.toml -print0 | xargs -0 \
  sed -i 's|path = "../../injectable/|path = "/root/rust_projects/injectable/|g'

echo
echo "==> 3/5  Repointing xaft manifests at the sandbox"
# crates/<name>/Cargo.toml : ../../../agtrs/  ->  ../../.agtrs-sandbox/agtrs/
find "$ROOT/crates" -name Cargo.toml -print0 | xargs -0 sed -i 's|\.\./\.\./\.\./agtrs/|../../.agtrs-sandbox/agtrs/|g'
n=$(grep -rl '\.\./\.\./\.agtrs-sandbox/' "$ROOT/crates" --include=Cargo.toml | wc -l)
echo "    repointed $n crate manifest(s)"

python3 - "$ROOT/Cargo.toml" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
if 'exclude' not in s:
    p.write_text(s.replace('resolver = "2"\n',
                           'resolver = "2"\n# LOCAL SANDBOX (scripts/sandbox-build.sh) - not for commit\nexclude = [".agtrs-sandbox"]\n', 1))
PY
echo "    workspace exclude set"

echo
echo "==> 4/5  Verifying the graph resolves"
cargo metadata --format-version 1 >/dev/null 2>/tmp/.sandbox.err \
  || { echo "    FAILED to resolve:"; head -20 /tmp/.sandbox.err; exit 1; }
echo "    cargo metadata: OK"

echo
echo "==> 5/5  Sandbox engaged"
echo "    agtrs source : $SANDBOX/agtrs  (ported clone, NOT /root/rust_projects/agtrs)"
echo "    xaft check   : ./scripts/sandbox-build.sh --status"
echo "    teardown     : ./scripts/sandbox-build.sh --off"
echo
echo "    *** Originals are backed up in $ORIG — do NOT commit these edits. ***"
echo "    *** Run './scripts/sandbox-build.sh --off' before 'git add'. ***"

if [ "$DO_CHECK" -eq 1 ]; then
  echo
  echo "==> Running cargo check --workspace (first build is slow)"
  if cargo check --workspace >/tmp/.sandbox-check.log 2>&1; then
    echo "    cargo check --workspace: OK"
    grep -E '^(error|warning: unused)' /tmp/.sandbox-check.log | head -10 || true
  else
    echo "    cargo check FAILED:"
    grep -E '^error' /tmp/.sandbox-check.log | head -20 | sed 's/^/      /'
    exit 1
  fi
fi
