#!/usr/bin/env python3
"""
xaft-members.py — print this workspace's member packages, for `just`.

WHY
    `cargo fmt --all` does NOT mean "format this workspace's crates". cargo-fmt
    enumerates PATH DEPENDENCIES as well as workspace members, and runs
    `cargo metadata --no-deps --manifest-path <pkg>` for each one it finds.

    With the agtrs build sandbox engaged (scripts/sandbox-build.sh) xaft
    path-depends on ten crates inside a throwaway clone under
    /root/rust_projects/.xaft-agtrs-sandbox. So `cargo fmt --all` formats those
    too, which is wrong for two reasons:

      * it is a THROWAWAY clone. Reformatting it is wasted work, and it
        makes the clone diverge from the agtrs repo it was cloned from;
      * `just fmt-check` would then go RED because a repository we do not
        control is not rustfmt-clean under the locally installed rustfmt.
        Nothing in xaft can fix that, and no xaft change caused it.

    Worse, it makes the gate mean different things in different places. CI has
    no sandbox, so CI's `cargo fmt --all` checks exactly the 11 xaft members.
    A local run with the sandbox engaged checks 21. A green CI and a red local
    fmt-check would then have entirely different meanings -- the same
    local-vs-CI divergence that the CI coverage gate already has.

    Passing `-p <member>` for each member scopes the run to xaft's own crates
    in every environment. The list is read from cargo rather than hardcoded, so
    a newly added crate cannot silently escape formatting.

USAGE
    scripts/xaft-members.py            # `-p xaft -p xaft-cli ...`, for cargo
    scripts/xaft-members.py --names    # bare names, one per line
    scripts/xaft-members.py --check    # exit 0 iff at least one member exists

EXIT CODES
    0  members resolved
    1  `cargo metadata` failed, or the workspace reports zero members -- both
       are treated as hard errors, because a silently EMPTY list would turn
       `just fmt` into a no-op that reports success while formatting nothing.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys


def workspace_members() -> list[str]:
    try:
        out = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            capture_output=True,
            text=True,
            check=True,
        )
    except FileNotFoundError:
        sys.exit("xaft-members.py: `cargo` is not on PATH")
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr)
        sys.exit(f"xaft-members.py: `cargo metadata --no-deps` failed "
                 f"(exit {exc.returncode})")

    try:
        meta = json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.exit("xaft-members.py: `cargo metadata` did not return valid JSON")

    names = [p["name"] for p in meta.get("packages", [])]
    if not names:
        # A path dependency is a package here too, so this means the manifest
        # resolved to nothing at all. Formatting zero crates while exiting 0 is
        # the failure mode this guard exists to prevent.
        sys.exit("xaft-members.py: workspace reports zero members")
    return sorted(names)


def main() -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("--names", action="store_true",
                    help="print bare package names instead of cargo -p flags")
    ap.add_argument("--check", action="store_true",
                    help="exit 0 if at least one member resolved")
    args = ap.parse_args()

    members = workspace_members()

    if args.check:
        print(f"xaft workspace members: {len(members)}")
        for name in members:
            print(f"  {name}")
        return 0

    if args.names:
        print(" ".join(members))
    else:
        print(" ".join(f"-p {name}" for name in members))
    return 0


if __name__ == "__main__":
    sys.exit(main())
