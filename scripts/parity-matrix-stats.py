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
    scripts/parity-matrix-stats.py                # print the derived table
    scripts/parity-matrix-stats.py --check        # exit 1 if the doc is stale
    scripts/parity-matrix-stats.py --self-test    # exit 1 if any drift mode
                                                   # this script claims to
                                                   # police is undetectable

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
            # `| 1. CLI commands | 59 | 19 | 37 | 5 | 18 |`
            if stripped.startswith("|") and not set(stripped) <= set("|-: "):
                cells = [c.strip() for c in stripped.strip("|").split("|")]
                nums = [c.strip("*") for c in cells]
                # The `N. ` prefix must be stripped BEFORE any isdigit guard.
                # Guarding on the raw label makes every section row fail the
                # test, which silently disables the whole per-section
                # cross-check while the docstring still claims it runs.
                label = re.sub(r"^\d+\.\s*", "", nums[0]) if nums else ""
                is_total = bool(cells) and cells[0].replace("*", "") == "TOTAL"
                if not is_total and label and len(nums) >= 4 and all(
                    n.isdigit() for n in nums[1:]
                ):
                    # The status columns are the LAST three numeric cells; the
                    # leading ones count entries on each side and are not
                    # row-derived, so they are deliberately not compared.
                    summary_rows[label] = (int(nums[-3]), int(nums[-2]), int(nums[-1]))
                elif is_total:
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


def check_text(text: str):
    """Compare every aggregate in `text` against the rows `text` contains.

    Returns (problems, per_section, totals). An empty `problems` list means the
    document is internally consistent.
    """
    rows, subtotals, classes, pct, ok_n, summary_rows, summary_total = parse(text)
    derived = tally(rows, "section")
    problems = []
    tot = {"rows": 0, "✅": 0, "🟡": 0, "❌": 0}

    for name, d in derived.items():
        for k in tot:
            tot[k] += d[k]
        written = subtotals.get(name)
        if written is None:
            problems.append(f"section {name!r} has no Subtotal line")
        elif (
            written["rows"] != d["rows"]
            or written["✅"] != d["✅"]
            or written["🟡"] != d["🟡"]
            or written["❌"] != d["❌"]
        ):
            problems.append(f"section {name!r}: subtotal says {written}, rows say {d}")

    for name, (g, y, n) in summary_rows.items():
        d = derived.get(name)
        if d is None:
            problems.append(
                f"Summary lists section {name!r}, which no `## N.` heading defines"
            )
        elif (d["✅"], d["🟡"], d["❌"]) != (g, y, n):
            problems.append(
                f"Summary row {name!r} says ✅{g} 🟡{y} ❌{n}, rows say "
                f"✅{d['✅']} 🟡{d['🟡']} ❌{d['❌']}"
            )
    for name in derived:
        if name not in summary_rows:
            problems.append(f"section {name!r} is missing from the Summary table")

    if summary_total and summary_total != (tot["✅"], tot["🟡"], tot["❌"]):
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

    per_class = {}
    for r in rows:
        if r.cls:
            per_class[r.cls] = per_class.get(r.cls, 0) + 1
    for name, declared in classes.items():
        actual = per_class.get(name, 0)
        if actual != declared:
            problems.append(f"class {name!r} declares {declared} keys, has {actual} rows")

    return problems, derived, tot


