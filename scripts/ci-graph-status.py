#!/usr/bin/env python3
"""
ci-graph-status.py — keep docs/ci-status.md's "CI cannot build" record honest.

WHY
    xaft's crates path-depend on the sibling repo ../agtrs, which is a separate
    workspace that has not been ported to the injectable crate rename. Every CI
    job that runs a raw cargo command therefore fails inside `cargo metadata`,
    before compiling a line, and no workflow can reach scripts/sandbox-build.sh
    (the clone it uses lives outside the repo tree on purpose, so a runner
    cannot see it).

    Goal 252addb7 was chartered to ask the operator which of four paths to take
    — clone-and-port in the runner, vendor agtrs into the tree, add it as a
    submodule, or decline with a reason. The operator was asked and did not
    answer, and three of the four add a CI step or a structural dependency that
    does not exist today, i.e. exactly the authorization that was not given.
    The path taken is the fourth: add nothing, and record the state, so the next
    pass does not re-derive it and does not read several red jobs as a code
    regression.

    A record like that rots in two directions. It can claim CI is broken after
    the graph starts resolving (agtrs finally ported, or the dependency
    dropped), and it can claim no workflow provides the graph after one does.
    Both are the stale-cross-reference class this repository keeps rediscovering
    — hand-maintained claims that nothing re-derives. This script is the
    re-derivation.

WHAT IT CHECKS
    * the inventory block in docs/ci-status.md names, for every workflow that
      runs a cargo command, the jobs that cannot pass — and those names match
      the workflow's own `jobs:` keys, so adding a job to tests.yml forces the
      record to be updated;
    * no inventory entry names a workflow that does not exist;
    * NO workflow names the graph (sandbox / agtrs / injectable) while the
      record says none does. Implement one of the three real options and this
      check fails until the record is rewritten in the same change;
    * the workspace graph still does not resolve. A pristine-checkout
      `cargo metadata` that SUCCEEDS means the reason this page exists is gone;
      the check then fails and asks for the workflows to be re-evaluated rather
      than leaving a stale "CI is knowingly red" claim in the tree;
    * the page still records the `declined-with-reason` decision itself.

    The probe is SKIPPED, with a printed note, while the build sandbox is
    engaged: then the graph resolves locally for a reason a CI runner cannot
    use, and a check that could not tell those two apart would fire on every
    sandboxed run.

USAGE
    scripts/ci-graph-status.py                 # print the derived state
    scripts/ci-graph-status.py --check         # exit 1 on any problem
    scripts/ci-graph-status.py --self-test     # exit 1 if a drift mode is
                                               # undetectable
    scripts/ci-graph-status.py --graph-resolves yes|no|skip
                                               # override the cargo probe
    scripts/ci-graph-status.py --workflows-dir DIR --doc FILE   # retarget

EXIT
    0  the record matches the repository
    1  a problem above (only with --check)
    2  a file could not be read or parsed
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS_DIR = ROOT / ".github" / "workflows"
DOC = ROOT / "docs" / "ci-status.md"

# A cargo INVOCATION, not the word "cargo": the hyphenated tool names this repo
# uses in prose (`cargo-tarpaulin`) must not classify a workflow as needing the
# graph, and neither must a comment.
RAW_CARGO = re.compile(
    r"\bcargo\s+(?:test|check|build|clippy|fmt|tarpaulin|doc|run|install|package|publish)\b"
)

# A step (or a comment) that makes the workspace graph resolvable. Deliberately
# textual: this page's claim is that NOTHING in the workflows names the graph,
# so a mention is the thing to catch, wherever it appears.
GRAPH_STEP = re.compile(r"sandbox|agtrs|injectable")

# The decision marker this page must keep recording.
DECISION = "declined-with-reason"

# The build sandbox's marker inside a manifest. If it is present anywhere, the
# tree is mid-sandbox and the graph probe would measure the clone, not the
# committed state.
SANDBOX_MARKER = "xaft-agtrs-sandbox"

INVENTORY_LANG = "ci-status"
INVENTORY_HEADER = re.compile(r"^([A-Za-z0-9._-]+\.ya?ml)\s*:\s*(.*)$")


# ── parsing ──────────────────────────────────────────────────────────────────


def strip_comment(line: str) -> str:
    """Drop a trailing YAML comment. Good enough for these hand-written files."""
    marker = line.find(" #")
    return line if marker < 0 else line[:marker]


def jobs_by_name(text: str) -> dict[str, list[str]]:
    """Map each job key under `jobs:` to its non-comment, de-commented lines.

    Textual rather than a YAML parse on purpose: it keeps this script
    dependency-free (CI runners and contributor machines alike), and the files
    it reads are two-space-indented by hand.
    """
    jobs: dict[str, list[str]] = {}
    current: str | None = None
    in_jobs = False
    for raw in text.splitlines():
        if not in_jobs:
            if re.match(r"^jobs:\s*$", raw):
                in_jobs = True
            continue
        if raw[:1] and not raw[:1].isspace():
            break
        m = re.match(r"^  ([^\s:#]+):\s*$", raw)
        if m:
            current = m.group(1)
            jobs[current] = []
            continue
        if current is not None:
            body = strip_comment(raw)
            if body.strip():
                jobs[current].append(body)
    return jobs


def graph_jobs(text: str) -> list[str]:
    """The jobs whose body invokes cargo — the ones the record must account for."""
    out = []
    for name, lines in jobs_by_name(text).items():
        if any(RAW_CARGO.search(line) for line in lines):
            out.append(name)
    return out


def inventory(doc: str) -> list[tuple[str, list[str]]] | None:
    """The ```ci-status fence of docs/ci-status.md as [(workflow, [jobs])]."""
    entries: list[tuple[str, list[str]]] = []
    inside = False
    seen_fence = False
    for line in doc.splitlines():
        if not inside:
            if re.match(rf"^```{INVENTORY_LANG}\s*$", line):
                inside, seen_fence = True, True
            continue
        if re.match(r"^```\s*$", line):
            inside = False
            continue
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        m = INVENTORY_HEADER.match(stripped)
        if not m:
            return None
        jobs = [j.strip() for j in m.group(2).split(",") if j.strip()]
        entries.append((m.group(1), jobs))
    return entries if seen_fence else None


