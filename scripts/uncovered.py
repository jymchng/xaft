#!/usr/bin/env python3
"""Print the UNCOVERED source lines for one or more files from a tarpaulin JSON report.

    python3 scripts/uncovered.py crates/xaft-tui/src/approval.rs
    python3 scripts/uncovered.py --uncovered-only crates/xaft-tui/src/state.rs | head

Tarpaulin writes `tarpaulin-report.json` into the repo root and it is
gitignored, so a STALE copy silently survives and will contradict a fresh
run. Delete it before measuring:

    rm -f tarpaulin-report.json
"""
from __future__ import annotations

import argparse
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load() -> dict[str, tuple[int, int, list[int]]]:
    path = os.path.join(REPO, "tarpaulin-report.json")
    if not os.path.exists(path):
        sys.exit(
            f"no report at {path}\n"
            "hint: rm -f tarpaulin-report.json && cargo tarpaulin --workspace "
            "--engine llvm --out Json"
        )
    with open(path) as fh:
        data = json.load(fh)
    out: dict[str, tuple[int, int, list[int]]] = {}
    for entry in data.get("files", []):
        raw = entry.get("path")
        if isinstance(raw, list):
            raw = os.path.join(*[str(p) for p in raw])
        rel = os.path.relpath(str(raw), REPO)
        # tarpaulin's `traces` is a list of {line, stats: {Line: hits}}.
        traces = entry.get("traces") or []
        missed: list[int] = []
        for tr in traces:
            if isinstance(tr, dict):
                hits = (tr.get("stats") or {}).get("Line", 0)
                if not hits:
                    missed.append(int(tr["line"]))
            elif not tr:
                missed.append(0)
        missed.sort()
        out[rel] = (entry.get("covered", 0), entry.get("coverable", 0), missed)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("files", nargs="+", help="repo-relative source paths")
    ap.add_argument(
        "--uncovered-only",
        action="store_true",
        help="print only the missed source lines, no header",
    )
    args = ap.parse_args()

    report = load()
    unknown: list[str] = []
    for target in args.files:
        key = next(
            (k for k in report if k.endswith(target) or target.endswith(k)),
            None,
        )
        if key is None:
            unknown.append(target)
            continue
        covered, coverable, missed = report[key]
        if args.uncovered_only:
            src = open(os.path.join(REPO, key), encoding="utf-8", errors="replace")
            lines = src.read().split("\n")
            for line in missed:
                if 1 <= line <= len(lines):
                    print(f"{line:5d}| {lines[line - 1]}")
            continue
        pct = 100.0 * covered / coverable if coverable else 0.0
        print(f"{key}: {covered}/{coverable} = {pct:.2f}%  ({len(missed)} missed)")
        src = open(os.path.join(REPO, key), encoding="utf-8", errors="replace")
        lines = src.read().split("\n")
        for line in missed:
            if 1 <= line <= len(lines):
                print(f"  {line:5d}| {lines[line - 1]}")

    for target in unknown:
        print(f"error: {target} is not in the report", file=sys.stderr)
    return 1 if unknown else 0


if __name__ == "__main__":
    raise SystemExit(main())
