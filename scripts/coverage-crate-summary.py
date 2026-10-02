#!/usr/bin/env python3
"""Summarise a tarpaulin JSON report as per-crate covered/coverable counts.

Used to refresh the tables in docs/coverage-baseline.md from a real
measurement instead of hand-edited numbers.

The report shape is `{"files": [...], "coverage": float, "covered": int,
"coverable": int}` where each file entry carries a `path` list of path
components and a `line_coverage` map of line number -> hit count. Lines
absent from that map are not coverable.
"""
import json
import os
import re
import sys
from collections import defaultdict

REPORT = sys.argv[1] if len(sys.argv) > 1 else "tarpaulin-report.json"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

with open(REPORT) as fh:
    data = json.load(fh)

if isinstance(data, dict):
    files = data.get("files", [])
else:  # older/alternate: a flat list
    files = data

by_crate = defaultdict(lambda: [0, 0])  # crate -> [covered, coverable]
rows = []

for entry in files:
    raw = entry.get("path")
    if isinstance(raw, list):
        path = "/".join(str(p) for p in raw)
    else:
        path = str(raw or entry.get("filename") or "")
    rel = os.path.relpath(path, ROOT) if path.startswith("/") else path
    if not rel.startswith("crates/"):
        continue
    m = re.match(r"crates/([^/]+)/", rel)
    if not m:
        continue
    crate = m.group(1)
    # Tarpaulin emits per-file covered/coverable counts directly. The
    # `traces` list only records *executed* lines, so it cannot be used to
    # recover the coverable denominator.
    if "coverable" in entry:
        covered = int(entry.get("covered", 0))
        coverable = int(entry["coverable"])
    else:
        lines = entry.get("line_coverage") or {}
        covered = sum(1 for hits in lines.values() if hits)
        coverable = len(lines)
    by_crate[crate][0] += covered
    by_crate[crate][1] += coverable
    if coverable:
        rows.append((coverable - covered, covered, coverable, rel))

tot_c = sum(v[0] for v in by_crate.values())
tot_t = sum(v[1] for v in by_crate.values())

print(f"{'crate':<22} {'covered':>8} {'coverable':>10} {'pct':>8} {'missing':>8}")
for crate in sorted(
    by_crate, key=lambda c: (by_crate[c][0] / by_crate[c][1]) if by_crate[c][1] else 0
):
    cov, tot = by_crate[crate]
    pct = (100.0 * cov / tot) if tot else 0.0
    print(f"{crate:<22} {cov:>8} {tot:>10} {pct:>7.2f}% {tot - cov:>8}")
print(
    f"{'WORKSPACE TOTAL':<22} {tot_c:>8} {tot_t:>10} "
    f"{100.0 * tot_c / tot_t:>7.2f}% {tot_t - tot_c:>8}"
)

required = int(0.85 * tot_t + 0.999999)
print()
print(f"required for 85%: {required}   gap: {required - tot_c}")
print()
print("--- largest gaps ---")
print(f"{'missing':>8} {'cov/tot':>12}  file")
for miss, cov, tot, path in sorted(rows, reverse=True)[:25]:
    print(f"{miss:>8} {f'{cov}/{tot}':>12}  {path}")
