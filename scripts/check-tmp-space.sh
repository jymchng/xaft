#!/usr/bin/env bash
#
# check-tmp-space.sh — turn a mysterious doctest link failure into a
#                      one-line, actionable diagnosis.
#
# WHY THIS EXISTS
#   rustc compiles every doctest binary under TMPDIR. When TMPDIR (or /tmp)
#   runs out of space, the failure surfaces as a LINKER error attached to a
#   NAMED doctest, which reads exactly like a regression in working code:
#
#       test crates/xaft-cli/src/jobs/mod.rs - jobs::JobStore (line 357) ... FAILED
#       error: linking with 'cc' failed: exit status: 1
#       LLVM ERROR: IO failure on output stream: No space left on device
#
#   Measured on this host (2026-10): /tmp is a 12G tmpfs that other projects
#   fill with scratch directories. At ~586M free the same doctest passed; with
#   a few hundred MB less it failed to link. The code was identical in both
#   runs. So the tempting "fix" — editing the doctest, adding #[ignore], or
#   marking it no_run — damages a healthy tree to chase an environmental fault.
#
# WHAT IT CHECKS
#   Free space on the filesystem holding the effective temp directory
#   ($TMPDIR, or /tmp when unset), in three steps:
#
#     1. the directory exists or can be created
#     2. it is writable
#     3. it has at least XAFT_TMP_MIN_KIB free (default 256 MiB)
#
#   Step 3 is the one that fires. 256 MiB is chosen from the measurement above:
#   the doctest link succeeded at 586M free and failed somewhere below a few
#   hundred MB, so the threshold sits under the observed working point and well
#   above the observed failure point.
#
# THE THRESHOLDS ARE OVERRIDABLE so this script can be self-tested without
# mounting anything, and so a caller can demand more headroom than the default:
#
#     XAFT_TMP_MIN_KIB    hard floor in KiB   (default 262144 = 256 MiB)
#     XAFT_TMP_WARN_KIB   warn below in KiB    (default 2097152 = 2 GiB)
#
# WHAT IT DOES NOT DO
#   It never deletes anything. The bulk under a full /tmp on a shared host
#   belongs to OTHER projects (/tmp/vcheck, /tmp/netlify-sim, ...) and deleting
#   it is not xaft's call. The remedy is to redirect xaft's own temp usage,
#   which is what the justfile's `export TMPDIR` does for every recipe.
#
# USAGE
#   scripts/check-tmp-space.sh              check, exit 1 if the floor is unmet
#   scripts/check-tmp-space.sh --quiet      same, but no output when healthy
#   scripts/check-tmp-space.sh --self-test  exercise the decision logic
#
# EXIT CODES
#   0  enough space (a warning may have been printed to stderr)
#   1  below the floor, unwritable, or uncreatable — with the diagnosis
#   2  bad usage
#
set -euo pipefail

MIN_KIB="${XAFT_TMP_MIN_KIB:-262144}"
WARN_KIB="${XAFT_TMP_WARN_KIB:-2097152}"

QUIET=0

# ── helpers ───────────────────────────────────────────────────────────────────

mib() { awk -v k="$1" 'BEGIN { printf "%d", k / 1024 }'; }

# Classify free KiB against explicit thresholds. Pure and positional, so
# --self-test can drive every branch without touching the globals. (Do not
# switch this to `VAR=x classify ...`: bash applies a prefix assignment to a
# shell function for the REST OF THE SHELL, not just the call, which silently
# rewrites the defaults for whatever runs next. That was the first bug this
# self-test caught.)
# Echoes one of: ok | warn | full
classify() { # classify <free_kib> <min_kib> <warn_kib>
  if [ "$1" -lt "$2" ]; then echo full
  elif [ "$1" -lt "$3" ]; then echo warn
  else echo ok
  fi
}

# Free KiB on the filesystem holding $1. Empty on failure.
free_kib() {
  df -Pk "$1" 2>/dev/null | awk 'NR == 2 { print $4 }'
}

effective_tmpdir() {
  if [ -n "${TMPDIR:-}" ]; then printf '%s' "$TMPDIR"; else printf '/tmp'; fi
}

# ── diagnosis ─────────────────────────────────────────────────────────────────