# ── the graph probe ──────────────────────────────────────────────────────────


def manifests(root: Path) -> list[Path]:
    found = [root / "Cargo.toml"]
    crates = root / "crates"
    if crates.is_dir():
        found.extend(sorted(crates.glob("*/Cargo.toml")))
    return [p for p in found if p.is_file()]


def sandbox_engaged(root: Path) -> bool:
    for path in manifests(root):
        try:
            if SANDBOX_MARKER in path.read_text(encoding="utf-8", errors="replace"):
                return True
        except OSError:
            continue
    return False


def probe_graph(root: Path) -> tuple[bool | None, str]:
    """(resolves, note). None means 'unknown — do not judge'."""
    if sandbox_engaged(root):
        return None, (
            "build sandbox is ENGAGED, so the graph resolves locally for a reason "
            "a CI runner cannot use — probe skipped"
        )
    try:
        done = subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=300,
        )
    except FileNotFoundError:
        return None, "cargo is not on PATH — probe skipped"
    except subprocess.TimeoutExpired:
        return None, "cargo metadata timed out — probe skipped"
    return done.returncode == 0, f"cargo metadata exit {done.returncode}"


# ── the check ────────────────────────────────────────────────────────────────


def check(
    workflows: dict[str, str],
    doc: str,
    graph_resolves: bool | None,
) -> tuple[list[str], dict]:
    """Return (problems, facts) for the workflow texts and the record's text."""
    problems: list[str] = []
    facts: dict = {"needs_graph": {}, "unaffected": [], "graph_named_by": []}

    for name, text in sorted(workflows.items()):
        needy = graph_jobs(text)
        if needy:
            facts["needs_graph"][name] = needy
        else:
            facts["unaffected"].append(name)
        if GRAPH_STEP.search(text):
            facts["graph_named_by"].append(name)

    entries = inventory(doc)
    if entries is None:
        problems.append(
            f"{DOC.name} carries no ```{INVENTORY_LANG} inventory block (or it is "
            "malformed); the record has no machine-readable claim to check"
        )
        entries = []
    recorded = {name: jobs for name, jobs in entries}

    for name in sorted(set(recorded) - set(workflows)):
        problems.append(
            f"the inventory block names {name}, which is not a workflow in "
            f"{WORKFLOWS_DIR.name}/ — the record lists a file that does not exist"
        )

    for name, needy in sorted(facts["needs_graph"].items()):
        if name not in recorded:
            problems.append(
                f"{name} runs cargo (job(s) {', '.join(needy)}) but the inventory "
                f"block in {DOC.name} does not account for it"
            )
            continue
        listed, missing = set(recorded[name]), set(needy) - set(recorded[name])
        extra = listed - set(needy)
        if missing:
            problems.append(
                f"{name}: job(s) {', '.join(sorted(missing))} run cargo but are "
                f"missing from {DOC.name}'s inventory block — the record is stale"
            )
        if extra:
            problems.append(
                f"{name}: {DOC.name}'s inventory block lists job(s) "
                f"{', '.join(sorted(extra))}, which do not run cargo in that file"
            )

    if facts["graph_named_by"]:
        problems.append(
            "workflow(s) "
            + ", ".join(facts["graph_named_by"])
            + " now name the workspace graph (sandbox/agtrs/injectable) while "
            + f"{DOC.name} records that none does — update the decision record and "
            "this check in the same change"
        )

    facts["graph_resolves"] = graph_resolves
    if graph_resolves is True:
        problems.append(
            "the workspace graph RESOLVES on a pristine checkout, so no CI job "
            "fails for the reason recorded in "
            f"{DOC.name}. Re-evaluate .github/workflows/: the jobs may now be "
            "green as written, and the page's 'CI is knowingly red' claim is false "
            "until it is rewritten"
        )

    if DECISION not in doc:
        problems.append(
            f"{DOC.name} no longer records the `{DECISION}` decision it exists to "
            "record"
        )

    return problems, facts


