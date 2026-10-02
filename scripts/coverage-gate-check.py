#!/usr/bin/env python3
"""
coverage-gate-check.py — assert CI's coverage gate and `just coverage` agree.

WHY
    The project-wide line-coverage gate has THREE independent homes:

      * `.github/workflows/coverage.yml` runs it on every push, threshold in
        `env.COVERAGE_MIN`;
      * the `coverage` recipe in the justfile is the developer-facing gate;
      * `docs/coverage-baseline.md` is the record of the measurement.

    Those three were written at different times by different passes and nothing
    connected them, which produced two quiet defects:

      1. A gate that is WIRED but NOT ARMED is decorative. The workflow passes
         `--fail-under "${COVERAGE_MIN}"` with `COVERAGE_MIN: "0"`, so it
         measures on every push and can never fail. If the coverage work closes
         the gap and nobody edits that one line, the project keeps a green
         "coverage" job that enforces nothing — while `just coverage` still
         hardcodes `--fail-under 85`. Local and CI then disagree about the
         Definition of Done and nothing says so.
      2. The two invocations can drift on their FLAGS independently of the
         threshold. `--timeout`, `--engine`, `--out` and `--skip-clean` are
         duplicated in three places (the workflow, the justfile recipe, and the
         recipe as quoted inside docs/coverage-baseline.md), and a mismatch
         between what CI measures and what a developer measures locally is
         invisible until the numbers disagree.

    Both defects are of the class this repository keeps rediscovering: a
    hand-maintained cross-reference that nothing re-derives. This script is the
    re-derivation, in the same spirit as scripts/parity-matrix-stats.py and
    scripts/cli-row-ownership.py.

WHAT IT DOES
    Parses the three files above and asserts:

      * every tarpaulin FLAG (i.e. everything except `--fail-under`) is
        identical in all three, so CI measures what `just coverage` measures;
      * the threshold is set in EXACTLY ONE place — the workflow's run block
        must interpolate `${COVERAGE_MIN}`, never a second hardcoded literal;
      * the two thresholds are either identical, or the workflow is explicitly
        disarmed (`COVERAGE_MIN: "0"`) AND the recorded measurement is below
        the target. Arming CI while the tree is below the target is an error,
        not a bonus: it is a red check on every commit, which trains
        contributors to ignore CI.
      * THE TRIPWIRE: if the percentage recorded in docs/coverage-baseline.md
        reaches or passes the target, a disarmed workflow FAILS the check with
        an explicit instruction to set `COVERAGE_MIN` to the target. The
        coverage batches are required to refresh that document, so this is how
        "G11b landed and nobody flipped the line" turns from silence into a
        red check.
      * the recorded measurement is self-consistent (the header's percentage
        agrees with its covered/coverable counts and with the workspace-total
        row of the per-crate table), so the tripwire cannot fire — or stay
        silent — because the document contradicts itself.

    The tripwire reads docs/coverage-baseline.md's `Status:` line because that
    page was deliberately reduced to a SINGLE status statement; if that sentence
    is wrong, the tripwire is wrong, which is why its internal consistency is
    itself checked.

USAGE
    scripts/coverage-gate-check.py            # print the derived gate state
    scripts/coverage-gate-check.py --check    # exit 1 on any problem
    scripts/coverage-gate-check.py --self-test
                                             # exit 1 if a drift mode is
                                             # undetectable

EXIT
    0  CI and `just coverage` agree, and the gate's armed state is justified
    1  a problem above (only with --check)
    2  a file could not be read or parsed
"""
from __future__ import annotations

import math
import re
import shlex
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_WORKFLOW = ROOT / ".github" / "workflows" / "coverage.yml"
DEFAULT_JUSTFILE = ROOT / "justfile"
DEFAULT_DOC = ROOT / "docs" / "coverage-baseline.md"

# The measurement record the disarm tripwire is keyed to. The workflow's
# comment block must name it, so the reason for disarming always points at the
# evidence instead of living only in a diff.
BASELINE_DOC_REF = "docs/coverage-baseline.md"

# `--fail-under` is the one flag allowed to differ between the three homes: it
# is the threshold, not a measurement option.
THRESHOLD_FLAG = "--fail-under"