diagnose() {
  local dir="$1" reason="$2" free_kib="${3:-}"

  {
    echo "error: not enough scratch space to build xaft — this is NOT a code defect."
    echo
    echo "  TMPDIR      : $dir"
    if [ -n "$free_kib" ]; then
      echo "  free space  : $(mib "$free_kib") MiB  (need at least $(mib "$MIN_KIB") MiB)"
    else
      echo "  free space  : unknown ($reason)"
    fi
    echo "  threshold   : XAFT_TMP_MIN_KIB=$MIN_KIB"
    echo
    echo "  WHY"
    echo "    rustc compiles every doctest binary under TMPDIR. When that"
    echo "    filesystem fills up, the link step dies and cargo attributes the"
    echo "    failure to the doctest it was building, so a healthy test reads:"
    echo
    echo "        test crates/xaft-cli/src/jobs/mod.rs - jobs::JobStore (line 357) ... FAILED"
    echo "        error: linking with 'cc' failed: exit status: 1"
    echo "        LLVM ERROR: IO failure on output stream: No space left on device"
    echo
    echo "    That is an exhausted filesystem wearing a test's name. Do NOT edit"
    echo "    the doctest, do NOT add #[ignore] or no_run to it, and do NOT"
    echo "    delete other projects' scratch data to make room."
    echo
    echo "  FIX — redirect xaft's own temp usage to a larger filesystem:"
    echo
    echo "      export TMPDIR=/var/tmp/xaft-tmp"
    echo "      mkdir -p \"\$TMPDIR\""
    echo
    echo "    The justfile already does this: it exports TMPDIR=/var/tmp/xaft-tmp"
    echo "    for every recipe unless you set TMPDIR yourself, so 'just test' and"
    echo "    'just coverage' are covered. This bites bare 'cargo' invocations,"
    echo "    which is where you saw it."
  } >&2

  # Best-effort: show what is actually consuming the space. Bounded, because a
  # deep du over a full tmpfs is exactly the kind of slow thing that must not
  # run on every failed build. Everything goes to stderr so the whole
  # diagnosis is one stream.
  if [ -d "$dir" ]; then
    local entries
    entries="$(timeout 10 du -x -h -d 1 "$dir" 2>/dev/null \
      | awk -v d="$dir" '$2 != d' | sort -rh | head -12)" || entries=""
    echo >&2
    if [ -n "$entries" ]; then
      echo "  Largest entries under $dir (delete only what belongs to YOU):" >&2
      printf '%s\n' "$entries" | sed 's/^/    /' >&2
    else
      echo "  Nothing sizeable found directly under $dir — the filesystem" >&2
      echo "  itself is exhausted; inspect it by hand." >&2
    fi
  fi
}

# ── main check ────────────────────────────────────────────────────────────────

check() {
  local dir
  dir="$(effective_tmpdir)"

  if [ ! -d "$dir" ]; then
    if ! mkdir -p "$dir" 2>/dev/null; then
      diagnose "$dir" "directory does not exist and could not be created"
      return 1
    fi
  fi

  if [ ! -w "$dir" ]; then
    diagnose "$dir" "directory is not writable by $(id -un 2>/dev/null || echo 'this user')"
    return 1
  fi

  local free
  free="$(free_kib "$dir")"
  if [ -z "$free" ]; then
    diagnose "$dir" "df could not report free space"
    return 1
  fi

  case "$(classify "$free" "$MIN_KIB" "$WARN_KIB")" in
    full)
      diagnose "$dir" "below XAFT_TMP_MIN_KIB" "$free"
      return 1
      ;;
    warn)
      if [ "$QUIET" -eq 0 ]; then
        echo "warning: only $(mib "$free") MiB free on $dir (the temp directory rustc" >&2
        echo "         links doctest binaries in). Under $(mib "$WARN_KIB") MiB this has" >&2
        echo "         been enough to fail a link step with a misleading ENOSPC." >&2
        echo "         If a doctest fails to link, set TMPDIR to a larger filesystem:" >&2
        echo "             export TMPDIR=/var/tmp/xaft-tmp" >&2
      fi
      ;;
    ok)
      if [ "$QUIET" -eq 0 ]; then
        echo "tmp-space: ok — $(mib "$free") MiB free on $dir"
      fi
      ;;
  esac

  return 0
}

# ── self-test ─────────────────────────────────────────────────────────────────
#
# Exercises the classifier, which is the whole decision. It deliberately does
# NOT mount a filesystem: a checker that cannot fail is worse than no checker,
# and the thresholds are overridable precisely so the branches can be reached
# from here. The real-space path is covered by scripts/check-tmp-space.sh's
# callers (just tmp-check) running against a deliberately full TMPDIR.

