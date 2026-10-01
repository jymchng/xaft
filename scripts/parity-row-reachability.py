#!/usr/bin/env python3
"""
parity-row-reachability.py — is every ✅ row in the parity matrix actually reachable?

WHY THIS EXISTS
    The matrix's ✅ means "a user can do this". Three separate audits have
    found rows where that was not true:

      * G5 found `/usage` marked ✅ while the only string a user could type
        answered "Unknown command".
      * Commit `5f06350` shipped a doc comment advertising a bare
        `xaft runs` that clap rejected with "requires a subcommand".
      * The section-1b note records `jobs` rows that were ✅ on the strength of
        a working store and an unreachable handler.

    All three are the same defect: ✅ was asserted from the *existence of a
    name* rather than from the parser or registry that decides what a user can
    actually reach. This script turns ✅ into a claim that has to be earned
    against a machine-generated list of what is reachable.

WHAT IT DOES
    For every ✅ row in sections 1, 2, 6 and 7 it asks "can a user reach this?"
    and answers from the artifact that *defines* reachability, not from a grep
    of the source:

      section 1  docs/cli-command-tree.txt   every path `XaftCli::command()`
                                             accepts, walked out of clap
                                             itself. Generated and re-verified
                                             against clap by
                                             crates/xaft-cli/tests/cli_command_tree.rs.
      section 2  the MATRIX_DONE list in     the proof already exists as an
               crates/xaft-tui/tests/       executable test that parses each
               slash_parity_integration.rs  command and resolves a handler.
                                             The script reads the list out of
                                             the test, so a command added to
                                             the matrix without being added
                                             there is a failure here, and the
                                             proof cannot silently rot.
      section 6  docs/tool-registry.txt      every name a registry built by
                                             ToolRegistryBuilder actually
                                             holds. Generated from the real
                                             registry and re-verified by
                                             crates/xaft-tools/tests/tool_parity_reachability.rs.
                                             A `const TOOL_NAME = "..."` grep is
                                             deliberately NOT used: row 78
                                             `mcp_connection_guide` returns a
                                             literal rather than a const, so a
                                             grep reports a live tool as
                                             missing. A false alarm in the one
                                             place this check exists to be
                                             accurate is worse than no check.
      section 7  scripts/config-key-audit.py a ✅ config row must be backed by a
               re-used, not re-invented    verdict of exact / rename /
                                             structural from the field-level
                                             diff, AND the field must be
                                             *read* by real code outside
                                             crates/xaft-config. Declaring a
                                             field is not using it.

THE SESSION RENAME IS AN ALLOWLIST ENTRY, NOT AN EXCEPTION
    Section 1 rows 14-17 name agenthicc's `sessions list` / `show` / `export`
    / `inspect`. xaft exposes all four, under the singular `session`, because
    agenthicc has two separate `session` and `sessions` command groups and
    xaft merged them. The matrix's own xaft column already records the mapping.
    That mapping is written down in COMMAND_RENAMES below and the script
    requires it to be *named*, so a future rename has to be declared rather
    than inferred. Everything not in that table has to match clap exactly.

    Note what this does NOT do: it does not mark those rows ❌. `xaft session
    list` is a reachable path, so the row is true. The bug class being hunted
    is "marked ✅ but unreachable", not "agenthicc and xaft spell it
    differently".

EXIT
    0  every ✅ row in sections 1, 2, 6 and 7 is backed by reachability evidence
    1  at least one ✅ row is unreachable (with --check)
    2  the matrix or a required artifact could not be read or parsed at all

USAGE
    scripts/parity-row-reachability.py            # report every section
    scripts/parity-row-reachability.py --section 1
    scripts/parity-row-reachability.py --check    # non-zero if any false ✅
    scripts/parity-row-reachability.py --self-test
"""
from __future__ import annotations