# ── parsing ───────────────────────────────────────────────────────────────────


def workflow_env(text: str) -> dict[str, str]:
    """Return the workflow's top-level `env:` block as {KEY: value}.

    Textual rather than a YAML parse on purpose: it keeps this script
    dependency-free (CI runners and contributor machines alike) and the block
    it reads is four lines of `KEY: "value"` written by this repository.
    """
    env: dict[str, str] = {}
    in_env = False
    for line in text.splitlines():
        if not in_env:
            if re.match(r"^env:\s*$", line):
                in_env = True
            continue
        if line.strip() and not line[:1].isspace():
            break
        m = re.match(r'^\s+([A-Za-z_][A-Za-z0-9_]*):\s*"([^"]*)"\s*$', line)
        if m:
            env[m.group(1)] = m.group(2)
    return env


def comment_block_above(text: str, key: str) -> str:
    """The contiguous `#` comment block immediately above `KEY:` in a YAML map."""
    lines = text.splitlines()
    out: list[str] = []
    for i, line in enumerate(lines):
        if not re.match(rf"^\s+{re.escape(key)}:", line):
            continue
        j = i - 1
        while j >= 0 and re.match(r"^\s*#", lines[j]):
            out.append(lines[j])
            j -= 1
        return "\n".join(reversed(out))
    return ""


def just_recipe(justfile: str, name: str) -> list[str]:
    """Body lines of the `name:` recipe, comments and blank lines included."""
    body: list[str] = []
    seen = False
    for line in justfile.splitlines():
        if not seen:
            if re.match(rf"^{re.escape(name)}:(.*)$", line):
                seen = True
            continue
        if line.strip() == "" or line[:1] in (" ", "\t"):
            body.append(line)
            continue
        break
    return body


def just_fenced_blocks(doc: str, language: str) -> list[str]:
    """Body of every ```<language> fence, in document order."""
    blocks: list[str] = []
    fence = re.compile(rf"^```{re.escape(language)}\s*$")
    opener = re.compile(r"^```\s*$")
    inside = False
    buf: list[str] = []
    for line in doc.splitlines():
        if not inside and fence.match(line):
            inside, buf = True, []
            continue
        if inside and opener.match(line):
            blocks.append("\n".join(buf))
            inside = False
            continue
        if inside:
            buf.append(line)
    return blocks


def tarpaulin_command(lines: list[str]) -> str:
    """The one `cargo tarpaulin` command among `lines`, continuations joined.

    Comment and blank lines are skipped. Returns "" if there is none, which is
    itself reportable: a missing invocation is how the gate goes missing.
    """
    parts: list[str] = []
    started = False
    for raw in lines:
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if not started:
            if not (stripped == "cargo tarpaulin" or stripped.startswith("cargo tarpaulin ")):
                continue
            started = True
        parts.append(stripped.rstrip("\\").strip())
        if not raw.rstrip().endswith("\\"):
            break
    return " ".join(parts)


def flags_of(command: str, env: dict[str, str] | None = None) -> dict[str, str]:
    """Parse a command into {flag: value}; a valueless flag maps to "".

    Handles both `--flag value` (what this repo writes) and `--flag=value`.
    """
    if not command:
        return {}
    text = command
    if env:
        text = re.sub(
            r"\$\{(\w+)\}", lambda m: env.get(m.group(1), m.group(0)), text
        )
    try:
        tokens = shlex.split(text)
    except ValueError:
        return {}
    out: dict[str, str] = {}
    i = 0
    while i < len(tokens):
        token = tokens[i]
        if token.startswith("--"):
            if "=" in token:
                key, value = token.split("=", 1)
                out[key] = value
            elif i + 1 < len(tokens) and not tokens[i + 1].startswith("--"):
                out[token] = tokens[i + 1]
                i += 1
            else:
                out[token] = ""
        i += 1
    return out


def as_percent(value: str | None) -> float | None:
    if value is None:
        return None
    m = re.fullmatch(r"\s*([0-9]+(?:\.[0-9]+)?)\s*%?\s*", value)
    return float(m.group(1)) if m else None


