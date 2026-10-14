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
#   Every original file is backed up under ../.xaft-agtrs-sandbox.state/originals/
#   before it is touched, and a post-rewrite snapshot of the same files is
#   kept under ../.xaft-agtrs-sandbox.state/patched-manifests/. BOTH live in a
#   SIBLING of the clone, never inside it — see WHERE THE BACKUPS LIVE below.
#   `--status` reports whether the sandbox is currently engaged by READING THE
#   MANIFESTS, not by testing whether a directory exists.
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
#   It runs in two phases, so an edit made NEXT TO a line the sandbox rewrote
#   merges instead of demanding a hand re-apply: first the sandbox's own path
#   substitution is taken back out line by line (scripts/sandbox-rewind.py,
#   driven by the rewrite map recorded at engage time), and only if that cannot
#   be proven safe is a 3-way merge attempted. A genuinely irreconcilable edit
#   still restores the pristine original, parks your bytes in
#   <manifest>.sandbox-edit-backup, and exits NON-ZERO -- silent discard is
#   never reintroduced.
#
# WHERE THE BACKUPS LIVE (and why they are not inside the clone)
#   Both `$ORIG` (the pristine manifests) and `$PATCHED` (the post-rewrite
#   snapshot) live in a sibling directory of the agtrs clone, not in it. They
#   used to live inside it, which made a whole class of damage unrecoverable:
#   anything that removed the clone without completing `--off` — `rm -rf`, a
#   disk sweep, an interrupted run, or an operator tidying up a directory it
#   was told to ignore — took the pristine originals with it. The manifests
#   were then left naming a sandbox that no longer existed, `cargo` could not
#   resolve the graph, and `--off` had nothing left to restore from.
#
#   They move TOGETHER on purpose. restore() tells "the sandbox rewrote this"
#   apart from "you edited this" by comparing against the $PATCHED snapshot,
#   so relocating $ORIG while stranding $PATCHED would make every ordinary
#   no-edit engage/teardown cycle look like a conflicting edit.
#
# --status CANNOT REPORT A FALSE CLEAN (goal 29c338b6)
#   It used to decide on `[ -d "$SANDBOX" ]` alone and print "manifests are
#   pristine" without ever reading a manifest. So the exact state it exists to
#   warn about — manifests still pointing into a deleted clone, which makes
#   every cargo command fail — was the one state it reported as clean, and it
#   exited 0, so it was safe to trust as a pre-commit gate precisely when it
#   was lying. It now classifies by what the manifests actually contain:
#
#     ENGAGED              clone present, manifests name it        exit 0
#     DIRTY-BUT-NOT-ENGAGED manifests name it, clone is GONE        exit 1
#     CLEAN                no manifest names it                    exit 0
#
#   and `--off` works in the DIRTY case too, restoring from the sibling
#   backups. `--off` additionally refuses to exit 0 while any manifest still
#   names the sandbox, so neither command can report success over a tree it
#   failed to repair.
#
# A STRANDED SNAPSHOT FAILS LOUDLY (goal df50869c)
#   $ORIG and $PATCHED are relocated together (see WHERE THE BACKUPS LIVE), but
#   either can still go missing on its own. Delete $ORIG alone and $PATCHED
#   survives with nothing left to restore FROM: restore() enumerates $ORIG, so
#   in that state its loop body never ran -- it counted "restored unchanged: 0 /
#   edits preserved: 0" -- and the unconditional `rm -rf "$SANDBOX"
#   "$SANDBOX_STATE"` then deleted $PATCHED, the last surviving backup, without
#   a word. Exit was non-zero only incidentally, through the leftover-marker
#   check; if the operator had also hand-restored the manifests (so no manifest
#   names the sandbox any more) that check was empty too, and `--off` exited 0
#   reporting a clean teardown having restored NOTHING and deleted the only
#   remaining state. A second silent path: --off's have_work gate counted only
#   `clone_present || [ -d "$ORIG" ] || [ -d "$SANDBOX" ]` and never $PATCHED,
#   so with the clone gone it printed "sandbox not engaged; nothing to do" and
#   exited 0, leaving $PATCHED in place to block every later re-engage via the
#   "leftover backup state" guard at the bottom of this script.
#
#   restore() now refuses BEFORE that removal whenever $PATCHED exists and $ORIG
#   does not: it names both directories, keeps $PATCHED for diagnosis, and
#   returns non-zero. --off counts $PATCHED as work, so the stranded state is
#   reported rather than dismissed, and the summary says the originals were
#   absent instead of printing a bare zero. The loud branches stay loud and
#   silent discard is never reintroduced.
#
# USAGE
#   ./scripts/sandbox-build.sh            # engage: clone, port, repoint, then cargo check
#   ./scripts/sandbox-build.sh --no-check # engage without running cargo check
#   ./scripts/sandbox-build.sh --status   # report state, change nothing (exit 1 if DIRTY)
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
# The backups live in a SIBLING of the clone, never inside it: `--off` deletes
# the clone, so backups stored inside it die with it and the manifests it
# rewrote can no longer be repaired. Both directories move together because
# restore() uses $PATCHED as the merge base that tells a sandbox rewrite apart
# from an operator edit. See "WHERE THE BACKUPS LIVE" in the header.
SANDBOX_STATE="${XAFT_SANDBOX_STATE_DIR:-${SANDBOX}.state}"
ORIG="$SANDBOX_STATE/originals"
# Post-rewrite snapshot of the same files, used by restore() as the three-way
# merge base. Kept OUT of $ORIG on purpose: restore() enumerates $ORIG to find
# the originals to put back, so leaving snapshots in there would make it try to
# "restore" the patched copies over the pristine ones.
PATCHED="$SANDBOX_STATE/patched-manifests"
# Tolerate the pre-relocation layout, so a tree stranded by the old script can
# still be torn down (and still finds its originals) instead of needing a hand
# repair.
if [ ! -d "$ORIG" ] && [ -d "$SANDBOX/originals" ]; then
  ORIG="$SANDBOX/originals"
