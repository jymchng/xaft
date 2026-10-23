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
      * the `**Subtotal: N rows.** ...` line under each section,
      * the `## Summary` table's per-section status columns, its "xaft
        entries" column (which is defined as ✅ + 🟡) and its TOTAL row, and
      * the prose stating the overall ✅ count, the row denominator, the
        percentage and the number of ➖ rows.

    Every class subsection header that declares `(N keys)` — or `(N keys, M
    removed upstream)` — is also checked against the rows it actually
    contains, because a class whose declared count disagrees with its rows is
    the same class of bug one level down.

➖ — REMOVED UPSTREAM, AND WHY IT IS NOT A FOURTH KIND OF ROW TO COUNT
    agenthicc deletes surface too (the 2026-10-23 re-baseline found 37 config
    keys PRD-250 deleted as unread). Deleting those rows would erase the
    record of what the matrix used to claim — including ✅ rows that silently
    stopped meaning anything — so they stay, marked ➖ with a note naming what
    replaced them. But a row for surface that no longer exists is not parity
    work in either direction: it is not a gap xaft must close, and it is not a
    feature xaft can be credited for. So ➖ rows are parsed, reported in
    their own column, and EXCLUDED from every count: a section's "N rows", the
    ✅/🟡/❌ columns, the TOTAL and the percentage's denominator. A ✅ that
    became ➖ therefore lowers the ✅ count and the denominator together, which
    is the honest effect.

    The status cell decides: a row is ➖ when its last cell is ➖. Whether
    agenthicc really lacks the surface is scripts/agenthicc-surface.py's job
    (it reads agenthicc) and, for section 7, scripts/config-key-audit.py's
    (it requires a recorded removal for every ➖ config row).

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

DONE, PARTIAL, MISSING, REMOVED = "✅", "🟡", "❌", "➖"
STATUSES = (DONE, PARTIAL, MISSING, REMOVED)
# The statuses a row is COUNTED under. ➖ rows are parsed and reported, never
# counted; see the module docstring.
COUNTED = (DONE, PARTIAL, MISSING)
DEFAULT_MATRIX = Path(__file__).resolve().parent.parent / "docs" / "parity-matrix.md"

# `## 1. CLI commands`  /  `## How to read this document`
SECTION_RE = re.compile(r"^##\s+(?:\d+\.\s*)?(.+?)\s*$")
# `**Subtotal: 60 rows.** ❌ 41 · 🟡 10 · ✅ 9`
# `**Subtotal: 154 rows.** ❌ 14 · 🟡 2 · ✅ 138 · ➖ 37 removed upstream, not counted`
SUBTOTAL_RE = re.compile(
    r"\*\*Subtotal:\s*(\d+)\s+rows?\.\*\*\s*"
    r"(?:❌\s*(\d+)\s*·\s*)?(?:🟡\s*(\d+)\s*·\s*)?(?:✅\s*(\d+)\s*)?"
    r"(?:·\s*➖\s*(\d+)\b[^\n]*)?$"
)
# `### \`ProviderProfile\` (18 keys)`  /  `### \`ExecutionSettings\` (31 keys, 3 removed upstream)`
CLASS_RE = re.compile(
    r"^###\s+`(\w+)`\s+\((\d+)\s+keys?(?:,\s*(\d+)\s+removed upstream)?\)"
)
# A data row: `| 18 | \`jobs list\` | \`cli/...\`.py\` | crates/... | ✅ |`
ROW_RE = re.compile(r"^\|\s*(\d+)\s*\|")
# `Current parity: **326** of 384 rows ✅ (85%)`
PARITY_RE = re.compile(r"\*\*(\d+)\*\*\s+of\s+(\d+)\s+rows\s+✅\s+\((\d+)%\)")
# `**37** rows are ➖ (removed upstream) ...`
REMOVED_PROSE_RE = re.compile(r"\*\*(\d+)\*\*\s+rows?\s+(?:are|is)\s+➖")


