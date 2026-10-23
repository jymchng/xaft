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

WHY 🟡 ROWS ARE NOT AUDITED — AND WHY THAT IS REPORTED RATHER THAN HIDDEN
    Only ✅ rows are checked, and the reason is that the two claims need
    different kinds of evidence.

      ✅ means "a user can do this". That is mechanically checkable against the
         parser or registry that decides what is reachable, which is what this
         script does.

      🟡 means "partially implemented". Commit `213e635` found ten rows in this
         exact state and they were ALL dishonest: rows 47-50 and 52-58 pointed
         at `list_files`, `read_file`, `create_directory` and friends, which
         are registered and reachable, so a reachability check would have
         passed every one of them. Sharing a VERB is not sharing a capability
         — `create_directory` does not partially implement calendar-event
         creation. Whether a 🟡 "genuinely implements part of" the agenthicc
         tool is a semantic judgement, and the precedent for it in this repo is
         a hand audit (docs/config-key-audit.py for section 7), not a grep.

    So auditing 🟡 rows here would produce green either way and prove nothing.

    MEASURED at commit de5acc2: section 6 has 39 ✅ / 0 🟡 / 39 ❌ and section 7
    has 37 ✅ / 0 🟡, i.e. there is nothing left to audit. Goals `3bb3d459` and
    `471ea5cf` downgraded all ten false-partials to ❌, which is a legitimate
    terminal state — a declined feature stays ❌ with its reason rather than
    sitting at 🟡 forever asserting a partial that does not exist.

    THE HAZARD, and why the counts below are printed rather than omitted: if a
    future pass flips a tool row from ✅ to 🟡 to express "partly done", this
    script would STOP checking it and still exit 0, reporting green over a row
    it no longer examines. A checker that goes quiet about what it stopped
    covering is the same false-comfort failure as the `/usage` row it was
    built to catch. Every run therefore reports how many 🟡 and ❌ rows were
    outside its remit. A non-zero count is NOT a failure — 🟡 is a legal
    status — but it is never allowed to pass unmentioned.

➖ ROWS (REMOVED UPSTREAM) ARE PARSED, NEVER AUDITED, AND REPORTED SEPARATELY
    A ➖ row names surface agenthicc no longer has (the 2026-10-23 re-baseline
    marked 37 config rows ➖ after PRD-250 deleted them). There is nothing to
    reach: the row is history, kept so the matrix does not silently forget
    what it used to claim. Such a row is parsed — a parser that skipped the
    status would make the row vanish from this report, the exact quiet loss
    described above — and printed in its own count, never folded into ❌.
    Rows 44 and 46 were among the 51 unreachable ✅ rows before the
    re-baseline; agenthicc deleted both keys, so they left the audit as ➖
    rather than by being "fixed".

FLAG ROWS: THE CLAP DERIVE, BECAUSE THE TREE HAS NO FLAGS
    The re-baseline added agenthicc's global flags (`--headless`,
    `--continue`, ...) to section 1. docs/cli-command-tree.txt lists command
    paths only, so a ✅ flag row is checked against the clap derive in
    crates/xaft-cli/src/args.rs instead: FLAG_HOMES (below) declares which
    xaft command carries the flag — agenthicc's flags are global, xaft's live
    on `xaft run` — the home path must be in clap's tree, and the `Args`
    struct the `Commands` variant binds must declare the long flag (an
    explicit `long = "..."`, or a bare `long` on the kebab-cased field;
    `#[command(flatten)]` structs are followed; `--version` comes from the
    `version` attribute on `XaftCli`). That is a structural parse of the very
    declarations clap reads, which is weaker evidence than a tree walked out of
    `XaftCli::command()` and stronger than a name grep. The upgrade path is to
    have crates/xaft-cli/tests/cli_command_tree.rs snapshot each command's
    long flags beside the paths, after which this branch should read that
    file instead.

SECTION 7 LOADER KEYS
    Three section-7 rows (`config.py::load_config`) are not struct fields on
    either side: `extends`, `AGENTHICC_CONFIG` and the env-override family are
    behaviour of the config LOADER. config-key-audit.py gives them the
    verdict `loader`, and the evidence here is that
    crates/xaft-config/src/loader.rs reads the literal in non-comment code —
    the loader is the reader, so the usual "read outside xaft-config" rule
    does not apply to them.