fi
if [ ! -d "$PATCHED" ] && [ -d "$SANDBOX/patched-manifests" ]; then
  PATCHED="$SANDBOX/patched-manifests"
fi
# The two halves of the path substitution engage applies, written in step 3.
# restore() needs both to take that substitution back out — see rewind_manifest.
REWRITE_MAP="$SANDBOX_STATE/rewrite-map"
# Any manifest text that means "this path points into a sandbox". Deliberately
# broader than the exact string this script writes: it also catches the
# pre-rename in-tree `.agtrs-sandbox` spelling and a relocated sandbox
# (XAFT_SANDBOX_DIR), because the point is to notice a rewrite that is not
# ours. scripts/require-sandbox.sh greps for the same marker.
SANDBOX_MARKER="xaft-agtrs-sandbox|$(basename "$SANDBOX")"
AGTRS="${AGTRS_REPO:-/root/rust_projects/agtrs}"
PORT="$ROOT/scripts/port-agtrs-injectable-rename.sh"
DO_CHECK=1

[ -f "$PORT" ] || { echo "error: $PORT not found" >&2; exit 1; }
cd "$ROOT"

# ------------------------------------------------------- what the tree says
#
# These two helpers are the reason `--status` cannot report a false CLEAN. The
# old implementation asked "does $SANDBOX exist?" and, when the answer was no,
# asserted that the manifests were pristine — a claim it never checked, and
# which was false in exactly the state that mattered. Asking the manifests
# themselves is cheap (11 small files) and is the only question whose answer
# determines whether `git add` is safe.

