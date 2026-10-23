#!/usr/bin/env python3
"""
cli-row-ownership.py — assert every open CLI parity row has a named owner.

WHY
    Section 1 of docs/parity-matrix.md carries a 60-row table and, next to it,
    an ownership table (§1c) saying who closes each open row and why it is not
    a small increment. The table has as many rows as there are open rows —
    21 at the time of writing, which is a derived figure, not a constant, so do
    not quote this docstring's number as the answer. That ownership table is
    hand-maintained, and a hand-maintained table that is not policed is the
    same class of bug this repository keeps rediscovering:

      * commit ba4aa21 flipped 14 `jobs` rows to ✅ and left the aggregates
        stale — that is what scripts/parity-matrix-stats.py exists to prevent;
      * the goal list itself carried "32 remaining G4 CLI rows" for several
        passes when the measured figure was 24, and docs/g4-decomposition.md
        carried 23 — that is what THIS script exists to prevent.

    The open count itself has moved repeatedly, and not only in one direction:
    23 → 21 when rows 48/49 landed, then 21 → 24 when §1b counted rows 20/27/29
    as open because they were a false ✅, then back to 21 once those three were
    genuinely fixed. A later pass seeing the same 21 it has seen before should
    read that round trip as history, not as a no-op commit.

    The failure mode is quiet: a row is added, flipped to ❌, or closed, and
    because nobody re-derives the open set, the ownership table keeps naming a
    row that is already done (or misses one that is newly open). A stale owner
    pointer is worse than no pointer, because it sends the next pass to work
    from a baseline that no longer describes the tree.

WHAT IT DOES
    Parses section 1 of the matrix with the SAME parser as
    scripts/parity-matrix-stats.py (imported, not reimplemented, so the two
    cannot disagree about what a "row" is), then asserts that the set of
    non-✅ section-1 rows is exactly the set of rows the §1c table claims.

    It also refuses an owner cell that is empty or that still says one of the
    placeholder words which have shipped in these tables before
    ("new increment", "TBD", "unassigned"), because "some goal" is the same
    defect as "no goal".

USAGE
    scripts/cli-row-ownership.py            # print the derived table
    scripts/cli-row-ownership.py --check    # exit 1 if any row is unowned
    scripts/cli-row-ownership.py --self-test
                                             # exit 1 if a drift mode is
                                             # undetectable

EXIT
    0  every open CLI row has a named owner
    1  an open row has no owner, an owner is a placeholder, or the table
       claims a row that is already ✅ (only with --check)
    2  the matrix or the §1c table could not be parsed
"""
from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STATS = Path(__file__).resolve().parent / "parity-matrix-stats.py"
DEFAULT_MATRIX = ROOT / "docs" / "parity-matrix.md"

# Owners that assert nothing. "new increment" shipped in the §2b table and is
# exactly the stale-cross-reference defect this script is here to stop.
# "unowned" is here because writing `**unowned — see below**` into an owner cell
# made this check pass while the row was in fact unassigned, which is the same
# defect wearing a different hat.
PLACEHOLDER_OWNERS = {
    "new increment",
    "tbd",
    "todo",
    "unassigned",
    "unowned",
    "no owner",
    "n/a",
    "none",
    "-",
    "—",
    "?",
}

SECTION = "CLI commands"
OWNERSHIP_HEADING = re.compile(r"^###\s+1c\.", re.M)