class Row:
    __slots__ = ("lineno", "number", "section", "cls", "status")

    def __init__(self, lineno: int, number: int, section: str, cls: str | None, status: str):
        self.lineno = lineno
        self.number = number
        self.section = section
        self.cls = cls
        self.status = status


def _empty_tally() -> dict[str, int]:
    return {"rows": 0, DONE: 0, PARTIAL: 0, MISSING: 0, REMOVED: 0}


def _summary_columns(cells: list[str]) -> dict[str, int]:
    """Map the Summary header's column names to cell indexes."""
    cols: dict[str, int] = {}
    for i, cell in enumerate(cells):
        name = cell.strip("* ")
        if name in STATUSES:
            cols[name] = i
        elif "xaft" in name.lower():
            cols["xaft"] = i
    return cols


def parse(text: str):
    """Return (rows, subtotals, classes, summary) parsed out of `text`.

    `summary` is a dict: `rows` {label: {column: int}}, `total` {column: int}
    or None, `columns` the header map, `parity` (ok, of, pct) or None and
    `removed` (the ➖ count the prose states) or None.
    """
    lines = text.split("\n")
    rows: list[Row] = []
    subtotals: dict[str, dict] = {}
    classes: dict[str, tuple[int, int]] = {}
    section: str | None = None
    cls: str | None = None
    in_summary = False
    summary: dict = {"rows": {}, "total": None, "columns": None, "parity": None,
                     "removed": None}

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
            if stripped.startswith("|") and not set(stripped) <= set("|-: "):
                cells = [c.strip() for c in stripped.strip("|").split("|")]
                if summary["columns"] is None:
                    # The first row of the table is its header. Reading the
                    # columns by NAME is what lets a column be added (➖ was)
                    # without every index below silently shifting by one.
                    summary["columns"] = _summary_columns(cells)
                    continue
                cols = summary["columns"]
                values: dict[str, int] = {}
                for name, idx in cols.items():
                    if idx < len(cells) and cells[idx].strip("*").isdigit():
                        values[name] = int(cells[idx].strip("*"))
                is_total = cells[0].replace("*", "").strip() == "TOTAL"
                # The `N. ` prefix must be stripped BEFORE any isdigit guard.
                # Guarding on the raw label makes every section row fail the
                # test, which silently disables the whole per-section
                # cross-check while the docstring still claims it runs.
                label = re.sub(r"^\d+\.\s*", "", cells[0].strip("*").strip())
                if is_total:
                    summary["total"] = values
                elif label and all(s in values for s in COUNTED):
                    summary["rows"][label] = values
            m = PARITY_RE.search(stripped)
            if m:
                summary["parity"] = (int(m.group(1)), int(m.group(2)), int(m.group(3)))
            m = REMOVED_PROSE_RE.search(stripped)
            if m:
                summary["removed"] = int(m.group(1))
            continue

        m = CLASS_RE.match(stripped)
        if m:
            cls = m.group(1)
            classes[cls] = (int(m.group(2)), int(m.group(3) or 0))
            continue

        m = SUBTOTAL_RE.match(stripped)
        if m and section:
            miss, part, done, gone = m.group(2), m.group(3), m.group(4), m.group(5)
            subtotals[section] = {
                "rows": int(m.group(1)),
                MISSING: int(miss or 0),
                PARTIAL: int(part or 0),
                DONE: int(done or 0),
                REMOVED: int(gone or 0),
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

    return rows, subtotals, classes, summary


def tally(rows) -> dict[str, dict[str, int]]:
    """Per-section counts. `rows` counts only rows that are COUNTED (not ➖)."""
    out: dict[str, dict] = {}
    for r in rows:
        slot = out.setdefault(r.section, _empty_tally())
        slot[r.status] += 1
        if r.status in COUNTED:
            slot["rows"] += 1
    return out


def check_text(text: str):
    """Compare every aggregate in `text` against the rows `text` contains.

    Returns (problems, per_section, totals). An empty `problems` list means the
    document is internally consistent.
    """
    rows, subtotals, classes, summary = parse(text)
    derived = tally(rows)
    problems = []
    tot = _empty_tally()

    for name, d in derived.items():
        for k in tot:
            tot[k] += d[k]
        written = subtotals.get(name)
        if written is None:
            problems.append(f"section {name!r} has no Subtotal line")
        elif any(written[k] != d[k] for k in ("rows", DONE, PARTIAL, MISSING, REMOVED)):
            problems.append(f"section {name!r}: subtotal says {written}, rows say {d}")

    cols = summary["columns"] or {}
    if derived and summary["columns"] is None:
        problems.append("the Summary table has no header row")
    if tot[REMOVED] and REMOVED not in cols:
        problems.append(
            f"the rows hold {tot[REMOVED]} ➖ row(s) but the Summary table has no ➖ column"
        )

    def compare(label: str, written: dict[str, int], d: dict[str, int]) -> None:
        for status in STATUSES:
            if status not in cols:
                continue
            if written.get(status) != d[status]:
                problems.append(
                    f"{label} says {status}{written.get(status)}, rows say {status}{d[status]}"
                )
        if "xaft" in cols and written.get("xaft") != d[DONE] + d[PARTIAL]:
            problems.append(
                f"{label} says xaft entries {written.get('xaft')}, rows say "
                f"{d[DONE] + d[PARTIAL]} (✅ + 🟡)"
            )

    for name, written in summary["rows"].items():
        d = derived.get(name)
        if d is None:
            problems.append(
                f"Summary lists section {name!r}, which no `## N.` heading defines"
            )
        else:
            compare(f"Summary row {name!r}", written, d)
    for name in derived:
        if name not in summary["rows"]:
            problems.append(f"section {name!r} is missing from the Summary table")
    if summary["total"] is not None:
        compare("Summary TOTAL", summary["total"], tot)
    elif derived:
        problems.append("the Summary table has no TOTAL row")

    if summary["parity"] is not None:
        ok_n, of_n, pct = summary["parity"]
        if ok_n != tot[DONE]:
            problems.append(f"prose says {ok_n} ✅, rows say {tot[DONE]}")
        if of_n != tot["rows"]:
            problems.append(
                f"prose says ✅ out of {of_n} rows, rows say {tot['rows']} "
                "(➖ rows are not in the denominator)"
            )
        real = round(100 * tot[DONE] / tot["rows"]) if tot["rows"] else 0
        if real != pct:
            problems.append(f"prose says {pct}%, rows give {real}%")
    if summary["removed"] is not None and summary["removed"] != tot[REMOVED]:
        problems.append(f"prose says {summary['removed']} ➖ rows, rows say {tot[REMOVED]}")
    elif summary["removed"] is None and tot[REMOVED]:
        problems.append(
            f"the Summary prose never states that {tot[REMOVED]} ➖ row(s) are excluded"
        )

    per_class: dict[str, list[int]] = {}
    for r in rows:
        if r.cls:
            live_removed = per_class.setdefault(r.cls, [0, 0])
            live_removed[1 if r.status == REMOVED else 0] += 1
    for name, (declared, declared_removed) in classes.items():
        live, removed = per_class.get(name, [0, 0])
        if live != declared:
            problems.append(f"class {name!r} declares {declared} keys, has {live} rows")
        if removed != declared_removed:
            problems.append(
                f"class {name!r} declares {declared_removed} removed upstream, "
                f"has {removed} ➖ rows"
            )

    return problems, derived, tot


SYNTHETIC = """\
## 1. Things

| # | thing | source | xaft | Status |
|---|---|---|---|---|
| 1 | `a` | s | x | ✅ |
| 2 | `b` | s | x | ❌ |
| 3 | `c` | s | gone | ➖ |
| 4 | `d` | s | gone | ➖ |

**Subtotal: 2 rows.** ❌ 1 · ✅ 1 · ➖ 2 removed upstream, not counted

## Summary

| Section | agenthicc entries | xaft entries | ✅ | 🟡 | ❌ | ➖ |
|---|---|---|---|---|---|---|
| 1. Things | 2 | 1 | 1 | 0 | 1 | 2 |
| **TOTAL** | **2** | **1** | **1** | **0** | **1** | **2** |

Current parity: **1** of 2 rows ✅ (50%). **2** rows are ➖ (removed upstream)
and are excluded.
"""


def self_test(text: str) -> int:
    """Assert this checker actually fails when the document drifts.

    A `--check` that cannot fail is worse than no check, and this script had
    exactly that bug: a dead `re.sub` left the per-section Summary cross-check
    unreachable while the docstring still claimed it ran. Every aggregate the
    script claims to police gets a mutation here that must be caught, so the
    same class of hole cannot reopen unnoticed.

    The mutations are written against the document's SHAPE, not against a
    literal row. The first version flipped one hard-coded line (`| 8 |
    \\`session send\\` ... | ❌ |`); the row was later closed, the replace
    became a no-op, and the self-test went red for a reason that said nothing
    about the checker.
    """
    failed = 0

    # ➖ exclusion, proven on a document where counting the ➖ rows would
    # change every number: 1 of 2 is 50%, 1 of 4 would be 25%.
    synth_problems, synth_derived, synth_tot = check_text(SYNTHETIC)
    if synth_problems or synth_tot["rows"] != 2 or synth_tot[REMOVED] != 2:
        print(f"self-test: FAIL — ➖ rows are not excluded from the counts: {synth_problems}")
        failed += 1
    else:
        print("self-test: ok — ➖ rows are parsed, reported, and excluded from the denominator")
    counted_anyway = SYNTHETIC.replace("of 2 rows ✅ (50%)", "of 4 rows ✅ (25%)")
    if not check_text(counted_anyway)[0]:
        print("self-test: FAIL — a denominator that counts ➖ rows was NOT caught")
        failed += 1
    else:
        print("self-test: ok — a denominator that counts ➖ rows is caught")

    problems, _, _ = check_text(text)
    if problems:
        print("self-test: FAIL — the real document is already inconsistent:")
        for p in problems:
            print(f"  - {p}")
        return 1

    def first_row_with(status: str, new: str):
        def mutate(s: str) -> str:
            return re.sub(
                rf"^(\| \d+ \|.*\| ){re.escape(status)}( \|)$",
                rf"\g<1>{new}\g<2>",
                s,
                count=1,
                flags=re.M,
            )
        return mutate

    has_removed = REMOVED in "".join(
        line for line in text.split("\n") if ROW_RE.match(line.strip())
    )

    def in_summary(mutate):
        # Other tables carry section labels too (the re-baseline note at the
        # top has a `1. CLI commands` row), and `count=1` would mutate the
        # first match — outside the table this mode is meant to exercise.
        def scoped(s: str) -> str:
            i = s.find("\n## Summary")
            return s if i < 0 else s[:i] + mutate(s[i:])
        return scoped

    mutations = [
        ("a row's status flipped ❌ → ✅", first_row_with(MISSING, DONE), True),
        ("a counted row marked ➖", first_row_with(DONE, REMOVED), True),
        ("a ➖ row counted again", first_row_with(REMOVED, MISSING), has_removed),
        (
            "a section Subtotal line",
            lambda s: re.sub(
                r"^\*\*Subtotal: (\d+) rows\.\*\*.*$",
                lambda m: f"**Subtotal: {m.group(1)} rows.** ❌ 41 · 🟡 10 · ✅ 9",
                s,
                count=1,
                flags=re.M,
            ),
            True,
        ),
        (
            "a Subtotal's ➖ clause",
            lambda s: re.sub(
                r"^(\*\*Subtotal: .*· ➖ )(\d+)",
                lambda m: f"{m.group(1)}{int(m.group(2)) + 1}",
                s,
                count=1,
                flags=re.M,
            ),
            has_removed,
        ),
        (
            "a Summary per-section ✅ cell",
            in_summary(lambda s: re.sub(
                r"^(\| 1\. CLI commands \| \d+ \| \d+ \| )(\d+)( \|)",
                r"\g<1>999\g<3>",
                s,
                count=1,
                flags=re.M,
            )),
            True,
        ),
        (
            "a Summary per-section ➖ cell",
            in_summary(lambda s: re.sub(
                r"^(\| 7\. Configuration keys \|(?: \d+ \|){5} )(\d+)( \|)$",
                r"\g<1>999\g<3>",
                s,
                count=1,
                flags=re.M,
            )),
            has_removed,
        ),
        (
            "a Summary xaft-entries cell",
            in_summary(lambda s: re.sub(
                r"^(\| 1\. CLI commands \| \d+ \| )(\d+)( \|)",
                r"\g<1>999\g<3>",
                s,
                count=1,
                flags=re.M,
            )),
            True,
        ),
        (
            "the Summary TOTAL row",
            in_summary(lambda s: re.sub(
                r"^(\| \*\*TOTAL\*\* \| \*\*\d+\*\* \| \*\*\d+\*\* \| \*\*)(\d+)(\*\* \|)",
                r"\g<1>999\g<3>",
                s,
                count=1,
                flags=re.M,
            )),
            True,
        ),
        (
            "the prose ✅ count",
            lambda s: re.sub(
                r"(Current parity: \*\*)\d+(\*\* of \d+ rows)",
                r"\g<1>999\g<2>",
                s,
                count=1,
            ),
            True,
        ),
        (
            "the prose denominator",
            lambda s: re.sub(
                r"(Current parity: \*\*\d+\*\* of )(\d+)( rows)",
                lambda m: f"{m.group(1)}{int(m.group(2)) + 37}{m.group(3)}",
                s,
                count=1,
            ),
            True,
        ),
        (
            "the prose percentage",
            lambda s: re.sub(
                r"(Current parity: \*\*\d+\*\* of \d+ rows ✅ \()(\d+)(\%\))",
                r"\g<1>9\g<3>",
                s,
                count=1,
            ),
            True,
        ),
        (
            "the prose ➖ count",
            lambda s: re.sub(
                r"(\*\*)(\d+)(\*\* rows? (?:are|is) ➖)",
                lambda m: f"{m.group(1)}{int(m.group(2)) + 1}{m.group(3)}",
                s,
                count=1,
            ),
            has_removed,
        ),
        (
            "a class (N keys) header",
            lambda s: re.sub(
                r"^(### `\w+` \()(\d+)( keys)",
                r"\g<1>99\g<3>",
                s,
                count=1,
                flags=re.M,
            ),
            True,
        ),
        (
            "a class header's removed-upstream count",
            lambda s: re.sub(
                r"^(### `\w+` \(\d+ keys?, )(\d+)( removed upstream\))",
                lambda m: f"{m.group(1)}{int(m.group(2)) + 1}{m.group(3)}",
                s,
                count=1,
                flags=re.M,
            ),
            has_removed,
        ),
    ]

    skipped = 0
    for label, mutate, applies in mutations:
        if not applies:
            print(f"self-test: skip — {label} (the document has no ➖ row to mutate)")
            skipped += 1
            continue
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
    ran = len(mutations) - skipped + 2
    note = f" ({skipped} ➖ mode(s) skipped: no ➖ row to mutate)" if skipped else ""
    print(f"\nself-test: all {ran} applicable drift modes are detected{note}")
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
    print(f"data rows: {tot['rows']} counted, {tot[REMOVED]} ➖ removed upstream (not counted)\n")
    hdr = f"{'section':<26}{'rows':>6}{'✅':>5}{'🟡':>5}{'❌':>5}{'➖':>5}   status"
    print(hdr)
    print("-" * len(hdr))

    for name, d in derived.items():
        bad = [p for p in problems if p.startswith(f"section {name!r}")]
        status = "ok" if not bad else "STALE"
        print(
            f"{name[:26]:<26}{d['rows']:>6}{d[DONE]:>5}"
            f"{d[PARTIAL]:>5}{d[MISSING]:>5}{d[REMOVED]:>5}   {status}"
        )

    print("-" * len(hdr))
    print(
        f"{'TOTAL':<26}{tot['rows']:>6}{tot[DONE]:>5}"
        f"{tot[PARTIAL]:>5}{tot[MISSING]:>5}{tot[REMOVED]:>5}"
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
