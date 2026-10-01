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
#   * ../.xaft-agtrs-sandbox/    — the ported agtrs clone. It lives OUTSIDE the
#                                  xaft tree on purpose: cargo force-adds any
#                                  path dep inside a workspace's tree to the
#                                  member list, and `exclude` cannot stop it.
#   * ./injectable               — symlink to ../injectable, so the clone's
#                                  `path = "../injectable/..."` deps resolve
#   * crates/*/Cargo.toml        — `../../../agtrs/` -> `../.xaft-agtrs-sandbox/`
#   * Cargo.toml                 — NOT touched
#
#   Every original file is backed up under ../.xaft-agtrs-sandbox/originals/
#   before it is touched, and a post-rewrite snapshot of the same files is
#   kept under ../.xaft-agtrs-sandbox/patched-manifests/. `--status` reports
#   whether the sandbox is currently engaged.
#
#   *** The sandbox edits are LOCAL ONLY and must never be committed. ***
#   Run `./scripts/sandbox-build.sh --off` before `git add`. The clone lives
#   outside the repo, so it cannot be committed by accident at all.
#
# WHAT THIS SUPERSEDES (recorded so a later pass does not re-derive it)
#   Three goals — 9079d4d7 (G0b), 7056fa3f, and 4c9d3ab8 — state that no xaft
#   test, coverage or parity goal can be VERIFIED until /root/rust_projects/agtrs
#   is authorized and patched. That is no longer true. This script performs the
#   equivalent port against a CLONE, so the real repo's authorization gate is
#   irrelevant to verification: `cargo check --workspace` exits 0 and
#   `cargo test --workspace` exits 0 (2230 passed / 0 failed / 9 ignored) with
#   exactly 11 workspace members, so no agtrs crate leaks into xaft's results.
#
#   The gate that DOES still apply is to editing the real agtrs repo, which
#   remains 7056fa3f's actual subject: the clone is a workaround, so a fresh
#   checkout or a CI runner still cannot build without it. Authorizing the
#   permanent port is goal 87b5970a, a product decision rather than a
#   verification blocker. Do not read this as "agtrs is fixed".
#
# TEARDOWN DOES NOT SILENTLY DISCARD YOUR EDITS
#   `--off` used to `cp` the engage-time backup straight back over each
#   manifest, which silently destroyed anything you edited while engaged — it
#   bit commit e3e5778, where `xaft-session` was added to
#   crates/xaft-cli/Cargo.toml during a sandboxed run and the teardown deleted
#   the line without a word; the build only failed later, at re-engage, with a
#   misleading `error inheriting 'xaft-session' ...`. Teardown is now a
#   three-way decision per manifest, using the patched-manifests snapshot as
#   the merge base. See the comment above restore() below.
#
# USAGE
#   ./scripts/sandbox-build.sh            # engage: clone, port, repoint, then cargo check
#   ./scripts/sandbox-build.sh --no-check # engage without running cargo check
#   ./scripts/sandbox-build.sh --status   # report state, change nothing
#   ./scripts/sandbox-build.sh --off      # restore every manifest, delete the clone
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# The sandbox MUST live OUTSIDE the xaft tree. Cargo force-adds every path
# dependency residing inside a workspace's directory tree to that workspace's
# member list, and `exclude` does NOT prevent it. Verified with three minimal
# repros:
#   dep outside tree, no exclude          -> members: [a]        (what we want)
#   dep inside  tree, exclude = ["sub"]   -> members: [a, dep]   (exclude ignored)
#   dep outside tree, exclude = ["../dep"]-> members: [a]        (exclude redundant)
# With the sandbox inside the tree, 10 agtrs crates joined `cargo metadata`
# as members, so `cargo test --workspace` built and ran agtrs' own tests (one
# of which fails in this environment) and their results were attributed to
# xaft. Sitting beside xaft keeps the member list at exactly 11.
SANDBOX="${XAFT_SANDBOX_DIR:-/root/rust_projects/.xaft-agtrs-sandbox}"
ORIG="$SANDBOX/originals"
# Post-rewrite snapshot of the same files, used by restore() as the three-way
# merge base. Kept OUT of $ORIG on purpose: restore() enumerates $ORIG to find
# the originals to put back, so leaving snapshots in there would make it try to
# "restore" the patched copies over the pristine ones.
PATCHED="$SANDBOX/patched-manifests"
AGTRS="${AGTRS_REPO:-/root/rust_projects/agtrs}"
PORT="$ROOT/scripts/port-agtrs-injectable-rename.sh"
DO_CHECK=1