def doc_measurement(doc: str) -> dict[str, float | None]:
    """The coverage figures recorded in docs/coverage-baseline.md's Status line.

    That page keeps exactly ONE status statement; this is it.
    """
    out: dict[str, float | None] = {
        "pct": None,
        "covered": None,
        "coverable": None,
        "target": None,
    }
    m = re.search(r"Status:\s*\*(.+?)\*", doc, re.S)
    if not m:
        return out
    body = " ".join(m.group(1).split())
    pct = re.search(r"MEASURED\s*[—–-]\s*([0-9]+(?:\.[0-9]+)?)%", body)
    counts = re.search(
        r"\(\s*([0-9]+)\s+of\s+([0-9]+)\s+coverable lines\s*\)", body
    )
    target = re.search(r"([0-9]+(?:\.[0-9]+)?)%\s+target", body)
    if pct:
        out["pct"] = float(pct.group(1))
    if counts:
        out["covered"] = float(counts.group(1))
        out["coverable"] = float(counts.group(2))
    if target:
        out["target"] = float(target.group(1))
    return out


def doc_table_total(doc: str) -> dict[str, float | None] | None:
    """The `**workspace total**` row of the per-crate table."""
    for line in doc.splitlines():
        if "workspace total" not in line or not line.strip().startswith("|"):
            continue
        cells = [c.strip().strip("*").strip() for c in line.strip().strip("|").split("|")]
        numbers = [c for c in cells if re.fullmatch(r"[0-9]+(?:\.[0-9]+)?%?", c)]
        if len(numbers) < 3:
            return None
        covered, coverable, pct = numbers[0], numbers[1], numbers[2]
        return {
            "covered": float(covered.rstrip("%")),
            "coverable": float(coverable.rstrip("%")),
            "pct": float(pct.rstrip("%")),
        }
    return None


# ── the check ─────────────────────────────────────────────────────────────────


