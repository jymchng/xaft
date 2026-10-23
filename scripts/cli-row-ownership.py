#!/usr/bin/env python3
"""
cli-row-ownership.py — assert every open CLI parity row has a named owner.

WHY
    Section 1 of docs/parity-matrix.md carries a table of every CLI row (60
    when this was written; like the open count below, a derived figure) and,
    next to it, an ownership table (§1c) saying who closes each open row and
    why it is not a small increment. The table has as many rows as there are
    open rows — 21 at the time of writing, which is a derived figure, not a
    constant, so do not quote this docstring's number as the answer. That
    ownership table is hand-maintained, and a hand-maintained table that is
    not policed is the same class of bug this repository keeps rediscovering:

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

➖ ROWS ARE NOT OPEN
    A row marked ➖ (removed upstream) names surface agenthicc no longer has.
    It is history, not work: nobody can close it, so it needs no owner, and a
    §1c entry claiming one is an error — exactly like claiming a ✅ row. The
    open set is therefore every section-1 row that is neither ✅ nor ➖.

"AUTHORIZED, UNCLAIMED" IS A RECORDED STATE, AND IT IS COUNTED OUT LOUD
    The 2026-10-23 re-baseline added rows for agenthicc surface that had none
    (the `schedules` family, the inbox, fifteen global flags, ...). The
    operator's 2026-10-23 authorization of every agenthicc subsystem covers
    them, but no branch has taken them yet. Writing a goal label for work no
    goal owns would be the stale-pointer defect this script exists to stop;
    writing "unowned" would rightly fail the placeholder check. So such a row
    carries the owner `**authorized, unclaimed** — ...`, which records both
    facts, and every run prints how many open rows are in that state and which
    ones. It is not a failure — the decision exists — but it is never silent.

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
DONE, REMOVED = "✅", "➖"
# The owner value for an open row the operator authorized and no branch has
# taken; see the module docstring. Matched after markdown emphasis is removed.
UNCLAIMED_OWNER = "authorized, unclaimed"


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
    rows = stats.parse(text)[0]
    cli = [r for r in rows if r.section == SECTION]
    if not cli:
        return ["section 1 yielded no rows"], {}, {}

    open_rows = {r.number: r for r in cli if r.status not in (DONE, REMOVED)}
    removed = {r.number for r in cli if r.status == REMOVED}
    owners = ownership_rows(text)
    problems: list[str] = []

    # With every CLI row closed there is nothing to own, and an empty §1c
    # table is the honest state; an open row with no table is still caught by
    # the per-row loop below. (Ported from parity/wave2a.)
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
        if n in removed:
            problems.append(
                f"§1c claims row {n}, which is ➖ (removed upstream) — there is "
                "nothing left to close, so remove it from §1c"
            )
            continue
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

    def drop_row_1c(s: str) -> str:
        # Delete the whole §1c table, leaving the rows open and unowned.
        i = s.find("### 1c.")
        j = s.find("\n## ", i)
        return s[:i] + s[j + 1:]

    # The mutations are written against the document's SHAPE, not against
    # literal lines. The first version replaced `| G4b-1 |` and one exact row
    # 57; both were closed, every replace became a no-op, and the self-test
    # went red while the checker itself was fine.
    def first_owner_row(s: str) -> tuple[int, int]:
        """(start, end) of the first §1c data row's text."""
        i = s.find("### 1c.")
        j = s.find("\n## ", i)
        m = re.search(r"^\| *\d[^\n]*$", s[i:j], re.M)
        assert m, "the §1c table has no data row"
        return i + m.start(), i + m.end()

    def with_owner(owner: str):
        def mutate(s: str) -> str:
            start, end = first_owner_row(s)
            row = s[start:end]
            cells = row.rstrip("|").split("|")
            return s[:start] + "|".join(cells[:-1]) + f"| {owner} |" + s[end:]
        return mutate

    def set_owned_row_status(status: str):
        def mutate(s: str) -> str:
            # Change an owned open row's status without touching §1c.
            n = min(ownership_rows(s))
            return re.sub(
                rf"^(\| {n} \|.*\| )(?:🟡|❌)( \|)$",
                rf"\g<1>{status}\g<2>",
                s,
                count=1,
                flags=re.M,
            )
        return mutate

    mutations = [
        ("the §1c table deleted", drop_row_1c),
        ("an owner cell blanked", with_owner("")),
        ("an owner replaced with a placeholder", with_owner("new increment")),
        ("an owned row flipped to ✅", set_owned_row_status(DONE)),
        ("an owned row marked ➖ but still claimed", set_owned_row_status(REMOVED)),
    ]

    failed = 0

    # A ➖ row is not open: unclaimed, it must not be reported as unowned.
    synthetic = (
        "## 1. CLI commands\n\n"
        "| # | command | source | xaft | Status |\n|---|---|---|---|---|\n"
        "| 1 | `a` | s | x | ✅ |\n| 2 | `b` | s | x | ❌ |\n"
        "| 3 | `c` | s | removed upstream | ➖ |\n\n"
        "### 1c. Open CLI rows\n\n"
        "| Rows | Family | Why | Owner |\n|---|---|---|---|\n"
        "| 2 | `b` | prose | **authorized, unclaimed** — no branch yet |\n\n"
        "## 2. Next\n"
    )
    probs, open_rows, _ = check_text(synthetic)
    if probs or sorted(open_rows) != [2]:
        print(f"self-test: FAIL — a ➖ row is treated as open: {probs}")
        failed += 1
    else:
        print("self-test: ok — a ➖ row is not open and needs no owner")

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

    stats = load_stats_module()
    rows = [r for r in stats.parse(text)[0] if r.section == SECTION]
    removed = sorted(r.number for r in rows if r.status == REMOVED)
    print(f"matrix: {path}")
    print(
        f"open CLI rows: {len(open_rows)}   owned: {len(owners)}   "
        f"removed upstream (➖, need no owner): {len(removed)}\n"
    )
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

    unclaimed = sorted(
        n for n, owner in owners.items()
        if re.sub(r"[*`]", "", owner).strip().lower().startswith(UNCLAIMED_OWNER)
    )
    if unclaimed:
        print(
            f"\n{len(unclaimed)} open row(s) are authorized but unclaimed — the decision "
            f"exists, no branch has taken them: {', '.join(map(str, unclaimed))}"
        )
    print("\nevery open CLI row has a named owner")
    return 0


if __name__ == "__main__":
    sys.exit(main())