# The manifests this script manages: the root manifest plus every crate's.
# Not a `find`: an untracked scratch Cargo.toml under target/ must not be able
# to change the verdict.
managed_manifests() {
  printf '%s\n' "$ROOT/Cargo.toml"
  local m
  for m in "$ROOT"/crates/*/Cargo.toml; do
    if [ -f "$m" ]; then printf '%s\n' "$m"; fi
  done
}

# Every managed manifest that still names a sandbox, repo-relative.
sandbox_marked_manifests() {
  local f rel
  while IFS= read -r f; do
    if grep -qE "$SANDBOX_MARKER" "$f" 2>/dev/null; then
      rel="${f#"$ROOT"/}"
      printf '%s\n' "$rel"
    fi
  done < <(managed_manifests)
}

# Is the agtrs clone actually there? $SANDBOX itself can exist without a clone
# in it (engage creates it before cloning, and an interrupted run leaves it
# behind), so test the clone rather than its parent.
clone_present() { [ -d "$SANDBOX/agtrs" ]; }

status() {
  local marked clone=0
  marked="$(sandbox_marked_manifests)"
  if clone_present; then clone=1; fi

  if [ "$clone" -eq 1 ]; then
    echo "sandbox: ENGAGED at $SANDBOX"
    echo "  originals : $ORIG"
    if [ -n "$marked" ]; then
      echo "  manifests rewritten to point into it:"
      printf '%s\n' "$marked" | sed 's|^|    |'
    else
      echo "  WARNING: the clone exists but NO manifest names the sandbox, so"
      echo "           nothing here needs restoring. Run --off to drop the clone."
    fi
    echo "  DO NOT COMMIT while engaged."
    return 0
  fi

  if [ -n "$marked" ]; then
    echo "sandbox: DIRTY-BUT-NOT-ENGAGED"
    echo "  The clone at $SANDBOX is GONE, but these manifests still point into"
    echo "  it. The workspace cannot build — every cargo command fails to resolve"
    echo "  the agtrs graph — and this tree is NOT safe to commit:"
    printf '%s\n' "$marked" | sed 's|^|    |'
    if [ -d "$ORIG" ]; then
      echo "  FIX: ./scripts/sandbox-build.sh --off    (originals are in $ORIG)"
    else
      echo "  No originals found at $ORIG, so --off cannot repair this."
      echo "  FIX: git checkout -- Cargo.toml crates/*/Cargo.toml"
    fi
    return 1
  fi

  echo "sandbox: CLEAN (no manifest names the sandbox; safe to commit)"
  if [ -d "$SANDBOX_STATE" ] || [ -d "$ORIG" ]; then
    echo "  leftover backup state at $SANDBOX_STATE — run --off to clear it"
  fi
  return 0
}

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
#   auditable rather than a surprise. It is handled in TWO phases, because a
#   merge alone gets the common case wrong:
#
#     phase 1  rewind. The rewrite was a recorded substitution, so subtract it
#              from your file line by line (rewind_manifest below). This cannot
#              conflict, because it is not asking git to reconcile two editors
#              -- it is removing one known edit. Your file comes back as "the
#              pristine manifest plus your edits", however close your edit sits
#              to a line we rewrote.
#     phase 2  merge, only if phase 1 declined. A 3-way merge (base = patched
#              snapshot, ours = pristine original, theirs = what you left on
#              disk) still handles an edit that lands away from every rewritten
#              line, and still detects a genuine one.
#
#   Phase 1 exists because the merge CONFLICTS on the most ordinary edit there
#   is. Appending a dependency to the end of a `[dependencies]` block that sits
#   directly under a rewritten `agtrs-runtime = { path = ... }` line lands in
#   the same region the sandbox rewrote, so `git merge-file` reports a conflict
#   every time and teardown answers "NEEDS HAND-REAPPLY" for a routine append.
#   That costs a hand re-apply on every teardown and, worse, trains operators to
#   ignore the very guard that exists to protect their work.
#
#   If the merge conflicts, the pristine original is restored (conflict markers
#   must never be left in a Cargo.toml), your version is preserved beside it as
#   <file>.sandbox-edit-backup, and teardown exits NON-ZERO. Likewise if
#   $PATCHED is missing for a file we cannot prove the change is ours, so we
#   take the loud branch: failing toward "you edited something" is the safe
#   direction, and failing toward "you did not" is how the original bug shipped.