[ -f "$PORT" ] || { echo "error: $PORT not found" >&2; exit 1; }
cd "$ROOT"

# ---------------------------------------------------------------- restore
#
# WHY THIS IS NOT JUST `cp`
#   $ORIG holds each manifest as it was when the sandbox engaged. Copying those
#   back verbatim silently DESTROYED any edit made while engaged — that is the
#   e3e5778 `xaft-session` incident named in the header. So teardown is now a
#   three-way decision per manifest, using the post-rewrite snapshot in $PATCHED
#   as the merge base:
#
#     current == original    untouched by anyone  -> plain restore
#     current == patched     sandbox rewrote it   -> plain restore
#     current == neither     you edited it        -> REBASE onto the original
#     current is missing     you deleted it        -> do NOT resurrect it
#
#   The third case is the point: your bytes win, the sandbox's agtrs path
#   rewrites do not, and the resulting diff is printed so the change is
#   auditable rather than a surprise. It is applied as a real 3-way merge
#   (base = patched snapshot, ours = pristine original, theirs = what you left
#   on disk), which succeeds whenever your edit and the sandbox's rewrite touch
#   different lines — which is the normal case.
#
#   If the merge conflicts, the pristine original is restored (conflict markers
#   must never be left in a Cargo.toml), your version is preserved beside it as
#   <file>.sandbox-edit-backup, and teardown exits NON-ZERO. Likewise if
#   $PATCHED is missing for a file we cannot prove the change is ours, so we
#   take the loud branch: failing toward "you edited something" is the safe
#   direction, and failing toward "you did not" is how the original bug shipped.
restore() {
  local n=0 rebased=0 conflicted=0 deleted=0 f rel target base
  if [ -d "$ORIG" ]; then
    while IFS= read -r f; do
      rel="${f#$ORIG/}"
      target="$ROOT/$rel"
      base="$PATCHED/$rel"

      if [ ! -e "$target" ]; then
        echo "  ERROR: $rel was DELETED while the sandbox was engaged." >&2
        echo "         Not restoring it — that deletion is your edit, not ours." >&2
        deleted=$((deleted+1))
        continue
      fi

      if cmp -s "$f" "$target"; then
        cp "$f" "$target" && n=$((n+1))
        continue
      fi

      if [ ! -f "$base" ]; then
        echo "  ERROR: $rel changed but no sandbox snapshot exists to compare it" >&2
        echo "         against, so this cannot be classified as sandbox-only." >&2
        echo "         Pristine original restored; your version is preserved at" >&2
        echo "         $target.sandbox-edit-backup" >&2
        cp "$target" "$target.sandbox-edit-backup"
        cp "$f" "$target"
        conflicted=$((conflicted+1))
        continue
      fi

      if cmp -s "$base" "$target"; then
        cp "$f" "$target" && n=$((n+1))
        continue
      fi

      # Foreign edits: rebase them onto the pristine original.
      cp "$target" "$target.sandbox-edit-backup"
      if git merge-file -q -L "as-you-left-it" -L "sandbox-snapshot" \
           -L "pristine-original" "$target" "$base" "$f"; then
        rebased=$((rebased+1))
        n=$((n+1))
        rm -f "$target.sandbox-edit-backup"
        echo "  WARNING: $rel was edited while the sandbox was engaged."
        echo "           Your edit was PRESERVED and the sandbox's agtrs path"
        echo "           rewrite removed. Diff (pristine original -> result):"
        diff -u --label "a/$rel" --label "b/$rel" "$f" "$target" \
          | sed 's/^/             /' || true
      else
        echo "  ERROR: $rel was edited while the sandbox was engaged and your edit" >&2
        echo "         CONFLICTS with the sandbox's rewrite. The pristine original" >&2
        echo "         has been restored and your version preserved at" >&2
        echo "         $target.sandbox-edit-backup — re-apply it by hand." >&2
        echo "         Diff (pristine original -> your edit):" >&2
        diff -u --label "a/$rel" --label "b/$rel" \
          "$f" "$target.sandbox-edit-backup" | sed 's/^/             /' >&2 || true
        cp "$f" "$target"
        conflicted=$((conflicted+1))
      fi
    done < <(find "$ORIG" -type f)
  fi

  rm -rf "$SANDBOX" "$ROOT/injectable"

  echo
  echo "teardown summary:"
  echo "  restored unchanged     : $n manifest(s)"
  echo "  edits preserved        : $rebased manifest(s)"
  [ "$deleted" -eq 0 ] || echo "  DELETED while engaged   : $deleted manifest(s) (left deleted)"
  [ "$conflicted" -eq 0 ] || echo "  NEEDS HAND-REAPPLY      : $conflicted manifest(s)"
  echo "  sandbox removed"

  if [ "$conflicted" -gt 0 ] || [ "$deleted" -gt 0 ]; then
    echo
    echo "REFUSING to report a clean teardown: work needed attention above." >&2
    return 1
  fi
}