EXIT
    0  every ✅ row in sections 1, 2, 6 and 7 is backed by reachability evidence,
       and the count of unchecked 🟡/❌ rows is reported
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
CLI_ARGS = ROOT / "crates" / "xaft-cli" / "src" / "args.rs"
CONFIG_LOADER = ROOT / "crates" / "xaft-config" / "src" / "loader.rs"

OK, PARTIAL, MISSING, REMOVED = "✅", "🟡", "❌", "➖"
STATUS_MARKERS = (OK, PARTIAL, MISSING, REMOVED)

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
    # A `"trust cli": "trust add"` entry lived here while row 47 kept the name
    # it was extracted under; agenthicc (PRD-250 S-02) replaced `trust cli`
    # with `trust list | add | revoke`. The 2026-10-23 re-baseline renamed
    # row 47 in place to `trust add` and added `trust list` / `trust revoke`
    # as rows 82/83, so every trust row now matches clap directly.
}

# agenthicc global flag -> (the xaft command path that carries it, the xaft
# long flag). agenthicc's flags are parser-global (`agenthicc --continue`);
# xaft's live on a command (`xaft run --continue`), so the home is declared
# here, row by row, exactly like a COMMAND_RENAMES entry. An empty path is the
# top level (`xaft --version`). Only ✅ flag rows are audited; a ✅ flag row
# with no entry here is a failure, not a skip.
FLAG_HOMES: dict[str, tuple[str, str]] = {
    "--headless": ("run", "--headless"),  # row 95
    "--version": ("", "--version"),  # row 99
    "--continue": ("run", "--continue"),  # row 100
    "--resume": ("run", "--resume"),  # row 101
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
        # A leading number is NOT sufficient to make this a data row. The
        # ownership tables (\u00a71b, \u00a71c, \u00a72b, \u00a76b) also start with a row
        # number, and their "status" column holds prose ("closed", "G4b-6",
        # "**unowned** \u2014 ...", a whole declined-with-reason paragraph). Counting
        # those inflated the "not audited" totals by 18 rows against a real
        # total of 372 \u2014 26 \u274c claimed in section 1 where the matrix has 19.
        # Require the status cell to BE a status marker, which is what makes a
        # row a matrix row at all. parity-matrix-stats.py --check independently
        # derives 372 data rows, so the two parsers agree after this guard.
        # ➖ is a status marker too: a removed-upstream row is parsed so it
        # can be reported, rather than vanishing from the run.
        if cells[-2] not in STATUS_MARKERS:
            continue
        out[section].append(Row(lineno, int(number), section, cells, cells[-2]))

    return out


def done_rows(rows: list[Row]) -> list[Row]:
    return [r for r in rows if r.status.strip() == OK]


def unchecked_rows(rows: list[Row]) -> list[Row]:
    """Rows this script deliberately does NOT check, because they are 🟡 or ❌.

    See "WHY 🟡 ROWS ARE NOT AUDITED" in the module docstring. This exists so a
    row that stops being ✅ cannot silently vanish from the run's coverage
    without the run saying so. ➖ rows are not here: they are not a parity
    claim of any kind, and `removed_rows` reports them on their own line.
    """
    return [r for r in rows if r.status.strip() in (PARTIAL, MISSING)]


def removed_rows(rows: list[Row]) -> list[Row]:
    """➖ rows: surface agenthicc no longer has. Reported, never audited."""
    return [r for r in rows if r.status.strip() == REMOVED]


def read_lines(path: Path) -> list[str]:
    if not path.exists():
        sys.exit(f"required artifact is missing: {path}")
    return [l.strip() for l in path.read_text(encoding="utf-8").split("\n") if l.strip()]


# ---------------------------------------------------------------- section 1


def _matching(text: str, start: int, open_ch: str, close_ch: str) -> int:
    """Index just past the bracket matching `text[start]`, skipping strings."""
    depth, i, in_str = 0, start, False
    while i < len(text):
        ch = text[i]
        if in_str:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
        elif ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise ValueError(f"unbalanced {open_ch}{close_ch} at offset {start}")


def _rust_item(src: str, kind: str, name: str) -> tuple[str, str]:
    """(attributes before, body) of `pub struct|enum <name> { ... }`."""
    m = re.search(rf"\bpub\s+{kind}\s+{re.escape(name)}\b[^{{;]*\{{", src)
    if not m:
        raise LookupError(f"{CLI_ARGS.name}: no `pub {kind} {name}`")
    open_idx = m.end() - 1
    body = src[open_idx + 1 : _matching(src, open_idx, "{", "}") - 1]
    # The attributes that apply to the item are the `#[...]` blocks directly
    # above it, after the previous item ended.
    head = src[: m.start()]
    prev_end = max(head.rfind("}\n"), head.rfind(";\n"))
    return head[prev_end + 1 :], body


def _attr_blocks(text: str, name: str) -> list[str]:
    """The bodies of every `#[name(...)]` attribute in `text`."""
    out = []
    for m in re.finditer(rf"#\[{name}\(", text):
        open_idx = m.end() - 1
        out.append(text[open_idx + 1 : _matching(text, open_idx, "(", ")") - 1])
    return out


def _kebab(field: str) -> str:
    return field.removeprefix("r#").replace("_", "-")


def clap_longs(src: str, struct: str, _seen: frozenset[str] = frozenset()) -> set[str]:
    """Every `--long` (and long alias) the clap `Args` struct declares."""
    if struct in _seen:
        return set()
    attrs, body = _rust_item(src, "struct", struct)
    longs: set[str] = set()
    for block in _attr_blocks(attrs, "command"):
        # `#[command(version)]` (or `version = "..."`) is how clap grows
        # `--version`. String literals are blanked first, so an `about`
        # text that mentions the word cannot count.
        code = re.sub(r'"(?:[^"\\]|\\.)*"', '""', block)
        if re.search(r"(?<!\w)version\b", code):
            longs.add("--version")
    prev = 0
    for m in re.finditer(r"^\s*pub\s+(r#)?(\w+)\s*:\s*([^\n]+)", body, re.M):
        field_attrs, prev = body[prev : m.start()], m.end()
        field = (m.group(1) or "") + m.group(2)
        if any(re.fullmatch(r"\s*flatten\s*", a) for a in _attr_blocks(field_attrs, "command")):
            inner = re.match(r"\s*(?:Option<)?(\w+)", m.group(3))
            if inner:
                longs |= clap_longs(src, inner.group(1), _seen | {struct})
            continue
        for arg in _attr_blocks(field_attrs, "arg"):
            explicit = re.search(r'(?<![\w"])long\s*=\s*"([^"]+)"', arg)
            if explicit:
                longs.add(f"--{explicit.group(1)}")
            elif re.search(r'(?<![\w"])long\b(?!\s*=)', arg):
                longs.add(f"--{_kebab(field)}")
            else:
                continue
            for alias in re.findall(r'(?<![\w"])(?:visible_)?alias\s*=\s*"([^"]+)"', arg):
                longs.add(f"--{alias}")
            for group in re.findall(r'(?<![\w"])(?:visible_)?aliases\s*=\s*\[([^\]]*)\]', arg):
                longs.update(f"--{a}" for a in re.findall(r'"([^"]+)"', group))
    return longs


def command_structs(src: str) -> dict[str, str]:
    """Top-level command path -> the `Args` struct its `Commands` variant binds."""
    _, body = _rust_item(src, "enum", "Commands")
    out: dict[str, str] = {}
    prev = 0
    for m in re.finditer(r"^\s*(\w+)\((\w+)\)\s*,", body, re.M):
        attrs, prev = body[prev : m.start()], m.end()
        named = None
        for block in _attr_blocks(attrs, "command"):
            n = re.search(r'(?<![\w"])name\s*=\s*"([^"]+)"', block)
            if n:
                named = n.group(1)
        path = named or re.sub(r"(?<!^)(?=[A-Z])", "-", m.group(1)).lower()
        out[path] = m.group(2)
    return out


def flag_is_declared(home: str, flag: str) -> tuple[bool, str]:
    """Whether the clap struct for `home` declares `flag`, with a reason."""
    src = CLI_ARGS.read_text(encoding="utf-8")
    if home:
        struct = command_structs(src).get(home)
        if struct is None:
            return False, f"`{home}` is not a top-level `Commands` variant in {CLI_ARGS.name}"
    else:
        struct = "XaftCli"
    try:
        longs = clap_longs(src, struct)
    except (LookupError, ValueError) as exc:
        return False, str(exc)
    if flag in longs:
        return True, f"`{struct}` declares `{flag}`"
    return False, f"`{struct}` declares {sorted(longs)}, not `{flag}`"


def audit_section_1(rows: list[Row]) -> tuple[list[str], list[str]]:
    """CLI rows: the command a user types must be a path clap accepts.

    Flag rows (`--headless`, ...) have no path; they are checked against the
    clap derive through FLAG_HOMES — see "FLAG ROWS" in the module docstring.
    """
    tree = {l for l in read_lines(CLI_TREE) if not l.startswith("#")}
    bad, checked = [], []
    for r in done_rows(rows):
        cmd = r.cells[2].strip("`").strip()
        if cmd.startswith("--"):
            home = FLAG_HOMES.get(cmd)
            if home is None:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{cmd}` is marked {OK} but has no "
                    f"FLAG_HOMES entry naming the xaft command that carries it."
                )
                continue
            path, flag = home
            if path and path not in tree:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{cmd}` lives on `{path}`, which is "
                    f"not a path XaftCli::command() accepts."
                )
                continue
            ok, why = flag_is_declared(path, flag)
            spelled = f"xaft {path} {flag}" if path else f"xaft {flag}"
            if ok:
                checked.append(f"  ok   {cmd} -> `{spelled}` ({why})")
            else:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{cmd}` is marked {OK} but "
                    f"`{spelled}` is not declared: {why}"
                )
            continue
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


def loader_reads(literal: str) -> bool:
    """True if crates/xaft-config/src/loader.rs reads `"literal"` in real code."""
    needle = f'"{literal}"'
    for line in CONFIG_LOADER.read_text(encoding="utf-8").split("\n"):
        code = line.split("//", 1)[0]
        if needle in code:
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
        if v["verdict"] not in ("exact", "rename", "structural", "loader"):
            bad.append(
                f"  line {r.lineno}: row {r.number} `{v['key']}` is marked {OK} in the "
                f"matrix but config-key-audit verdict is `{v['verdict']}` "
                f"({v['evidence']})"
            )
            continue

        target = v.get("target", "")
        if v["verdict"] == "loader":
            # A loader key's reader is the loader itself; see "SECTION 7
            # LOADER KEYS" in the module docstring.
            literal = v.get("literal", "")
            if literal and loader_reads(literal):
                checked.append(f"  ok   {v['key']} -> loader reads \"{literal}\"")
            else:
                bad.append(
                    f"  line {r.lineno}: row {r.number} `{v['key']}` is marked {OK} but "
                    f"{CONFIG_LOADER.relative_to(ROOT)} never reads \"{literal}\" in code."
                )
            continue
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


def _ranges(numbers: list[int]) -> str:
    """`[44, 46, 120, 121, 122]` -> `44, 46, 120-122`."""
    out, run_start, prev = [], None, None
    for n in sorted(numbers) + [None]:
        if run_start is not None and (n is None or n != prev + 1):
            out.append(f"{run_start}" if run_start == prev else f"{run_start}-{prev}")
            run_start = None
        if n is not None and run_start is None:
            run_start = n
        prev = n
    return ", ".join(out)


def run(
    sections: list[str],
) -> tuple[list[str], dict[str, int], dict[str, list[Row]], dict[str, list[Row]]]:
    text = MATRIX.read_text(encoding="utf-8")
    parsed = parse_sections(text)
    failures: list[str] = []
    counts: dict[str, int] = {}
    unchecked: dict[str, list[Row]] = {}
    removed: dict[str, list[Row]] = {}
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
        # Report what was NOT looked at. Not a failure — 🟡/❌ are legal
        # statuses — but never let it pass unmentioned, because a row that
        # leaves the ✅ set would otherwise stop being checked silently.
        skipped = unchecked_rows(rows)
        unchecked[key] = skipped
        partial = [r for r in skipped if r.status.strip() == PARTIAL]
        print(
            f"  not audited: {len(skipped)} non-{OK} row(s)"
            f" ({len(partial)} 🟡, {len(skipped) - len(partial)} ❌)"
            f" — reachability is a ✅-only claim, see the module docstring"
        )
        for r in partial:
            print(f"    🟡 row {r.number} (line {r.lineno}) is NOT reachability-checked")
        gone = removed_rows(rows)
        removed[key] = gone
        if gone:
            print(
                f"  removed upstream: {len(gone)} ➖ row(s), excluded — rows "
                f"{_ranges([r.number for r in gone])}"
            )
        if bad:
            print(f"  {len(bad)} UNREACHABLE:")
            for b in bad:
                print(b)
        failures.extend(f"section {key}: {b.strip()}" for b in bad)
    return failures, counts, unchecked, removed


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

    # 4. `unchecked_rows` must actually separate 🟡/❌ from ✅, or the "not
    #    audited" line this run prints is a lie. Regression guard for the
    #    silent-loss hazard: a row leaving the ✅ set must become visible.
    mixed = [
        Row(1, 1, "6", ["", "1", "`a`", "src", "`a`", OK, ""], OK),
        Row(2, 2, "6", ["", "2", "`b`", "src", "`b`", PARTIAL, ""], PARTIAL),
        Row(3, 3, "6", ["", "3", "`c`", "src", "`c`", MISSING, ""], MISSING),
    ]
    skipped = unchecked_rows(mixed)
    if len(done_rows(mixed)) != 1:
        failures.append("done_rows no longer selects exactly the ✅ rows")
    elif len(skipped) != 2 or {r.status.strip() for r in skipped} != {PARTIAL, MISSING}:
        failures.append("unchecked_rows does not report the 🟡/❌ rows")
    elif not any(r.status.strip() == PARTIAL for r in skipped):
        failures.append("a 🟡 row is not reported as unchecked")
    else:
        print("  ok  🟡/❌ rows are reported as NOT audited")

    # 4b. An OWNERSHIP-TABLE row must not be counted as a matrix row, or the
    #     "not audited" totals above are inflated and the report lies. The §1b,
    #     §1c, §2b and §6b tables all start with a row number and their status
    #     column holds prose. Regression guard for the 18 phantom rows that
    #     made section 1 claim 26 ❌ against the matrix's actual 19.
    ownership = parse_sections(
        "## 1. CLI commands\n"
        "| # | feature | source | xaft | status |\n"
        "|---|---|---|---|---|\n"
        "| 41 | `run_tests` | A **pytest/unittest runner** | declined | G4b-x |\n"
        "| 20 | `jobs cancel` | stores it | reachable | ✅ |\n"
    ).get("1", [])
    if len(ownership) != 1:
        failures.append(
            "an ownership-table row was parsed as a matrix row "
            f"(got {len(ownership)} rows, expected 1)"
        )
    elif ownership[0].number != 20:
        failures.append("parse_sections kept the wrong row from the sample")
    else:
        print("  ok  ownership-table rows are not counted as matrix rows")

    # 5. A recorded rename must actually resolve, or the allowlist is a hole.
    renamed = COMMAND_RENAMES.get("sessions export")
    if renamed is None:
        failures.append("COMMAND_RENAMES lost its `sessions export` entry")
    elif renamed not in {l for l in read_lines(CLI_TREE) if not l.startswith("#")}:
        failures.append(f"COMMAND_RENAMES maps `sessions export` -> `{renamed}`, which clap does not accept")
    else:
        print("  ok  the sessions→session rename resolves in clap's tree")

    # 5b. A ➖ row is parsed — so it is visible — but it is neither audited
    #     nor reported as an unchecked 🟡/❌. Counting it as ❌ would inflate
    #     the open total with surface agenthicc deleted; dropping it in the
    #     parser would make it vanish from the report.
    gone = parse_sections(
        "## 7. Configuration keys\n"
        "| 1 | `a` | `config.py::X` | x | ✅ |\n"
        "| 2 | `b` | `config.py::X` | removed upstream | ➖ |\n"
        "| 3 | `c` | `config.py::X` | — | ❌ |\n"
    ).get("7", [])
    if len(gone) != 3:
        failures.append(f"a ➖ row is not parsed (got {len(gone)} rows, expected 3)")
    elif [r.number for r in removed_rows(gone)] != [2]:
        failures.append("removed_rows does not select exactly the ➖ row")
    elif [r.number for r in unchecked_rows(gone)] != [3] or len(done_rows(gone)) != 1:
        failures.append("a ➖ row leaks into the ✅ or the unchecked 🟡/❌ set")
    else:
        print("  ok  ➖ rows are parsed and reported apart from ✅, 🟡 and ❌")

    # 5c. Flag rows. A ✅ flag row with no declared home, and one whose home's
    #     clap struct does not declare it, must both fail; a real flag must
    #     resolve, or the clap-derive parser has silently broken.
    flag_row = Row(1, 1, "1", ["", "1", "`--teleport`", "src", "xaft", OK, ""], OK)
    bad, _ = audit_section_1([flag_row])
    saved = dict(FLAG_HOMES)
    try:
        FLAG_HOMES["--teleport"] = ("run", "--teleport")
        bad_declared, _ = audit_section_1([flag_row])
        FLAG_HOMES["--teleport"] = ("run", "--headless")
        good, proven = audit_section_1([flag_row])
    finally:
        FLAG_HOMES.clear()
        FLAG_HOMES.update(saved)
    if not bad:
        failures.append("section 1: a ✅ flag row with no FLAG_HOMES entry was not detected")
    elif not bad_declared:
        failures.append("section 1: a flag its home's clap struct lacks was not detected")
    elif good or not proven:
        failures.append(f"section 1: `xaft run --headless` no longer resolves: {good}")
    else:
        print("  ok  section 1 rejects an undeclared or homeless flag, and resolves a real one")
    for flag, (path, xaft_flag) in FLAG_HOMES.items():
        ok, why = flag_is_declared(path, xaft_flag)
        if not ok:
            failures.append(f"FLAG_HOMES maps `{flag}` to something clap lacks: {why}")

    # 5d. The loader evidence must be able to fail.
    if loader_reads("XAFT_DEFINITELY_NOT_A_LOADER_KEY"):
        failures.append("loader_reads accepted a literal the loader never reads")
    elif not loader_reads("XAFT_CONFIG"):
        failures.append("loader_reads no longer finds `XAFT_CONFIG` in the loader")
    else:
        print("  ok  section 7 loader evidence rejects a key the loader never reads")

    # 6. Every ✅ row must be parsed out of the real matrix, or the whole run
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
    failures, counts, unchecked, removed = run(sections)

    total_rows = sum(counts.values())
    total_unchecked = sum(len(v) for v in unchecked.values())
    total_partial = sum(
        1 for v in unchecked.values() for r in v if r.status.strip() == PARTIAL
    )
    total_removed = sum(len(v) for v in removed.values())
    print(f"\n{total_rows} ✅ rows audited across section(s) {', '.join(sections)}")
    print(
        f"{total_unchecked} non-✅ row(s) NOT audited "
        f"({total_partial} 🟡, {total_unchecked - total_partial} ❌).\n"
        f"  🟡 is a legal status but is not a reachability claim, so those rows\n"
        f"  carry no proof from this run — see the module docstring."
    )
    if total_removed:
        print(
            f"{total_removed} ➖ row(s) removed upstream — not parity rows in either "
            f"direction, excluded from every count above."
        )
    if failures:
        print(f"{len(failures)} UNREACHABLE ✅ rows — a ✅ here is not reachable")
        return 1 if args.check else 0
    print("every ✅ row is backed by reachability evidence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