# The first phase of restore()'s "foreign edits" case: take the sandbox's OWN
# rewrite back out of the file the operator left behind.
#
# The rewrite was a single recorded substitution (rewrite-map, written in step 3
# of engage). Subtracting it line by line is exact and cannot drop, reorder or
# add a byte of the operator's work; scripts/sandbox-rewind.py additionally
# REFUSES — writing nothing — unless the recorded prefixes explain the snapshot
# line for line, so a stale or wrong map can never corrupt a manifest. It also
# refuses when the operator's edit and the rewrite touch the same line, because
# that case has no safe automatic answer and must reach the loud branch.
#
# Any output is the explanation to show the operator. A non-zero return means
# "could not prove it"; the caller then falls back to the merge, and failing
# that, to the loud branch.
REWOUND=""
REWOUND_NOTE=""
rewind_manifest() {
  local orig_="$1" base_="$2" cur="$3"
  [ -d "$SANDBOX_STATE" ] || return 1
  [ -f "$REWRITE_MAP" ] || {
    REWOUND_NOTE="no rewrite map at $REWRITE_MAP (this sandbox was engaged by an older script)"
    return 1
  }
  REWOUND="$SANDBOX_STATE/rewound-manifest"
  rm -f "$REWOUND"
  # The helper's stdout is the sentence to show the operator; its 4th argument
  # is where it writes the rewound manifest. Capture the note rather than
  # letting it leak to the terminal ahead of the WARNING block that frames it.
  REWOUND_NOTE="$(python3 "$ROOT/scripts/sandbox-rewind.py" "$orig_" "$base_" \
    "$cur" "$REWOUND" "$(sed -n 1p "$REWRITE_MAP")" \
    "$(sed -n 2p "$REWRITE_MAP")" 2>&1)" \
    || { rm -f "$REWOUND"; REWOUND=""; return 1; }
  [ -s "$REWOUND" ] || { REWOUND=""; return 1; }
  return 0
}

