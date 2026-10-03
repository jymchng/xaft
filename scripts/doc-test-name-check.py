#!/usr/bin/env python3
"""
doc-test-name-check.py — assert every test name cited in a Rust doc comment resolves.

WHY
    A module doc header that says "covered by the tests in `tests/foo.rs`", or
    names a test, is a claim about the *tests*, not about the code. This
    repository has been bitten by that class repeatedly, and every instance was
    a claim that survived because nothing could mechanically disprove it:

      * `crates/xaft-tools/src/terminals/mod.rs` asserted its safety invariants
        "are covered by the tests in `tests/terminal_security.rs`". That file was
        never written; the tests are inline `mod tests` blocks in manager.rs,
        record.rs and redact.rs. A pass checking "are the security properties
        pinned?" against that path would conclude they are not — when the real
        state is that they live elsewhere.
      * `crates/xaft-tools/src/terminals/tools.rs` cited a tool-level test named
        `stop_refuses_an_orphaned_record_instead_of_signalling_a_recycled_pid`
        that does not exist under any name in the tree.
      * `docs/parity-matrix.md` §3c named
        `two_of_the_six_registered_modes_are_shadowed_by_the_alias_table` while
        the committed test was still
        `three_of_the_six_registered_modes_cannot_be_activated_by_their_own_name`.

    All three are the same defect as the false-✅ parity rows this repository
    already chases: a name is asserted to exist, and existence is assumed rather
    than checked.

WHAT IT DOES
    For every Rust file given (default: the background-terminal subsystem), it

      1. collects every `fn` name defined anywhere under `crates/` (unit tests,
         integration tests, helpers — all of them, because a doc comment does not
         distinguish and neither should this check);
      2. extracts snake_case identifiers from backticked spans inside `//!` and
         `///` doc comments; and
      3. asserts each such identifier resolves to a real `fn`.

    Identifiers that are plainly not test names — struct fields, config keys,
    tool names that live in a `fn name()` body rather than a `fn` — are allowed
    through only if they are found in the source tree as a *field or method
    name*, so the allow-list cannot grow by adding a name to this file. Tool
    names are handled explicitly, per goal `b1b6bcea`: agenthicc-tool names are
    string literals returned from `fn name()`, not `const TOOL_NAME` values, so a
    source grep for a `fn` of that name would wrongly flag them.

SCOPE, AND WHY IT IS NOT WIDER BY DEFAULT
    Measured across the whole `crates/` tree this check reports 53 cited
    phrases, of which 6 resolve to no `fn`. All six were examined individually:

      * `args_conflicts_with_subcommands` — a clap derive attribute, correctly
        cited as one (crates/xaft-cli/src/args.rs).
      * `find_latest_session_for_cwd`, `get_session_log_path` — agenthicc's OWN
        function names, correctly cited in a divergence note
        (crates/xaft-tui/src/slash/commands/replay.rs).
      * `extract_json_from_text`, `body_rest_of_file` — method and prose spans,
        not test citations.
      * `auth_ignores_the_scope_flags` — a TRUNCATED citation of the real test
        `auth_ignores_the_scope_flags_because_it_takes_none`
        (crates/xaft-cli/tests/mcp_handlers.rs:633, cited at :32).

    So widening the default to `crates/` today would put a red gate on the
        check for five non-defects plus one real finding that belongs to another
        goal, which is the "wired but permanently red, so nobody reads it"
        failure mode goal `8f620512` exists to chase. The default is therefore
        the background-terminal subsystem, which is clean. Widening it needs the
        agenthicc-API and clap-attribute classes recognised first, and the
        truncated citation fixed; do not widen it by growing an allow-list.

USAGE
    python3 scripts/doc-test-name-check.py            # report
    python3 scripts/doc-test-name-check.py --check    # exit 1 on any dangling name
    python3 scripts/doc-test-name-check.py --check crates   # wider, 6 known
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATES = REPO / "crates"

DEFAULT_TARGETS = ["crates/xaft-tools/src/terminals"]

# Backticked spans that are prose, not identifiers. Kept small and explicit:
# a name that is not here must resolve to a real `fn`.
PROSE_SPANS = re.compile(r"^[A-Z]")

# A cited test name is a phrase, not a word: Rust test names in this repository
# read as sentences (`a_cwd_outside_the_workspace_is_refused_before_any_spawn`).
# Requiring at least three underscores is what separates a cited *test* from a
# tool name (`wait_terminal_ready`, 2), a file stem (`file_exists`, 1) or a crate
# name (`agtrs_runtime`, 1). Without this floor the check reports 61 false
# positives across crates/xaft-tools alone and is therefore worthless.
MIN_UNDERSCORES = 3

# Spans that are paths or module paths are not identifiers at all.
PATH_LIKE = re.compile(r"[/\\]|\.rs\b|::")

# Tool names live in `fn name()` string literals, so they have no `fn` of their
# own. This is goal b1b6bca's caveat, applied deliberately rather than by
# accident: a source grep for `const TOOL_NAME` misses these entirely.
AGENTHICC_TOOL_NAMES = {
    "inspect_terminal",
    "stop_terminal",
    "wait_terminal",
    "wait_terminal_ready",
    "mcp_connection_guide",
}


def defined_fnames() -> set[str]:
    """Every `fn` name defined anywhere under `crates/`."""
    names: set[str] = set()
    for path in CRATES.rglob("*.rs"):
        text = path.read_text(errors="replace")
        for m in re.finditer(r"\bfn\s+([a-z][a-z0-9_]*)\b", text):
            names.add(m.group(1))
    return names


def known_non_fn_identifiers() -> set[str]:
    """Struct-field names, so the check does not flag them."""
    names: set[str] = set()
    for path in CRATES.rglob("*.rs"):
        text = path.read_text(errors="replace")
        for m in re.finditer(r"\b([a-z][a-z0-9_]*)\s*:\s*[^:]", text):
            names.add(m.group(1))
    return names


def cited_identifiers(path: Path) -> dict[str, list[str]]:
    """Backticked test-name phrases in `//!` and `///` lines, with locations."""
    found: dict[str, list[str]] = {}
    for lineno, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
        stripped = line.strip()
        if not (stripped.startswith("//!") or stripped.startswith("///")):
            continue
        body = stripped[3:]
        for span in re.findall(r"`([^`]+)`", body):
            span = span.strip()
            if PROSE_SPANS.match(span) or PATH_LIKE.search(span):
                continue
            for name in re.findall(r"\b([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b", span):
                if name.count("_") < MIN_UNDERSCORES:
                    continue
                found.setdefault(name, []).append(f"{path}:{lineno}")
    return found


def check(targets: list[str]) -> int:
    defined = defined_fnames()
    non_fn = known_non_fn_identifiers()

    dangling: list[tuple[str, str]] = []
    checked = 0
    for target in targets:
        path = REPO / target
        if path.is_dir():
            files = sorted(path.rglob("*.rs"))
        elif path.exists():
            files = [path]
        else:
            print(f"error: {target} does not exist", file=sys.stderr)
            return 2
        for f in files:
            for name, where in sorted(cited_identifiers(f).items()):
                checked += 1
                if name in defined or name in non_fn or name in AGENTHICC_TOOL_NAMES:
                    continue
                dangling.append((name, where[0]))

    print(f"cited identifiers checked: {checked}")
    print(f"defined fn names under crates/: {len(defined)}")
    if dangling:
        print(f"\nDANGLING: {len(dangling)} cited name(s) resolve to no fn:")
        for name, where in dangling:
            print(f"  {name}   <- {where}")
        return 1
    print("\nOK: every cited name resolves.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("targets", nargs="*", default=None, help="files or dirs to check")
    ap.add_argument("--check", action="store_true", help="exit 1 on any dangling name")
    args = ap.parse_args()
    rc = check(args.targets or DEFAULT_TARGETS)
    if rc and not args.check:
        return 0
    return rc


if __name__ == "__main__":
    sys.exit(main())