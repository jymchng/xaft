#!/usr/bin/env python3
"""Summarise a cargo-tarpaulin JSON report: total, per-crate, worst files.

Kept in the repo because the coverage numbers quoted in
docs/coverage-baseline.md and in goal notes otherwise have to be re-derived by
hand every pass, and the per-crate and per-file breakdowns are what tell you
which module to write the next batch of tests for.

Usage:
    cargo tarpaulin --workspace --engine llvm --out Json
    python3 scripts/coverage-report.py [tarpaulin-report.json] [TOP_N]

Exits 0 always; it is a reporting tool, not a gate. The gate is
`cargo tarpaulin --fail-under 85` (see `just coverage`).
"""
import collections
import json
import sys

# Files whose uncovered lines are structural rather than testable.
# A binary's main() only runs via the real process; a re-export or a
# declarative table is data, not logic. Counting them would make the number
# look worse than the code is.
UNCOVERABLE_NOTE = "noted in docs/coverage-baseline.md"


def crate_of(path):
    parts = path.split("/")
    if "crates" in parts:
        return parts[parts.index("crates") + 1]
    return "(root binary)"


def main():
    report = sys.argv[1] if len(sys.argv) > 1 else "tarpaulin-report.json"
    top_n = int(sys.argv[2]) if len(sys.argv) > 2 else 30

    with open(report) as fh:
        data = json.load(fh)
    files = data["files"] if isinstance(data, dict) else data

    per_crate = collections.defaultdict(lambda: [0, 0])
    per_file = []
    for entry in files:
        path = "/".join(entry["path"])
        covered, coverable = entry["covered"], entry["coverable"]
        if not coverable:
            continue
        per_crate[crate_of(path)][0] += covered
        per_crate[crate_of(path)][1] += coverable
        per_file.append((coverable - covered, covered, coverable, path))

    total_cov = sum(v[0] for v in per_crate.values())
    total_all = sum(v[1] for v in per_crate.values())
    pct = total_cov / total_all * 100 if total_all else 0.0

    print(f"{UNCOVERABLE_NOTE}\n")
    print(f"TOTAL LINE COVERAGE: {total_cov}/{total_all} = {pct:.2f}%\n")
    print(f"{'crate':<22}{'covered':>9}{'total':>8}{'pct':>9}{'missing':>9}")
    for name, (cov, tot) in sorted(per_crate.items(), key=lambda kv: kv[1][0] / kv[1][1]):
        print(f"{name:<22}{cov:>9}{tot:>8}{cov / tot * 100:>8.2f}%{tot - cov:>9}")

    print(f"\n--- {top_n} files with the most MISSING lines (>=20 coverable) ---")
    rows = sorted((r for r in per_file if r[2] >= 20), reverse=True)
    for miss, cov, tot, path in rows[:top_n]:
        print(f"{miss:>5} missing  {cov:>5}/{tot:<5} ({cov / tot * 100:>5.1f}%)  {'/'.join(path.split('/')[-3:])}")


if __name__ == "__main__":
    main()