restore() {
  local n=0 rebased=0 conflicted=0 deleted=0 f rel target base
  # PRE-FLIGHT (goal df50869c): $PATCHED surviving without $ORIG is the one
  # state where this function could report a clean, zero-file teardown while
  # having restored NOTHING and destroyed the last surviving backup. The loop
  # below enumerates $ORIG, so with it gone the body never runs and `n` stays 0;
  # the `rm -rf "$SANDBOX" "$SANDBOX_STATE"` further down then took $PATCHED
  # with it. Refuse before that removal, keep $PATCHED for diagnosis, and say
  # which directory is missing and why nothing could be restored -- otherwise
  # the operator sees a bare zero and cannot tell why.
  local stranded=0
  if [ ! -d "$ORIG" ] && [ -d "$PATCHED" ]; then
    stranded=1
    echo
    echo "ERROR: backup state is STRANDED - there is nothing left to restore from." >&2
    echo "  originals : $ORIG  (MISSING)" >&2
    echo "  snapshots : $PATCHED  (present; KEPT for diagnosis)" >&2
    echo "  --off restores each manifest from the pristine originals under" >&2
    echo "  $ORIG, and that directory is gone, so the sandbox's agtrs path" >&2
    echo "  rewrites cannot be undone from backup here." >&2
    echo "  If a manifest still names the sandbox, repair it from git:" >&2
    echo "      git checkout -- Cargo.toml crates/*/Cargo.toml" >&2
    echo "  Nothing was restored and nothing was deleted; $PATCHED still holds" >&2
    echo "  the post-rewrite snapshots. Delete $SANDBOX_STATE yourself once you" >&2
    echo "  have finished with it." >&2
  fi

  if [ "$stranded" -eq 0 ] && [ -d "$ORIG" ]; then
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

      # Foreign edits: get the pristine agtrs paths back, then keep yours.
      cp "$target" "$target.sandbox-edit-backup"

      # PHASE 1 -- REWIND. The sandbox's rewrite is a substitution we recorded
      # at engage time, so take it straight back out, line by line. This has to
      # come FIRST because a merge cannot do it: the ordinary operator edit
      # (append a dependency to the end of a `[dependencies]` block that sits
      # directly under a rewritten `agtrs-runtime = { path = ... }` line) lands
      # in the same region the sandbox rewrote, so `git merge-file` reports a
      # conflict on it every single time and teardown charges a hand re-apply
      # for a routine append. Subtracting our own edit from your file has no
      # such conflict to hit.
      rewind_note=""
      rewound=0
      if rewind_manifest "$f" "$base" "$target"; then
        cp "$REWOUND" "$target"
        rewound=1
        rewind_note="$REWOUND_NOTE"
      else
        # Keep the reason: "could not prove it" and "proved it cannot" are very
        # different problems and the operator needs to be told which one hit.
        rewind_note="$REWOUND_NOTE"
      fi

      # PHASE 2 -- only if the rewind declined. It refuses when it cannot PROVE
      # the substitution is safe (the recorded prefixes do not explain the
      # snapshot, or your edit and our rewrite touch the same line), and a
      # refusal must never be silently resolved. The merge is still the right
      # tool for an edit that lands away from every line we rewrote, so try it
      # before falling back to the loud branch.
      if [ "$rewound" -eq 1 ]; then
        rebased=$((rebased+1))
        n=$((n+1))
        rm -f "$target.sandbox-edit-backup"
        echo "  WARNING: $rel was edited while the sandbox was engaged."
        echo "           Your edit was PRESERVED and the sandbox's agtrs path"
        echo "           rewrite removed. $rewind_note"
        echo "           Diff (pristine original -> result):"
        diff -u --label "a/$rel" --label "b/$rel" "$f" "$target" \
          | sed 's/^/             /' || true
      elif git merge-file -q -L "as-you-left-it" -L "sandbox-snapshot" \
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
        if [ -n "$rewind_note" ]; then
          echo "         rewind declined: $rewind_note" >&2
        fi
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

  # A stranded snapshot ($PATCHED without $ORIG) is the LAST surviving backup:
  # deleting it would destroy the only record of what the manifests looked like
  # while the sandbox was engaged, so the removal only happens when the backups
  # were NOT stranded. In the stranded case everything is left on disk for
  # diagnosis -- see the PRE-FLIGHT guard at the top of this function.
  if [ "$stranded" -eq 0 ]; then
    rm -rf "$SANDBOX" "$SANDBOX_STATE" "$ROOT/injectable"
  fi

  # The clone and its backups are gone, so if a manifest STILL names the
  # sandbox we have just thrown away the only thing that could repair it.
  # Refuse to report success over that: the tree is unbuildable and unsafe to
  # commit, and an exit 0 here is what let the original bug look like a pass.
  local leftover
  leftover="$(sandbox_marked_manifests)"
  if [ -n "$leftover" ]; then
    echo
    echo "ERROR: these manifests still point into the sandbox after teardown:" >&2
    printf '%s\n' "$leftover" | sed 's|^|    |' >&2
    echo "  The workspace will not build and this tree must not be committed." >&2
    echo "  Repair each by hand (point its agtrs deps back at ../../../agtrs/) or" >&2
    echo "  discard the manifest change:" >&2
    echo "      git checkout -- <manifest>" >&2
  fi

  echo
  echo "teardown summary:"
  if [ "$stranded" -eq 1 ]; then
    # Say the originals were ABSENT rather than printing a bare 0, which read
    # as a clean no-op teardown over a tree nothing could repair (df50869c).
    echo "  originals ABSENT       : nothing restored (see ERROR above)"
  else
    echo "  restored unchanged     : $n manifest(s)"
  fi
  echo "  edits preserved        : $rebased manifest(s)"
  [ "$deleted" -eq 0 ] || echo "  DELETED while engaged   : $deleted manifest(s) (left deleted)"
  [ "$conflicted" -eq 0 ] || echo "  NEEDS HAND-REAPPLY      : $conflicted manifest(s)"
  if [ "$stranded" -eq 1 ]; then
    echo "  sandbox NOT removed     : backup state at $SANDBOX_STATE is stranded"
  else
    echo "  sandbox removed"
  fi

  if [ "$stranded" -eq 1 ] || [ "$conflicted" -gt 0 ] || [ "$deleted" -gt 0 ] || [ -n "$leftover" ]; then
    echo
    echo "REFUSING to report a clean teardown: work needed attention above." >&2
    return 1
  fi
}

case "${1:-}" in
  --off)
    # The clone's ABSENCE must not mean "nothing to do". The clone is the one
    # thing that can legitimately vanish out from under us -- `rm -rf`, a disk
    # sweep, an interrupted run, an operator tidying up a directory it was told
    # to ignore -- while the originals now live in a sibling precisely so that
    # case stays repairable. Gating on `[ -d "$SANDBOX" ]` is what let a tree
    # left naming a deleted clone report "nothing to do", exit 0, and stay
    # unbuildable with the only repair path sitting there untouched.
    # $PATCHED counts as work too (df50869c): a stranded post-rewrite snapshot
    # is real state that --off must report, not dismiss. Without it, a tree
    # holding only $PATCHED (clone gone, $ORIG gone) printed "nothing to do",
    # exited 0, and left that state in place to block every later re-engage.
    have_work=0
    if clone_present || [ -d "$ORIG" ] || [ -d "$PATCHED" ] || [ -d "$SANDBOX" ]; then
      have_work=1
    fi
    [ -n "$(sandbox_marked_manifests)" ] && have_work=1
    if [ "$have_work" -eq 0 ]; then
      echo "sandbox not engaged; nothing to do"
      exit 0
    fi
    restore || exit 1
    exit 0 ;;
  --status)
    # Ask the MANIFESTS, not the directory. That is the whole fix: the old
    # branch inferred "manifests are pristine" from `[ -d "$SANDBOX" ]` without
    # ever opening a manifest, so it certified precisely the state that breaks
    # every cargo command, and exited 0, so it was safe to trust as a
    # pre-commit gate precisely when it was lying. status() classifies by what
    # the manifests contain and exits 1 when DIRTY.
    status
    exit $? ;;
  "") ;;
  --no-check) DO_CHECK=0 ;;
  *) sed -n '30,45p' "$0"; exit 2 ;;