def self_test(text: str) -> int:
    """Assert this checker actually fails when the document drifts.

    A `--check` that cannot fail is worse than no check, and this script had
    exactly that bug: a dead `re.sub` left the per-section Summary cross-check
    unreachable while the docstring still claimed it ran. Every aggregate the
    script claims to police gets a mutation here that must be caught, so the
    same class of hole cannot reopen unnoticed.
    """
    problems, _, _ = check_text(text)
    if problems:
        print("self-test: FAIL — the real document is already inconsistent:")
        for p in problems:
            print(f"  - {p}")
        return 1

    mutations = [
        (
            "a row's status flipped",
            lambda s: s.replace(
                "| 8 | `session send` | `cli/commands/session_service.py` | — | ❌ |",
                "| 8 | `session send` | `cli/commands/session_service.py` | — | ✅ |",
                1,
            ),
        ),
        (
            "a section Subtotal line",
            lambda s: re.sub(
                r"^\*\*Subtotal: (\d+) rows\.\*\*.*$",
                lambda m: f"**Subtotal: {m.group(1)} rows.** ❌ 41 · 🟡 10 · ✅ 9",
                s,
                count=1,
                flags=re.M,
            ),
        ),
        (
            "a Summary per-section row",
            lambda s: re.sub(
                r"^(\| 1\. CLI commands \| \d+ \| \d+ \| )(\d+)( \| \d+ \| \d+ \|)$",
                r"\g<1>99\g<3>",
                s,
                count=1,
                flags=re.M,
            ),
        ),
        (
            "the Summary TOTAL row",
            lambda s: re.sub(
                r"^(\| \*\*TOTAL\*\* \| \*\*\d+\*\* \| \*\*\d+\*\* \| \*\*)(\d+)(\*\* \|)",
                r"\g<1>999\g<3>",
                s,
                count=1,
                flags=re.M,
            ),
        ),
        (
            "the prose ✅ count",
            lambda s: re.sub(
                r"(Current parity: \*\*)\d+(\*\* of \d+ rows)",
                r"\g<1>999\g<2>",
                s,
                count=1,
            ),
        ),
        (
            "the prose percentage",
            lambda s: re.sub(
                r"(Current parity: \*\*\d+\*\* of \d+ rows ✅ \()(\d+)(\%\))",
                r"\g<1>99\g<3>",
                s,
                count=1,
            ),
        ),
        (
            "a class (N keys) header",
            lambda s: re.sub(
                r"^(### `\w+` \()(\d+)( keys\))",
                r"\g<1>99\g<3>",
                s,
                count=1,
                flags=re.M,
            ),
        ),
    ]

    failed = 0
    for label, mutate in mutations:
        mutated = mutate(text)
        if mutated == text:
            print(f"self-test: FAIL — mutation {label!r} did not change the document")
            failed += 1
            continue
        problems, _, _ = check_text(mutated)
        if problems:
            print(f"self-test: ok — {label} is caught")
        else:
            print(f"self-test: FAIL — {label} drifted but was NOT caught")
            failed += 1

    if failed:
        print(f"\nself-test: {failed} check(s) cannot detect drift")
        return 1
    print(f"\nself-test: all {len(mutations)} drift modes are detected")
    return 0


def main() -> int:
    args = [a for a in sys.argv[1:]]
    if "--matrix" in args:
        path = Path(args[args.index("--matrix") + 1])
    else:
        path = DEFAULT_MATRIX
    check = "--check" in args
    self_test_mode = "--self-test" in args

    if not path.is_file():
        print(f"error: matrix not found: {path}", file=sys.stderr)
        return 2

    text = path.read_text(encoding="utf-8")
    if self_test_mode:
        return self_test(text)

    problems, derived, tot = check_text(text)
    if not derived:
        print(f"error: parsed 0 data rows from {path}", file=sys.stderr)
        return 2

    print(f"matrix: {path}")
    print(f"data rows: {tot['rows']}\n")
    hdr = f"{'section':<26}{'rows':>6}{'✅':>5}{'🟡':>5}{'❌':>5}   status"
    print(hdr)
    print("-" * len(hdr))

    for name, d in derived.items():
        bad = [p for p in problems if p.startswith(f"section {name!r}")]
        status = "ok" if not bad else "STALE"
        print(
            f"{name[:26]:<26}{d['rows']:>6}{d['✅']:>5}"
            f"{d['🟡']:>5}{d['❌']:>5}   {status}"
        )

    print("-" * len(hdr))
    print(
        f"{'TOTAL':<26}{tot['rows']:>6}{tot['✅']:>5}"
        f"{tot['🟡']:>5}{tot['❌']:>5}"
    )

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