def check(workflow: str, justfile: str, doc: str) -> tuple[list[str], dict]:
    """Return (problems, facts) for the three file contents."""
    problems: list[str] = []
    facts: dict = {}

    env = workflow_env(workflow)
    ci_raw_command = tarpaulin_command(workflow.splitlines())
    just_command = tarpaulin_command(just_recipe(justfile, "coverage"))
    doc_commands = [tarpaulin_command(b.splitlines()) for b in just_fenced_blocks(doc, "just")]
    doc_command = next((c for c in doc_commands if c), "")

    for name, command in (
        (".github/workflows/coverage.yml", ci_raw_command),
        ("justfile `coverage` recipe", just_command),
        ("docs/coverage-baseline.md's quoted recipe", doc_command),
    ):
        if not command:
            problems.append(f"no `cargo tarpaulin` invocation found in {name}")

    ci_flags = flags_of(ci_raw_command, env)
    just_flags = flags_of(just_command)
    doc_flags = flags_of(doc_command)

    # ── one threshold, in one place ───────────────────────────────────────────
    # The workflow's run block must interpolate the env var. A hardcoded
    # literal there is the same "one knob, two values" bug as two knobs.
    raw_threshold = flags_of(ci_raw_command).get(THRESHOLD_FLAG)
    if ci_raw_command and raw_threshold is None:
        problems.append(
            ".github/workflows/coverage.yml runs cargo tarpaulin with no "
            f"`{THRESHOLD_FLAG}`: the gate is not wired at all"
        )
    elif raw_threshold is not None and raw_threshold.strip('"') != "${COVERAGE_MIN}":
        problems.append(
            ".github/workflows/coverage.yml must pass `--fail-under "
            '"${COVERAGE_MIN}"` so the threshold is set in exactly one place; '
            f"it passes a second literal `{raw_threshold}`"
        )

    # ── flag parity ───────────────────────────────────────────────────────────
    def without_threshold(flags: dict[str, str]) -> dict[str, str]:
        return {k: v for k, v in flags.items() if k != THRESHOLD_FLAG}

    if ci_flags and just_flags:
        a, b = without_threshold(ci_flags), without_threshold(just_flags)
        if a != b:
            only_ci = sorted(set(a) - set(b))
            only_just = sorted(set(b) - set(a))
            changed = sorted(k for k in set(a) & set(b) if a[k] != b[k])
            detail = []
            if only_ci:
                detail.append(f"only in the workflow: {', '.join(only_ci)}")
            if only_just:
                detail.append(f"only in `just coverage`: {', '.join(only_just)}")
            if changed:
                detail.append(
                    "differing values: "
                    + ", ".join(f"{k} ({a[k]!r} vs {b[k]!r})" for k in changed)
                )
            problems.append(
                "the coverage flags have drifted between CI and `just coverage` "
                f"({'; '.join(detail)}) — CI must measure what a developer measures"
            )
    if just_flags and doc_flags and without_threshold(doc_flags) != without_threshold(just_flags):
        problems.append(
            "the tarpaulin command quoted in docs/coverage-baseline.md no longer "
            "matches the justfile's `coverage` recipe; update the quoted block "
            "when the recipe's flags change"
        )

    # ── the threshold itself ──────────────────────────────────────────────────
    target = as_percent(just_flags.get(THRESHOLD_FLAG))
    ci_min_raw = env.get("COVERAGE_MIN")
    ci_min = as_percent(ci_min_raw)
    measured = doc_measurement(doc)
    table = doc_table_total(doc)

    facts.update(
        {
            "env": env,
            "ci_command": ci_raw_command,
            "just_command": just_command,
            "doc_command": doc_command,
            "ci_min": ci_min,
            "target": target,
            "measured": measured,
            "table_total": table,
            "state": "UNKNOWN",
        }
    )

    if target is None:
        facts["state"] = "NO TARGET"
        problems.append(
            "the justfile `coverage` recipe no longer passes a numeric "
            f"`{THRESHOLD_FLAG}`, so there is no enforced threshold to compare against"
        )
        return problems, facts

    if ci_min is None:
        problems.append(
            "`.github/workflows/coverage.yml` declares no numeric `env.COVERAGE_MIN`"
        )
        return problems, facts

    # ── the recorded measurement must be self-consistent ──────────────────────
    # The tripwire is keyed to this number; if the document contradicts itself,
    # the tripwire fires wrongly or not at all.
    pct = measured["pct"]
    covered = measured["covered"]
    coverable = measured["coverable"]
    if pct is None:
        problems.append(
            f"{BASELINE_DOC_REF}'s Status line states no measured percentage; the "
            "disarm tripwire has no input and cannot fire"
        )
    else:
        if covered is not None and coverable:
            computed = 100.0 * covered / coverable
            if round(computed, 2) != round(pct, 2):
                problems.append(
                    f"{BASELINE_DOC_REF} is self-inconsistent: the Status line claims "
                    f"{pct:g}% but {covered:g}/{coverable:g} is {computed:.2f}%"
                )
        if table is not None:
            if covered is not None and table["covered"] != covered:
                problems.append(
                    f"{BASELINE_DOC_REF} is self-inconsistent: the Status line reports "
                    f"{covered:g} covered lines but the per-crate table's workspace "
                    f"total row reports {table['covered']:g}"
                )
            if coverable and table["coverable"] != coverable:
                problems.append(
                    f"{BASELINE_DOC_REF} is self-inconsistent: the Status line reports "
                    f"{coverable:g} coverable lines but the per-crate table's workspace "
                    f"total row reports {table['coverable']:g}"
                )
            if round(table["pct"] or 0.0, 2) != round(pct, 2):
                problems.append(
                    f"{BASELINE_DOC_REF} is self-inconsistent: the Status line claims "
                    f"{pct:g}% but the per-crate table's workspace total row claims "
                    f"{table['pct']:g}%"
                )
    if measured["target"] is not None and measured["target"] != target:
        problems.append(
            f"{BASELINE_DOC_REF} states a {measured['target']:g}% target but "
            f"`just coverage` enforces {target:g}%"
        )

    facts["pct"] = pct
    if pct is None:
        return problems, facts

    # ── armed, disarmed, or the tripwire ──────────────────────────────────────
    block = comment_block_above(workflow, "COVERAGE_MIN")
    if pct >= target:
        facts["state"] = "ARMED" if ci_min == target else "SHOULD BE ARMED"
        if ci_min != target:
            problems.append(
                f"coverage is AT OR ABOVE the gate ({pct:g}% >= {target:g}%) but CI "
                f"still runs `--fail-under {ci_min_raw}`. Set "
                f"`COVERAGE_MIN: \"{target:g}\"` in .github/workflows/coverage.yml and "
                "rewrite the comment above it to state the threshold and the commit "
                f"that reached it. This is the check that exists so the gate cannot "
                "stay wired-but-unarmed."
            )
        elif str(int(target)) not in block and f"{target:g}" not in block:
            problems.append(
                "CI is armed, but the comment above COVERAGE_MIN does not state the "
                f"threshold ({target:g})"
            )
        if re.search(r"currently\s+0", block, re.IGNORECASE):
            problems.append(
                "CI is armed, but the comment above COVERAGE_MIN still says the "
                "value is 0 — it was not updated when the gate was armed"
            )
    elif ci_min == 0:
        facts["state"] = "REPORTING (deliberately disarmed)"
        if BASELINE_DOC_REF not in block:
            problems.append(
                "the gate is disarmed, but the comment above COVERAGE_MIN does not "
                f"point at {BASELINE_DOC_REF} as the record of the measurement"
            )
    elif ci_min == target:
        facts["state"] = "ARMED BELOW TARGET"
        problems.append(
            f"CI enforces `--fail-under {target:g}` while the recorded coverage is "
            f"{pct:g}% — every push would fail. Arm the gate only once the "
            f"measurement reaches the target; until then keep `COVERAGE_MIN: \"0\"`."
        )
    else:
        facts["state"] = "INCONSISTENT"
        problems.append(
            f"CI enforces `--fail-under {ci_min_raw}`, which is neither 0 "
            f"(disarmed) nor the justfile's {target:g}"
        )

    return problems, facts