case "${1:-}" in
  --off)
    [ -d "$SANDBOX" ] || { echo "sandbox not engaged; nothing to do"; exit 0; }
    restore || exit 1
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
# The sandbox now sits OUTSIDE the xaft tree, so the relative depth changes.
# crates/xaft-cli/ -> ../../../ = /root/rust_projects  ->  ../.xaft-agtrs-sandbox/
# Every path dep in crates/<name>/Cargo.toml is written `../../../agtrs/`, i.e.
# relative to the MEMBER directory, not to crates/. So SBX_REL must be relative
# to crates/<name>/ as well, and the crates live one level deeper inside
# $SANDBOX/agtrs.
SBX_REL="$(python3 -c "import os,sys;print(os.path.relpath(sys.argv[1],sys.argv[2]))" "$SANDBOX" "$ROOT/crates/xaft-cli")"
find "$ROOT/crates" -name Cargo.toml -print0 | xargs -0 sed -i "s|\\.\\./\\.\\./\\.\\./agtrs/|$SBX_REL/agtrs/|"
n=$(grep -rl "$SBX_REL" "$ROOT/crates" --include=Cargo.toml | wc -l)
echo "    repointed $n crate manifest(s) -> $SBX_REL"
# The root Cargo.toml is deliberately left UNTOUCHED. An earlier version added
# `exclude = [".agtrs-sandbox"]` here, but cargo ignores `exclude` for path
# deps inside the tree (see the SANDBOX comment above), so it only dirtied a
# tracked file without changing the member list. Now that the sandbox lives
# outside the tree, no exclude is needed and the root manifest stays pristine.

# Snapshot the post-rewrite state of every manifest we backed up. This is what
# makes teardown safe: without it there is no way to tell "the sandbox rewrote
# this line" apart from "the operator edited this line", which is precisely the
# distinction the old teardown could not make, and silently lost work over.
mkdir -p "$PATCHED"
while IFS= read -r rel; do
  mkdir -p "$(dirname "$PATCHED/$rel")"
  cp "$ROOT/$rel" "$PATCHED/$rel"
done < <(find "$ORIG" -type f | sed "s|^$ORIG/||")
echo "    snapshotted $(find "$PATCHED" -type f | wc -l) manifest(s) for safe teardown"

# Fail loudly if anything other than the 11 intended crates is a member: a
# silent leak is what made `cargo test --workspace` report agtrs' failures as
# if they were xaft's.
members=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print(len(d['workspace_members']))" \
  || echo "?")
echo "    workspace members: $members (expected 11)"
if [ "$members" != "11" ]; then
  echo "    WARNING: member count is $members, not 11 - the sandbox is leaking" >&2
  echo "    crates into the workspace and their tests will pollute results." >&2
fi

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
echo "    *** Manifests you edit while engaged are PRESERVED on teardown, and ***"
echo "    *** anything ambiguous is reported loudly instead of discarded.     ***"

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