def report(facts: dict, problems: list[str], note: str) -> None:
    print("CI graph status\n")
    for name, jobs in sorted(facts.get("needs_graph", {}).items()):
        print(f"  needs the graph   {name:16} jobs: {', '.join(jobs)}")
    for name in facts.get("unaffected", []):
        print(f"  unaffected        {name}")
    resolves = facts.get("graph_resolves")
    state = {True: "RESOLVES", False: "does not resolve", None: "not probed"}[resolves]
    print(f"\n  pristine graph    {state}  ({note})")
    if facts.get("graph_named_by"):
        print(f"  graph named by    {', '.join(facts['graph_named_by'])}")
    print()

    if problems:
        print(f"{len(problems)} problem(s):")
        for p in problems:
            print(f"  - {p}")
        return

    if resolves is None:
        print(
            "The record matches the workflows, and the graph probe was skipped, so\n"
            "the claim itself is unverified this run — re-check with the sandbox off."
        )
    else:
        print(
            "The record matches the workflows: every cargo job that needs the\n"
            "workspace graph is accounted for, no workflow provides one, and the\n"
            "graph does not resolve on a pristine checkout — so CI cannot build\n"
            "xaft yet, exactly as docs/ci-status.md states."
        )


# ── self-test ────────────────────────────────────────────────────────────────


