#!/usr/bin/env python3
"""Summarise a tarpaulin JSON report as per-crate covered/coverable counts.

Used to refresh the tables in docs/coverage-baseline.md from a real
measurement instead of hand-edited numbers.

The report shape is `{"files": [...], "coverage": float, "covered": int,
"coverable": int}` where each file entry carries a `path` list of path
components and a `line_coverage` map of line number -> hit count. Lines
absent from that map are not coverable.

WHAT COUNTS AS A CRATE
    * `crates/<name>/...`  -> `<name>`
    * `src/...` at the repository root -> the `xaft` binary crate.

The second rule is not cosmetic. The `xaft` binary crate lives at the
repository root, not under `crates/`, so an earlier revision of this script
dropped its four files (`src/main.rs`, `src/lifecycle.rs`, `src/query.rs` and
an empty `src/lib.rs`) and printed a `WORKSPACE TOTAL` that silently
contradicted the report's own top-level `covered`/`coverable` fields. That is
the exact defect the per-crate table in docs/coverage-baseline.md was
hand-fixed for, so the generator must not reintroduce it.

SELF-CHECK
    The derived rows are compared against the report's own top-level
    `covered`/`coverable`. They must match byte-for-byte; a dropped or
    unclassified file therefore exits non-zero instead of quietly shrinking
    the total. When the report carries no top-level counts (an older/alternate
    shape) the check is skipped with a note.

    python3 scripts/coverage-crate-summary.py tarpaulin-report.json
    echo $?   # 0 when the rows sum to the report's own total, 1 when they do not
"""
import json
import os
import re
import sys
from collections import defaultdict

REPORT = sys.argv[1] if len(sys.argv) > 1 else "tarpaulin-report.json"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The root binary crate: `xaft` itself, whose sources are `src/*.rs` at the
# repository root rather than under `crates/`. Named to match the per-crate
# table in docs/coverage-baseline.md.
BIN_CRATE = "xaft (bin)"

with open(REPORT) as fh:
    data = json.load(fh)

if isinstance(data, dict):
    files = data.get("files", [])
else:  # older/alternate: a flat list
    files = data

by_crate = defaultdict(lambda: [0, 0])  # crate -> [covered, coverable]
rows = []
dropped = []

for entry in files:
    raw = entry.get("path")
    if isinstance(raw, list):
        path = "/".join(str(p) for p in raw)
    else:
        path = str(raw or entry.get("filename") or "")
    rel = os.path.relpath(path, ROOT) if path.startswith("/") else path
    if rel.startswith("crates/"):
        m = re.match(r"crates/([^/]+)/", rel)
        if not m:
            dropped.append(rel)
            continue
        crate = m.group(1)
    elif rel.startswith("src/"):
        # The `xaft` binary crate at the repository root.
        crate = BIN_CRATE
    else:
        dropped.append(rel)
        continue
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
    f"{100.0 * tot_c / tot_t if tot_t else 0.0:>7.2f}% {tot_t - tot_c:>8}"
)

required = int(0.85 * tot_t + 0.999999)
print()
print(f"required for 85%: {required}   gap: {required - tot_c}")
print()
print("--- largest gaps ---")
print(f"{'missing':>8} {'cov/tot':>12}  file")
for miss, cov, tot, path in sorted(rows, reverse=True)[:25]:
    print(f"{miss:>8} {f'{cov}/{tot}':>12}  {path}")

# ── self-check ───────────────────────────────────────────────────────────────
# The rows must reproduce the report's own top-level totals. If they do not, a
# file was dropped or fell outside the two classification rules above, and the
# per-crate table would silently contradict the workspace total.
report_c = data.get("covered") if isinstance(data, dict) else None
report_t = data.get("coverable") if isinstance(data, dict) else None

print()
if report_c is None or report_t is None:
    print(
        "self-check: SKIPPED — the report carries no top-level "
        "`covered`/`coverable` to compare against"
    )
    sys.exit(0)

problems = []
if tot_c != int(report_c) or tot_t != int(report_t):
    problems.append(
        f"derived rows total {tot_c}/{tot_t} but the report reports "
        f"{int(report_c)}/{int(report_t)}"
    )
if dropped:
    problems.append(
        "unclassified file(s) outside `crates/` and the root `src/`: "
        + ", ".join(sorted(dropped)[:5])
        + (" …" if len(dropped) > 5 else "")
    )

if problems:
    print("self-check: FAIL — the per-crate rows do not sum to the report's own total")
    for p in problems:
        print(f"  - {p}")
    print(
        "  a dropped or unclassified file would make the per-crate table "
        "contradict the workspace total; classify it instead of dropping it"
    )
    sys.exit(1)

print(
    f"self-check: OK — derived {tot_c}/{tot_t} equals the report's own "
    f"covered/coverable ({int(report_c)}/{int(report_t)})"
)
sys.exit(0)