def report(facts: dict, problems: list[str]) -> None:
    print("coverage gate\n")
    print(f"  justfile `just coverage`  {facts.get('just_command') or '(none)'}")
    print(f"  CI        coverage.yml    {facts.get('ci_command') or '(none)'}")
    pct = facts.get("pct")
    target = facts.get("target")
    measured = facts.get("measured", {})
    if pct is not None and measured.get("covered") is not None:
        detail = f"{pct:g}% ({measured['covered']:g}/{measured['coverable']:g})"
        if target:
            need = math.ceil(target / 100.0 * measured["coverable"]) - measured["covered"]
            detail += f", +{need:g} covered lines to reach {target:g}%"
        print(f"  recorded                 {detail}")
    elif target is not None:
        print(f"  target                   {target:g}%")
    print(f"  state                    {facts.get('state')}\n")

    if problems:
        print(f"{len(problems)} problem(s):")
        for p in problems:
            print(f"  - {p}")
        return

    state = facts.get("state", "")
    if state.startswith("REPORTING"):
        print(
            "CI measures exactly what `just coverage` measures, and the gate is "
            "disarmed on purpose:\n"
            f"  COVERAGE_MIN is 0 while the recorded {pct:g}% is below the "
            f"{target:g}% target.\n"
            "The tripwire is this same check: once the measurement recorded in "
            f"{BASELINE_DOC_REF}\nreaches the target it fails and names the one line "
            "to change, so the gate cannot\nstay wired-but-unarmed."
        )
    else:
        print(
            "CI measures exactly what `just coverage` measures and enforces the "
            f"same {target:g}% threshold."
        )


# ── self-test ─────────────────────────────────────────────────────────────────