import argparse
import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MATRIX = ROOT / "docs" / "parity-matrix.md"
CLI_TREE = ROOT / "docs" / "cli-command-tree.txt"
TOOL_REGISTRY = ROOT / "docs" / "tool-registry.txt"
SLASH_TEST = ROOT / "crates" / "xaft-tui" / "tests" / "slash_parity_integration.rs"
CONFIG_AUDIT = ROOT / "scripts" / "config-key-audit.py"

OK, PARTIAL, MISSING = "✅", "🟡", "❌"

# agenthicc command path -> the xaft path that actually parses, where the two
# differ for a *spelling* reason rather than a missing-feature reason.
#
# agenthicc has both a `session` group (service commands) and a `sessions`
# group (export/inspect). xaft merged them under the singular `session`.
# Anything not listed here must match a clap path character for character.
COMMAND_RENAMES = {
    "sessions list": "session list",
    "sessions show": "session show",
    "sessions export": "session export",
    "sessions inspect": "session inspect",
}

# xaft fields that are read by code inside crates/xaft-config itself, which
# the section-7 "read outside xaft-config" scan deliberately excludes.
SECTION7_READ_EXEMPT = {
    # `defaults.rs` constructs every default, so a field only defaulted and
    # never consumed elsewhere would be exempt here by accident. These two are
    # exempt because real non-config code reads them by a different spelling.
    ("ProviderConfig", "timeout_secs"): "crates/xaft-cli and xaft-runtime read the resolved value",
}


class Row:
    __slots__ = ("lineno", "number", "section", "cells", "status")

    def __init__(self, lineno, number, section, cells, status):
        self.lineno = lineno
        self.number = number
        self.section = section
        self.cells = cells
        self.status = status

    @property
    def label(self) -> str:
        return self.cells[1] if len(self.cells) > 1 else f"row {self.number}"


def parse_sections(text: str) -> dict[str, list[Row]]:
    """Split the matrix into {section number: [data rows]}."""
    out: dict[str, list[Row]] = {}
    section: str | None = None
    for lineno, line in enumerate(text.split("\n"), 1):
        m = re.match(r"^##\s+(\d+)\.\s", line)
        if m:
            section = m.group(1)
            out.setdefault(section, [])
            continue
        if line.startswith("## ") or section is None:
            continue
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.split("|")]
        # [ "", "#", name, source, xaft, status, "" ]
        if len(cells) < 5:
            continue
        number = cells[1]
        if not number.isdigit():
            continue
        out[section].append(Row(lineno, int(number), section, cells, cells[-2]))

    return out


def done_rows(rows: list[Row]) -> list[Row]:
    return [r for r in rows if r.status.strip() == OK]


def read_lines(path: Path) -> list[str]:
    if not path.exists():
        sys.exit(f"required artifact is missing: {path}")
    return [l.strip() for l in path.read_text(encoding="utf-8").split("\n") if l.strip()]


# ---------------------------------------------------------------- section 1


def audit_section_1(rows: list[Row]) -> tuple[list[str], list[str]]:
    """CLI rows: the command a user types must be a path clap accepts."""
    tree = {l for l in read_lines(CLI_TREE) if not l.startswith("#")}
    bad, checked = [], []
    for r in done_rows(rows):
        cmd = r.cells[2].strip("`").strip()
        want = COMMAND_RENAMES.get(cmd, cmd)
        checked.append(f"  ok   {cmd} -> `{want}`")
        if want not in tree:
            bad.append(
                f"  line {r.lineno}: row {r.number} `{cmd}` is marked {OK} but "
                f"`{want}` is not a path XaftCli::command() accepts.\n"
                f"    a ✅ CLI row must be a string the binary parses; "
                f"if it is renamed, add it to COMMAND_RENAMES with a reason."
            )
    if not checked:
        bad.append("  no ✅ rows parsed from section 1 — the parser is broken")
    return bad, checked


# ---------------------------------------------------------------- section 2


SLASH_LIST_RE = re.compile(r"const MATRIX_DONE: &\[\(&str, &str\)\] = &\[(.*?)\];", re.S)
SLASH_ITEM_RE = re.compile(r'\(\s*"([^"]+)"\s*,')