self_test() {
  local failed=0

  expect() { # expect <label> <expected-class> <free_kib> <min_kib> <warn_kib>
    local got
    got="$(classify "$3" "$4" "$5")"
    if [ "$got" = "$2" ]; then
      echo "  ok   $1 -> $got"
    else
      echo "  FAIL $1 -> $got (expected $2)"
      failed=1
    fi
  }

  # Defaults are 256 MiB / 2 GiB, so 262144 and 2097152 KiB.
  expect "plenty of space"        ok   4194304 262144 2097152  # 4G
  expect "just above the floor"   ok     300000 262144  300000  # warn pulled to meet it
  expect "above floor, under warn" warn  300000 262144 2097152  # correct: still low
  expect "between floor and warn" warn  1048576 262144 2097152  # 1G
  expect "exactly at warn"        ok    2097152 262144 2097152  # not *below* warn
  expect "exactly at the floor"   warn   262144 262144 2097152  # not *below* floor
  expect "one KiB under floor"    full   262143 262144 2097152
  expect "no space at all"        full         0 262144 2097152

  # A caller demanding different headroom must be honoured, not clamped. Each
  # of these lands in a DIFFERENT band under the default thresholds, which is
  # what makes them prove the override was actually read rather than ignored.
  expect "raised floor is used"   full   500000 600000  700000  # default -> warn
  expect "lowered floor is used"  ok       1000      0    1000  # default -> warn
  expect "raised warn is used"    warn  3000000 262144 3097152  # default -> ok

  # The whole script, driven end to end through its real exit codes.
  if check-tmpdir "$PWD" 1 1 >/dev/null 2>&1; then
    echo "  ok   a satisfiable floor passes"
  else
    echo "  FAIL a satisfiable floor passes"
    failed=1
  fi

  if check-tmpdir "$PWD" 999999999999 999999999999 >/dev/null 2>&1; then
    echo "  FAIL an unsatisfiable floor fails"
    failed=1
  else
    echo "  ok   an unsatisfiable floor fails"
  fi

  # A directory that cannot exist and cannot be created must fail, not pass.
  # /proc is a kernel filesystem: nothing can be mkdir'd under it.
  if check-tmpdir "/proc/xaft-tmp-space-should-not-exist" 1 1 >/dev/null 2>&1; then
    echo "  FAIL an uncreatable temp directory fails"
    failed=1
  else
    echo "  ok   an uncreatable temp directory fails"
  fi

  # A satisfied-but-unwritable directory must fail. Running as root defeats a
  # mode-based check, so this branch is only asserted when we can honour it.
  local ro
  ro="$(mktemp -d)" || { echo "  FAIL could not mktemp"; exit 1; }
  chmod 500 "$ro"
  if [ -w "$ro" ]; then
    echo "  skip unwritable-directory check (running with override privileges)"
  elif check-tmpdir "$ro" 1 1 >/dev/null 2>&1; then
    echo "  FAIL an unwritable temp directory fails"
    failed=1
  else
    echo "  ok   an unwritable temp directory fails"
  fi
  chmod 700 "$ro"
  rm -rf "$ro"

  if [ "$failed" -eq 0 ]; then
    echo "self-test: all checks passed"
    return 0
  fi
  echo "self-test: FAILURES above"
  return 1
}

# check-tmpdir <dir> <min_kib> <warn_kib> — the same check against an explicit
# directory and thresholds, so the self-test can drive it without mutating TMPDIR
# or the globals. `local` keeps the override properly scoped.
check-tmpdir() {
  local saved_tmp="${TMPDIR-__unset__}" saved_min="$MIN_KIB" saved_warn="$WARN_KIB"
  TMPDIR="$1"
  MIN_KIB="$2"
  WARN_KIB="$3"
  local rc=0
  check || rc=$?
  if [ "$saved_tmp" = "__unset__" ]; then unset TMPDIR; else TMPDIR="$saved_tmp"; fi
  MIN_KIB="$saved_min"
  WARN_KIB="$saved_warn"
  return $rc
}

# ── entry point ───────────────────────────────────────────────────────────────

case "${1:-}" in
  --self-test)
    QUIET=1
    self_test
    ;;
  --quiet|-q)
    QUIET=1
    shift
    [ $# -eq 0 ] || { echo "usage: $0 [--quiet|--self-test]" >&2; exit 2; }
    check
    ;;
  --help|-h)
    sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; $d'
    ;;
  "")
    check
    ;;
  *)
    echo "usage: $0 [--quiet|--self-test]" >&2
    exit 2
    ;;
esac