def self_test(workflow: str, justfile: str, doc: str) -> int:
    """Every drift mode this script claims to police must actually fail."""
    problems, _ = check(workflow, justfile, doc)
    if problems:
        print("self-test: FAIL — the real files are already inconsistent:")
        for p in problems:
            print(f"  - {p}")
        return 1

    mutations = [
        (
            "the workflow's flags drift from `just coverage`",
            # Anchored on the trailing backslash so it hits the command line and
            # not the prose above it, which also mentions `--timeout 300`.
            lambda w, j, d: (
                w.replace("            --timeout 300 \\\n", "            --timeout 60 \\\n", 1),
                j,
                d,
            ),
        ),
        (
            "a flag the workflow passes is dropped from `just coverage`",
            lambda w, j, d: (w, j.replace("        --skip-clean \\\n", "", 1), d),
        ),
        (
            "the workflow's threshold stops coming from COVERAGE_MIN",
            lambda w, j, d: (
                w.replace('--fail-under "${COVERAGE_MIN}"', "--fail-under 85", 1),
                j,
                d,
            ),
        ),
        (
            "CI is armed while the tree is still below the target",
            lambda w, j, d: (w.replace('COVERAGE_MIN: "0"', 'COVERAGE_MIN: "85"', 1), j, d),
        ),
        (
            "the recorded measurement reaches the target and nobody armed CI",
            lambda w, j, d: (
                w,
                j,
                d.replace("MEASURED — 77.29% (11141 of 14415 coverable lines)",
                         "MEASURED — 86.40% (12455 of 14415 coverable lines)", 1)
                 .replace("| **workspace total** | **11141** | **14415** | **77.29%**",
                          "| **workspace total** | **12455** | **14415** | **86.40%**", 1),
            ),
        ),
        (
            "CI is armed and the comment above it still claims it is not",
            lambda w, j, d: (
                w.replace('COVERAGE_MIN: "0"', 'COVERAGE_MIN: "85"', 1),
                j,
                d.replace("MEASURED — 77.29% (11141 of 14415 coverable lines)",
                         "MEASURED — 86.40% (12455 of 14415 coverable lines)", 1)
                 .replace("| **workspace total** | **11141** | **14415** | **77.29%**",
                          "| **workspace total** | **12455** | **14415** | **86.40%**", 1),
            ),
        ),
        (
            "the disarm justification stops naming the baseline document",
            lambda w, j, d: (w.replace("docs/coverage-baseline.md", "a spreadsheet"), j, d),
        ),
        (
            "the header percentage contradicts its own line counts",
            lambda w, j, d: (w, j, d.replace("MEASURED — 77.29%", "MEASURED — 71.00%", 1)),
        ),
        (
            "the header and the per-crate total row disagree",
            lambda w, j, d: (
                w,
                j,
                d.replace("| **workspace total** | **11141** |",
                         "| **workspace total** | **11000** |", 1),
            ),
        ),
        (
            "the justfile's target moves and the document does not",
            lambda w, j, d: (w, j.replace("        --fail-under 85", "        --fail-under 90", 1), d),
        ),
        (
            "the recipe quoted in the baseline document loses a flag",
            lambda w, j, d: (
                w,
                j,
                d.replace("        --out Json \\\n        --fail-under 85",
                          "        --fail-under 85", 1),
            ),
        ),
        (
            "the workflow stops wiring the gate at all",
            lambda w, j, d: (w.replace('            --fail-under "${COVERAGE_MIN}"\n', "", 1), j, d),
        ),
    ]

    failed = 0
    for label, mutate in mutations:
        mutated = mutate(workflow, justfile, doc)
        if mutated == (workflow, justfile, doc):
            print(f"self-test: FAIL — mutation {label!r} did not change anything")
            failed += 1
            continue
        probs, _ = check(*mutated)
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
    paths = {
        "workflow": DEFAULT_WORKFLOW,
        "justfile": DEFAULT_JUSTFILE,
        "doc": DEFAULT_DOC,
    }
    for key in list(paths):
        flag = f"--{key}"
        if flag in args:
            paths[key] = Path(args[args.index(flag) + 1])

    texts = {}
    for key, path in paths.items():
        if not path.is_file():
            print(f"error: {key} not found: {path}", file=sys.stderr)
            return 2
        texts[key] = path.read_text(encoding="utf-8")

    if not tarpaulin_command(texts["workflow"].splitlines()):
        print(f"error: parsed no coverage invocation from {paths['workflow']}", file=sys.stderr)
        return 2

    if "--self-test" in args:
        return self_test(texts["workflow"], texts["justfile"], texts["doc"])

    problems, facts = check(texts["workflow"], texts["justfile"], texts["doc"])
    report(facts, problems)

    if problems:
        return 1 if "--check" in args else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