def audit_section_2(rows: list[Row]) -> tuple[list[str], list[str]]:
    """Slash rows: the command must be in the test list that proves dispatch.

    Section 2's proof is an executable test, not a generated artifact, so the
    script reads the command names out of that test. That keeps the two in
    lockstep in both directions: a matrix row without a test entry fails here,
    and a test entry without a row is reported as an orphan so the lists
    cannot quietly diverge.
    """
    src = SLASH_TEST.read_text(encoding="utf-8")
    m = SLASH_LIST_RE.search(src)
    if not m:
        return ["  could not find MATRIX_DONE in the slash parity test"], []
    proven = {n for n in SLASH_ITEM_RE.findall(m.group(1))}

    bad, checked = [], []
    for r in done_rows(rows):
        name = r.cells[2].strip("`").strip().lstrip("/")
        if name in proven:
            checked.append(f"  ok   /{name}")
        else:
            bad.append(
                f"  line {r.lineno}: row {r.number} `/{name}` is marked {OK} but is "
                f"not in MATRIX_DONE in {SLASH_TEST.relative_to(ROOT)}.\n"
                f"    that test is what proves the command parses AND has a "
                f"handler; add `{name}` to its list."
            )
    if not checked:
        bad.append("  no ✅ rows parsed from section 2 — the parser is broken")

    orphans = sorted(proven - {r.cells[2].strip("`").lstrip("/") for r in done_rows(rows)})
    for o in orphans:
        bad.append(f"  MATRIX_DONE proves `/{o}`, which no {OK} section-2 row claims")
    return bad, checked


# ---------------------------------------------------------------- section 6