def load_stats_module():
    """Import parity-matrix-stats.py so both scripts share one definition of a row."""
    spec = importlib.util.spec_from_file_location("parity_matrix_stats", STATS)
    if spec is None or spec.loader is None:  # pragma: no cover - install layout
        sys.exit(f"error: cannot load {STATS}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def ownership_rows(text: str) -> dict[int, str]:
    """Return {row_number: owner} parsed from the §1c table, or {} if absent."""
    m = OWNERSHIP_HEADING.search(text)
    if not m:
        return {}

    # The §1c table is the first pipe table after the heading, and runs until
    # the next `##` heading (section 2).
    rest = text[m.end():]
    end = rest.find("\n## ")
    block = rest if end == -1 else rest[:end]

    owners: dict[int, str] = {}
    for line in block.split("\n"):
        s = line.strip()
        if not s.startswith("|"):
            continue
        cells = [c.strip() for c in s.strip("|").split("|")]
        if len(cells) < 4:
            continue
        rows_cell, owner = cells[0], cells[-1]
        # A range (`50–53`, `50-53`) or a comma list (`8, 11, 12`) in the first
        # cell; a header row like `Rows` simply matches nothing.
        normalised = rows_cell.replace("–", "-").replace("—", "-")
        for part in re.split(r"[,\s]+", normalised):
            part = part.strip().strip("`")
            if not part:
                continue
            span = re.fullmatch(r"(\d+)\s*-\s*(\d+)", part)
            if span:
                lo, hi = int(span.group(1)), int(span.group(2))
                if hi < lo:
                    sys.exit(
                        f"error: §1c row range {part!r} is written backwards "
                        f"({lo} > {hi})"
                    )
                numbers: list[int] = list(range(lo, hi + 1))
            elif part.isdigit():
                numbers = [int(part)]
            else:
                continue
            for n in numbers:
                if n in owners:
                    sys.exit(
                        f"error: §1c lists row {n} in more than one family; "
                        "an open row must have exactly one owner"
                    )
                owners[n] = owner
    return owners


def check_text(text: str):
    """Return (problems, open_rows, owners)."""
    stats = load_stats_module()
    rows, _, _, _, _, _, _ = stats.parse(text)
    cli = [r for r in rows if r.section == SECTION]
    if not cli:
        return ["section 1 yielded no rows"], {}, {}

    open_rows = {r.number: r for r in cli if r.status != "✅"}
    owners = ownership_rows(text)
    problems: list[str] = []

    # With every CLI row closed there is nothing to own, and an empty §1c
    # table is the honest state; an open row with no table is still caught by
    # the per-row loop below.
    if not owners and open_rows and OWNERSHIP_HEADING.search(text) is None:
        problems.append("no §1c ownership table found in the matrix")

    def is_placeholder(owner: str) -> bool:
        # Strip markdown emphasis/backticks first: `**unowned — see below**` is
        # the same non-answer as `unowned`, and the emphasis must not be a way
        # to slip past this check.
        norm = re.sub(r"[*`]", "", owner).strip().lower()
        if norm in PLACEHOLDER_OWNERS:
            return True
        # Also catch `unowned — see below`, where the cell leads with the
        # non-answer and then explains at length why it has no owner.
        lead = re.split(r"\s+[—–-]\s+|\s+--\s+", norm, maxsplit=1)[0].strip()
        return lead in PLACEHOLDER_OWNERS

    for n, owner in sorted(owners.items()):
        if n not in open_rows:
            problems.append(
                f"§1c claims row {n}, which is not an open row in section 1 "
                "(it is ✅, or it does not exist) — remove it or close the row"
            )
            continue
        if not owner.strip() or is_placeholder(owner):
            problems.append(
                f"row {n} ({open_rows[n].status}) has a placeholder owner "
                f"{owner!r}; name a goal or record a declined-with-reason"
            )

    for n in sorted(open_rows):
        if n not in owners:
            problems.append(
                f"row {n} is open ({open_rows[n].status}) but §1c gives it no owner"
            )

    return problems, open_rows, owners


def self_test(text: str) -> int:
    """Every drift mode this script claims to police must actually fail."""
    problems, _, _ = check_text(text)
    if problems:
        print("self-test: FAIL — the real document is already inconsistent:")
        for p in problems:
            print(f"  - {p}")
        return 1

    # The mutations are built from the document as it is, so the self-test
    # keeps working whichever rows happen to be open: row 57 (`init`, closed
    # by the cli-family branch) is re-opened or re-claimed synthetically.
    closed_57 = re.search(r"^\| 57 \| `init` \|.*\| ✅ \|$", text, re.M)
    if closed_57 is None:
        print("self-test: FAIL — row 57 (`init`) is not a ✅ row to mutate")
        return 1
    reopened = closed_57.group(0)[: -len("✅ |")] + "❌ |"

    def table_end(s: str) -> int:
        i = s.find("### 1c.")
        bar = s.find("\n|---", i)
        return s.find("\n\n", bar)

    def claim(s: str, owner: str) -> str:
        j = table_end(s)
        return s[:j] + f"\n| 57 | `init` | synthetic | {owner} |" + s[j:]

    def open_unowned(s: str) -> str:
        return s.replace(closed_57.group(0), reopened, 1)

    def open_with_placeholder(s: str) -> str:
        return claim(open_unowned(s), "new increment")

    def owned_but_closed(s: str) -> str:
        return claim(s, "goal `deadbeef`")

    def drop_row_1c(s: str) -> str:
        # Delete the whole §1c table, leaving a re-opened row unowned.
        s = claim(open_unowned(s), "goal `deadbeef`")
        i = s.find("### 1c.")
        j = s.find("\n## ", i)
        return s[:i] + s[j + 1:]

    mutations = [
        ("an open row with no owner", open_unowned),
        ("an owner replaced with a placeholder", open_with_placeholder),
        ("an owned row flipped to ✅", owned_but_closed),
        ("the §1c table deleted", drop_row_1c),
    ]

    failed = 0
    for label, mutate in mutations:
        mutated = mutate(text)
        if mutated == text:
            print(f"self-test: FAIL — mutation {label!r} did not change the document")
            failed += 1
            continue
        probs, _, _ = check_text(mutated)
        if probs:
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
    args = list(sys.argv[1:])
    path = DEFAULT_MATRIX
    if "--matrix" in args:
        path = Path(args[args.index("--matrix") + 1])

    if not path.is_file():
        print(f"error: matrix not found: {path}", file=sys.stderr)
        return 2

    text = path.read_text(encoding="utf-8")

    if "--self-test" in args:
        return self_test(text)

    problems, open_rows, owners = check_text(text)
    if problems == ["section 1 yielded no rows"]:
        print(f"error: parsed nothing from {path}", file=sys.stderr)
        return 2

    print(f"matrix: {path}")
    print(f"open CLI rows: {len(open_rows)}   owned: {len(owners)}\n")
    hdr = f"{'row':>5}  {'status':<7}{'owner'}"
    print(hdr)
    print("-" * len(hdr))
    for n in sorted(open_rows):
        print(f"{n:>5}  {open_rows[n].status:<7}{owners.get(n, '<UNOWNED>')}")

    if problems:
        print(f"\n{len(problems)} ownership problem(s):")
        for p in problems:
            print(f"  - {p}")
        if "--check" in args:
            return 1
        return 0

    print("\nevery open CLI row has a named owner")
    return 0


if __name__ == "__main__":
    sys.exit(main())