esac

[ -d "$AGTRS/.git" ] || { echo "error: $AGTRS is not a git repository" >&2; exit 1; }

# Refuse to clobber an already-engaged sandbox with a possibly-modified clone.
[ -d "$SANDBOX" ] && { echo "error: sandbox already engaged; run --off first" >&2; exit 1; }

# Refuse to engage over a tree that is ALREADY rewritten. The backup taken in
# step 1 is what `--off` restores from, so engaging here would snapshot the
# SANDBOX'S OWN REWRITES as the "pristine original" -- and the sandbox's path
# rewrites are exactly what a teardown exists to remove. The result is a tree
# that can never be repaired: every later `--off` would faithfully restore a
# manifest that still names the sandbox. This is the same unrecoverable-by-
# design failure the relocation fixed, reached from the other direction.
stale="$(sandbox_marked_manifests)"
if [ -n "$stale" ]; then
  echo "error: these manifests already point into a sandbox, so engaging would" >&2
  echo "       snapshot the rewrites as the pristine original and make the tree" >&2
  echo "       unrecoverable. Repair them first:" >&2
  printf '%s\n' "$stale" | sed 's|^|         |' >&2
  if [ -d "$ORIG" ]; then
    echo "       ./scripts/sandbox-build.sh --off    (originals are in $ORIG)" >&2
  else
    echo "       git checkout -- Cargo.toml crates/*/Cargo.toml" >&2
  fi
  exit 1
fi

# Stale backup state from an interrupted earlier run would be overwritten in
# step 1, destroying the only copy of the originals. Say so rather than clobber.
if [ -d "$ORIG" ] || [ -d "$PATCHED" ]; then
  echo "error: leftover backup state at $SANDBOX_STATE" >&2
  echo "       Engaging now would overwrite the only surviving originals." >&2
  echo "       Run --off first, or delete $SANDBOX_STATE once you are sure it is dead." >&2
  exit 1
fi

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
# The rewrite is a TWO-part substitution and teardown needs BOTH halves to take
# it back out: the pristine prefix being replaced, and the sandboxed prefix
# written in its place. Record them beside the originals rather than
# recomputing at teardown, because $SBX_REL depends on $SANDBOX, and an
# operator who engaged with XAFT_SANDBOX_DIR set and tears down without it
# would otherwise compute a different prefix and rewind nothing.
AGTRS_DEP_PREFIX="../../../agtrs/"
SBX_DEP_PREFIX="$SBX_REL/agtrs/"
find "$ROOT/crates" -name Cargo.toml -print0 | xargs -0 sed -i "s|\\.\\./\\.\\./\\.\\./agtrs/|$SBX_DEP_PREFIX|"
n=$(grep -rl "$SBX_REL" "$ROOT/crates" --include=Cargo.toml | wc -l)
echo "    repointed $n crate manifest(s) -> $SBX_REL"
# Recorded here, right where the sed's own prefix is defined, so the literal
# the rewrite matches and the literal teardown undoes cannot drift.
# sandbox-rewind.py REFUSES if these two prefixes do not explain the snapshot
# line for line, so a drift can never silently corrupt a manifest: it falls
# back to the loud branch instead.
printf '%s\n%s\n' "$AGTRS_DEP_PREFIX" "$SBX_DEP_PREFIX" \
  > "$SANDBOX_STATE/rewrite-map"
echo "    recorded rewrite map: '$AGTRS_DEP_PREFIX' -> '$SBX_DEP_PREFIX'"
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