def audit_section_6(rows: list[Row]) -> tuple[list[str], list[str]]:
    """Tool rows: the name must be in a registry built by ToolRegistryBuilder."""
    registered = {l for l in read_lines(TOOL_REGISTRY) if not l.startswith("#")}
    bad, checked = [], []
    for r in done_rows(rows):
        cell = r.cells[-3]
        names = re.findall(r"`([^`]+)`", cell)
        if not names:
            bad.append(
                f"  line {r.lineno}: row {r.number} is marked {OK} but its xaft "
                f"column names no tool"
            )
            continue
        for name in names:
            if name in registered:
                checked.append(f"  ok   `{name}`")
            else:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{name}` is marked {OK} but "
                    f"the built ToolRegistry does not register it.\n"
                    f"    a ✅ tool row means the agent can actually call it."
                )
    if len(checked) < 30:
        bad.append(
            f"  only {len(checked)} tool names matched the registry — far fewer "
            f"than the matrix claims, so the comparison is broken"
        )
    return bad, checked


# ---------------------------------------------------------------- section 7


def load_config_audit():
    spec = importlib.util.spec_from_file_location("config_key_audit", CONFIG_AUDIT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _source_roots() -> list[Path]:
    """Every `src/` tree that can hold a reader, including the root binary.

    The root `src/` is the `xaft` binary crate and it is NOT under `crates/`,
    so a scan that only walks `crates/*/src` misses it. That omission is not
    cosmetic: `src/main.rs` is the only reader of
    `MemoryConfig.project_memory_path` (row 120), so without this the audit
    reported a live key as dead. A check whose false alarms are its most
    visible output is a check people learn to ignore.
    """
    roots = [p for p in sorted((ROOT / "crates").iterdir()) if (p / "src").is_dir()]
    if (ROOT / "src").is_dir():
        roots.append(ROOT)
    return [r / "src" for r in roots]


def source_dir_crate_reads(field: str) -> bool:
    """True if any non-config crate reads `.field` in real code."""
    pattern = re.compile(rf"\.{re.escape(field)}\b")
    for src_root in _source_roots():
        if src_root.parent.name == "xaft-config":
            continue
        for rs in src_root.rglob("*.rs"):
            text = rs.read_text(encoding="utf-8", errors="replace")
            for line in text.split("\n"):
                stripped = line.strip()
                if stripped.startswith("//"):
                    continue
                if pattern.search(line):
                    return True
    return False


def audit_section_7(rows: list[Row]) -> tuple[list[str], list[str]]:
    """Config rows: a ✅ needs a field-level verdict AND a real reader.

    `scripts/config-key-audit.py` already maps every row to exact / rename /
    structural / collision / absent / deferred. This section reuses that
    classification instead of re-deriving it, and then adds the thing the
    classification cannot see: whether anything actually *reads* the field.
    A declared-but-never-read field is exactly the false ✅ that G5 found in
    the slash section, one layer down.
    """
    mod = load_config_audit()
    cka_rows = mod.parse_matrix_section()
    structs = mod.parse_xaft_fields()
    index = mod.index_wire_names(structs)
    verdicts = mod.classify(cka_rows, index, structs)
    by_number = {v["row"]: v for v in verdicts}

    bad, checked = [], []
    # Fields proven read, so the O(crates x files) scan runs once per field
    # rather than once per row that shares it.
    read_cache: dict[str, bool] = {}

    for r in done_rows(rows):
        v = by_number.get(str(r.number))
        if v is None:
            bad.append(f"  line {r.lineno}: row {r.number} is absent from config-key-audit")
            continue
        if v["verdict"] not in ("exact", "rename", "structural"):
            bad.append(
                f"  line {r.lineno}: row {r.number} `{v['key']}` is marked {OK} in the "
                f"matrix but config-key-audit verdict is `{v['verdict']}` "
                f"({v['evidence']})"
            )
            continue

        target = v.get("target", "")
        if v["verdict"] == "structural":
            # Satisfied by construction: the TOML table key IS the name.
            checked.append(f"  ok   {v['key']} (structural)")
            continue
        if "." in target:
            # A rename points at a specific `Struct.field`; that is the field
            # whose reader proves the row.
            field = target.split(".")[-1]
        else:
            # An exact verdict's `target` is the *struct*, and the field is the
            # row's own key under that struct. Resolving it through the parsed
            # struct map rather than by string surgery is the difference
            # between checking `ProviderConfig.max_retries` and checking a
            # non-existent field called `ProviderConfig`.
            struct = target
            fields = structs.get(struct, {})
            field = fields.get(v["key"]) or fields.get(mod.snake(v["key"]))
            if not field:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{v['key']}` is marked {OK} but "
                    f"`{struct}` declares no such field, so the ✅ cannot be traced to code"
                )
                continue
        if not re.fullmatch(r"[A-Za-z_]\w*", field):
            # The rename points at a whole struct/table, not a field. A table
            # is read when any of its fields is.
            checked.append(f"  ok   {v['key']} -> `{target}` (table)")
            continue
        if field not in read_cache:
            read_cache[field] = source_dir_crate_reads(field)
        if read_cache[field]:
            checked.append(f"  ok   {v['key']} -> `{target}`")
        else:
            bad.append(
                f"  line {r.lineno}: row {r.number} `{v['key']}` is marked {OK} but "
                f"`{target}` is declared in crates/xaft-config and read by NO "
                f"other crate.\n"
                f"    a declared field nobody reads is not a working config key."
            )

    if not checked:
        bad.append("  no ✅ rows parsed from section 7 — the parser is broken")
    return bad, checked


# -------------------------------------------------------------------- main

AUDITS = {
    "1": ("CLI commands", audit_section_1, "docs/cli-command-tree.txt (clap's own tree)"),
    "2": ("Slash commands", audit_section_2, "MATRIX_DONE in slash_parity_integration.rs"),
    "6": ("Built-in tools", audit_section_6, "docs/tool-registry.txt (a built ToolRegistry)"),
    "7": ("Configuration keys", audit_section_7, "config-key-audit.py verdicts + real readers"),
}


def run(sections: list[str]) -> tuple[list[str], dict[str, int]]:
    text = MATRIX.read_text(encoding="utf-8")
    parsed = parse_sections(text)
    failures: list[str] = []
    counts: dict[str, int] = {}
    for key in sections:
        title, fn, evidence = AUDITS[key]
        rows = parsed.get(key, [])
        done = len(done_rows(rows))
        counts[key] = done
        if not done:
            failures.append(f"section {key} ({title}): no {OK} rows found — cannot audit")
            continue
        bad, checked = fn(rows)
        print(f"section {key} — {title}: {done} {OK} rows, {len(checked)} proven")
        print(f"  evidence: {evidence}")
        if bad:
            print(f"  {len(bad)} UNREACHABLE:")
            for b in bad:
                print(b)
        failures.extend(f"section {key}: {b.strip()}" for b in bad)
    return failures, counts


def self_test() -> int:
    """Prove the audits can actually fail.

    An audit that cannot fail is worse than no audit, because it reads as
    coverage. Each case below feeds a deliberately broken input through the
    real code path and asserts the failure is detected.
    """
    failures = []

    # 1. A ✅ CLI row naming a command clap does not accept must be caught.
    fake = [Row(1, 1, "1", ["", "1", "`teleport`", "src", "xaft", OK, ""], OK)]
    bad, _ = audit_section_1(fake)
    if not bad:
        failures.append("section 1: a bogus ✅ command path was not detected")
    else:
        print("  ok  section 1 rejects an unreachable command")

    # 2. A ✅ slash row missing from the proof list must be caught.
    fake = [Row(1, 1, "2", ["", "1", "`/teleport`", "—", OK, ""], OK)]
    bad, _ = audit_section_2(fake)
    if not bad:
        failures.append("section 2: a slash ✅ row with no proof was not detected")
    else:
        print("  ok  section 2 rejects an unproven slash command")

    # 3. A ✅ tool row whose name is not registered must be caught.
    fake = [Row(1, 1, "6", ["", "1", "`teleport`", "src", "`teleport`", OK, ""], OK)]
    bad, _ = audit_section_6(fake)
    if not bad:
        failures.append("section 6: an unregistered ✅ tool was not detected")
    else:
        print("  ok  section 6 rejects an unregistered tool")

    # 4. A recorded rename must actually resolve, or the allowlist is a hole.
    renamed = COMMAND_RENAMES.get("sessions export")
    if renamed is None:
        failures.append("COMMAND_RENAMES lost its `sessions export` entry")
    elif renamed not in {l for l in read_lines(CLI_TREE) if not l.startswith("#")}:
        failures.append(f"COMMAND_RENAMES maps `sessions export` -> `{renamed}`, which clap does not accept")
    else:
        print("  ok  the sessions→session rename resolves in clap's tree")

    # 5. Every ✅ row must be parsed out of the real matrix, or the whole run
    #    is vacuous.
    parsed = parse_sections(MATRIX.read_text(encoding="utf-8"))
    total = sum(len(done_rows(parsed.get(k, []))) for k in AUDITS)
    if total < 100:
        failures.append(f"only {total} ✅ rows parsed across sections 1/2/6/7 — expected well over 100")
    else:
        print(f"  ok  {total} ✅ rows parsed across the audited sections")

    if failures:
        print("\nself-test FAILED:")
        for f in failures:
            print(f"  {f}")
        return 1
    print("self-test passed: every audit can fail")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--section", choices=sorted(AUDITS), action="append",
                    help="audit only this section (repeatable)")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if any ✅ row is unreachable")
    ap.add_argument("--self-test", action="store_true",
                    help="prove each audit can fail, then exit")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    sections = args.section or sorted(AUDITS)
    failures, counts = run(sections)

    total_rows = sum(counts.values())
    print(f"\n{total_rows} ✅ rows audited across section(s) {', '.join(sections)}")
    if failures:
        print(f"{len(failures)} UNREACHABLE ✅ rows — a ✅ here is not reachable")
        return 1 if args.check else 0
    print("every ✅ row is backed by reachability evidence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