def self_test(workflows: dict[str, str], doc: str) -> int:
    """Every drift mode this script claims to police must actually fail."""
    problems, _ = check(workflows, doc, False)
    if problems:
        print("self-test: FAIL — the real files are already inconsistent:")
        for p in problems:
            print(f"  - {p}")
        return 1

    def with_workflow(name: str, needle: str, insert: str):
        def mutate(w, d, r):
            w = dict(w)
            w[name] = w[name].replace(needle, needle + insert, 1)
            return w, d, r

        return mutate

    def add_job(name: str, job: str, body: str):
        """Insert a new job, with its own body lines, after a file's `jobs:`."""

        def mutate(w, d, r):
            w = dict(w)
            w[name] = w[name].replace("\njobs:\n", f"\njobs:\n  {job}:\n{body}", 1)
            return w, d, r

        return mutate

    mutations = [
        (
            "a workflow starts engaging the agtrs sandbox",
            with_workflow(
                "tests.yml",
                "    steps:\n",
                "      - run: ./scripts/sandbox-build.sh  # agtrs\n",
            ),
        ),
        (
            "a job that runs cargo is added to a listed workflow",
            add_job(
                "tests.yml",
                "nightly",
                "    runs-on: ubuntu-latest\n"
                "    steps:\n"
                "      - run: cargo test --workspace\n",
            ),
        ),
        (
            "a workflow that runs cargo is dropped from the record",
            lambda w, d, r: (w, d.replace("codeql.yml: analyze\n", "", 1), r),
        ),
        (
            "a workflow that did not run cargo starts running it",
            with_workflow("docs.yml", "    steps:\n", "      - run: cargo test --workspace\n"),
        ),
        (
            "the record lists a workflow that does not exist",
            lambda w, d, r: (w, d.replace("tests.yml: test", "nightly.yml: test", 1), r),
        ),
        (
            "the graph resolves while the record says it cannot",
            lambda w, d, r: (w, d, True),
        ),
        (
            "the recorded decision disappears from the page",
            lambda w, d, r: (w, d.replace("declined-with-reason", "some other thing"), r),
        ),
    ]

    failed = 0
    for label, mutate in mutations:
        mutated = mutate(workflows, doc, False)
        if mutated == (workflows, doc, False):
            print(f"self-test: FAIL — mutation {label!r} did not change anything")
            failed += 1
            continue
        w, d, r = mutated
        probs, _ = check(w, d, r)
        if probs:
            print(f"self-test: ok — {label} is caught")
        else:
            print(f"self-test: FAIL — {label} drifted but was NOT caught")
            failed += 1

    # Negative control: a job added to a LISTED workflow that does not run cargo
    # is not drift. The record only claims which jobs need the graph, and the
    # page says so explicitly, so flagging it would make this check fire on
    # every unrelated workflow change.
    w, d, r = add_job(
        "tests.yml",
        "notify",
        "    runs-on: ubuntu-latest\n    steps:\n      - run: echo done\n",
    )(workflows, doc, False)
    probs, _ = check(w, d, r)
    if probs:
        print(
            "self-test: FAIL \u2014 a job that does not run cargo was reported as "
            f"drift, so the check fires on unrelated workflow changes: {probs[0]}"
        )
        failed += 1
    else:
        print("self-test: ok \u2014 a job that does not run cargo is not drift")

    # Negative control: a SKIPPED probe must not be treated as a stale record,
    # or the check would fire on every sandboxed run.
    probs, _ = check(workflows, doc, None)
    if probs:
        print(
            "self-test: FAIL — a skipped graph probe was treated as a problem, so "
            f"the check cannot run while the sandbox is engaged: {probs[0]}"
        )
        failed += 1
    else:
        print("self-test: ok — a skipped graph probe is not a problem")

    if failed:
        print(f"\nself-test: {failed} check(s) cannot detect drift")
        return 1
    print(
        f"\nself-test: all {len(mutations)} drift modes are detected, and both "
        "negative controls stay quiet"
    )
    return 0


# ── entry point ──────────────────────────────────────────────────────────────


def main() -> int:
    args = list(sys.argv[1:])
    workflows_dir = WORKFLOWS_DIR
    doc_path = DOC
    override: bool | None = None
    explicit = False

    if "--workflows-dir" in args:
        workflows_dir = Path(args[args.index("--workflows-dir") + 1])
    if "--doc" in args:
        doc_path = Path(args[args.index("--doc") + 1])
    if "--graph-resolves" in args:
        value = args[args.index("--graph-resolves") + 1].lower()
        explicit = True
        override = {"yes": True, "true": True, "no": False, "false": False}.get(value)

    if not workflows_dir.is_dir():
        print(f"error: no workflows directory: {workflows_dir}", file=sys.stderr)
        return 2
    if not doc_path.is_file():
        print(f"error: no decision record: {doc_path}", file=sys.stderr)
        return 2

    workflows = {
        path.name: path.read_text(encoding="utf-8")
        for path in sorted(workflows_dir.iterdir())
        if path.suffix in (".yml", ".yaml") and path.is_file()
    }
    if not workflows:
        print(f"error: no workflow files in {workflows_dir}", file=sys.stderr)
        return 2
    doc = doc_path.read_text(encoding="utf-8")

    if "--self-test" in args:
        return self_test(workflows, doc)

    if explicit:
        resolves, note = override, "overridden on the command line"
    else:
        resolves, note = probe_graph(workflows_dir.resolve().parent)

    problems, facts = check(workflows, doc, resolves)
    report(facts, problems, note)

    if problems:
        return 1 if "--check" in args else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
