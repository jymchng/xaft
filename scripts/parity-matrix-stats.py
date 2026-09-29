#!/usr/bin/env python3
"""
parity-matrix-stats.py — derive the parity matrix's aggregate numbers from its rows.

WHY
    docs/parity-matrix.md states per-section subtotals and a Summary table by
    hand. They drifted: commit ba4aa21 flipped 14 `jobs` rows to ✅ and updated
    the rows but not the aggregates, so the document claimed 62 ✅ / 16% while
    its own rows summed to 76. A hand-maintained total is only correct at the
    instant someone writes it.

    This script recomputes everything from the rows, which are the only
    hand-maintained part. It can be run with --check in CI or a pre-commit hook
    to fail the moment the aggregates disagree with the rows.

WHAT IT DOES
    Parses every numbered data row in docs/parity-matrix.md, groups it by the
    `## N. <name>` section it falls under, and counts statuses. It then
    compares that against:
      * the `**Subtotal: N rows.** ...` line under each section, and
      * the `## Summary` table's per-section ✅/🟡/❌ columns and TOTAL row,
        plus the prose line stating the overall ✅ count and percentage.

    Every class subsection header that declares `(N keys)` is also checked
    against the number of rows it actually contains, because a class whose
    declared count disagrees with its rows is the same class of bug one level
    down.

USAGE
    scripts/parity-matrix-stats.py            # print the derived table
    scripts/parity-matrix-stats.py --check    # exit 1 if the doc is stale

EXIT
    0  the document's aggregates agree with its rows
    1  the document's aggregates are stale (only with --check)
    2  the document could not be parsed at all
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

STATUSES = ("✅", "🟡", "❌")
DEFAULT_MATRIX = Path(__file__).resolve().parent.parent / "docs" / "parity-matrix.md"

# `## 1. CLI commands`  /  `## How to read this document`
SECTION_RE = re.compile(r"^##\s+(?:\d+\.\s*)?(.+?)\s*$")
# `**Subtotal: 60 rows.** ❌ 41 · 🟡 10 · ✅ 9`
SUBTOTAL_RE = re.compile(
    r"\*\*Subtotal:\s*(\d+)\s+rows?\.\*\*\s*"
    r"(?:❌\s*(\d+)\s*·\s*)?(?:🟡\s*(\d+)\s*·\s*)?(?:✅\s*(\d+)\s*)?$"
)
# `### \`ProviderProfile\` (18 keys)`
CLASS_RE = re.compile(r"^###\s+`(\w+)`\s+\((\d+)\s+keys?\)")
# A data row: `| 18 | \`jobs list\` | \`cli/...\`.py\` | crates/... | ✅ |`
ROW_RE = re.compile(r"^\|\s*(\d+)\s*\|")


class Row:
    __slots__ = ("lineno", "number", "section", "cls", "status")

    def __init__(self, lineno: int, number: int, section: str, cls: str | None, status: str):
        self.lineno = lineno
        self.number = number
        self.section = section
        self.cls = cls
        self.status = status


def parse(text: str) -> tuple[list[Row], dict, dict, int | None, int | None]:
    """Return (rows, section_subtotals, class_declared, summary_pct, summary_ok)."""
    lines = text.split("\n")
    rows: list[Row] = []
    subtotals: dict[str, dict] = {}
    classes: dict[str, int] = {}
    section: str | None = None
    cls: str | None = None
    in_summary = False
    summary_rows: dict[str, tuple[int, int, int]] = {}
    summary_total: tuple[int, int, int] | None = None
    summary_ok: int | None = None
    summary_pct: int | None = None

    for i, line in enumerate(lines, start=1):
        stripped = line.strip()

        if stripped.startswith("## "):
            head = stripped[3:].strip()
            in_summary = head.lower() == "summary"
            if in_summary:
                section = None
                continue
            m = SECTION_RE.match(stripped)
            section = m.group(1) if m else head
            cls = None
            continue

        if in_summary:
            # `| 1. CLI commands | 59 | 12 | 9 | 10 | 41 |`
            if stripped.startswith("|") and not set(stripped) <= set("|-: "):
                cells = [c.strip() for c in stripped.strip("|").split("|")]
                nums = [c.strip("*") for c in cells]
                if len(nums) >= 6 and all(n.isdigit() for n in nums[:6]):
                    name = re.sub(r"^\d+\.\s*", "", nums[0])
                    summary_rows[name] = (int(nums[3]), int(nums[4]), int(nums[5]))
                elif len(nums) >= 5 and nums[0].isdigit() and all(n.isdigit() for n in nums[1:5]):
                    # `| **365** | **108** | **62** | **155** | **154** |` shape
                    pass
                elif cells and cells[0].replace("*", "") == "TOTAL":
                    # cells[0] is the label; the three status columns are the
                    # LAST three numeric cells, so index from the end rather
                    # than assuming they start at cells[1].
                    vals = [c.strip("*") for c in cells if c.strip("*").isdigit()]
                    if len(vals) >= 3:
                        summary_total = (int(vals[-3]), int(vals[-2]), int(vals[-1]))
            m = re.search(r"\*\*(\d+)\*\*\s+of\s+(\d+)\s+rows\s+✅\s+\((\d+)%\)", stripped)
            if m:
                summary_ok = int(m.group(1))
                summary_pct = int(m.group(3))
            continue

        m = CLASS_RE.match(stripped)
        if m:
            cls = m.group(1)
            classes[cls] = int(m.group(2))
            continue

        m = SUBTOTAL_RE.match(stripped)
        if m and section:
            miss, part, done = m.group(2), m.group(3), m.group(4)
            subtotals[section] = {
                "rows": int(m.group(1)),
                "❌": int(miss or 0),
                "🟡": int(part or 0),
                "✅": int(done or 0),
            }
            continue

        if not ROW_RE.match(stripped) or section is None:
            continue
        if not any(s in stripped for s in STATUSES):
            continue
        cells = [c.strip() for c in stripped.strip("|").split("|")]
        if len(cells) < 5:
            continue
        status = next((s for s in STATUSES if s in cells[-1]), None)
        if status is None:
            continue
        rows.append(Row(i, int(cells[0]), section, cls, status))

    return rows, subtotals, classes, summary_pct, summary_ok, summary_rows, summary_total


def tally(rows, key):
    out: dict[str, dict] = {}
    for r in rows:
        slot = out.setdefault(r.section, {"rows": 0, "✅": 0, "🟡": 0, "❌": 0})
        slot["rows"] += 1
        slot[r.status] += 1
    return out


def main() -> int:
    args = [a for a in sys.argv[1:]]
    if "--matrix" in args:
        path = Path(args[args.index("--matrix") + 1])
    else:
        path = DEFAULT_MATRIX
    check = "--check" in args

    if not path.is_file():
        print(f"error: matrix not found: {path}", file=sys.stderr)
        return 2

    rows, subtotals, classes, pct, ok_n, summary_rows, summary_total = parse(
        path.read_text(encoding="utf-8")
    )
    if not rows:
        print(f"error: parsed 0 data rows from {path}", file=sys.stderr)
        return 2

    derived = tally(rows, "section")

    print(f"matrix: {path}")
    print(f"data rows: {len(rows)}\n")
    hdr = f"{'section':<26}{'rows':>6}{'✅':>5}{'🟡':>5}{'❌':>5}   status"
    print(hdr)
    print("-" * len(hdr))

    problems: list[str] = []
    tot = {"rows": 0, "✅": 0, "🟡": 0, "❌": 0}

    for name, d in derived.items():
        for k in tot:
            tot[k] += d[k]
        written = subtotals.get(name)
        if written is None:
            status = "no subtotal line"
            problems.append(f"section {name!r} has no Subtotal line")
        elif (
            written["rows"] != d["rows"]
            or written["✅"] != d["✅"]
            or written["🟡"] != d["🟡"]
            or written["❌"] != d["❌"]
        ):
            status = (
                f"STALE: doc says {written['rows']} rows "
                f"❌{written['❌']} 🟡{written['🟡']} ✅{written['✅']}"
            )
            problems.append(
                f"section {name!r}: subtotal says {written}, rows say {d}"
            )
        else:
            status = "ok"
        print(f"{name[:26]:<26}{d['rows']:>6}{d['✅']:>5}{d['🟡']:>5}{d['❌']:>5}   {status}")

    print("-" * len(hdr))
    print(f"{'TOTAL':<26}{tot['rows']:>6}{tot['✅']:>5}{tot['🟡']:>5}{tot['❌']:>5}")

    # Summary table cross-check
    for name, (g, y, n) in summary_rows.items():
        d = derived.get(name)
        if d and (d["✅"], d["🟡"], d["❌"]) != (g, y, n):
            problems.append(
                f"Summary row {name!r} says ✅{g} 🟡{y} ❌{n}, rows say "
                f"✅{d['✅']} 🟡{d['🟡']} ❌{d['❌']}"
            )
    if summary_total and (summary_total != (tot["✅"], tot["🟡"], tot["❌"])):
        problems.append(
            f"Summary TOTAL says ✅{summary_total[0]} 🟡{summary_total[1]} "
            f"❌{summary_total[2]}, rows say ✅{tot['✅']} 🟡{tot['🟡']} ❌{tot['❌']}"
        )
    if ok_n is not None and ok_n != tot["✅"]:
        problems.append(f"prose says {ok_n} ✅, rows say {tot['✅']}")
    if pct is not None:
        real = round(100 * tot["✅"] / tot["rows"])
        if real != pct:
            problems.append(f"prose says {pct}%, rows give {real}%")

    # class (N keys) headers
    per_class: dict[str, int] = {}
    for r in rows:
        if r.cls:
            per_class[r.cls] = per_class.get(r.cls, 0) + 1
    for name, declared in classes.items():
        actual = per_class.get(name, 0)
        if actual != declared:
            problems.append(f"class {name!r} declares {declared} keys, has {actual} rows")

    if problems:
        print(f"\n{len(problems)} disagreement(s) between the rows and the aggregates:")
        for p in problems:
            print(f"  - {p}")
        if check:
            return 1
        return 0

    print("\nall aggregates agree with the rows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
