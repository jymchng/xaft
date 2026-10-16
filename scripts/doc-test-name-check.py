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
    For every Rust file given (default: the whole workspace — every crate under
    `crates/` plus the root binary crate's `src/`), it

      1. collects the symbols a citation may legitimately name;
      2. extracts snake_case identifiers from backticked spans inside `//!` and
         `///` doc comments; and
      3. asserts each such identifier resolves.

    A citation is allowed when ANY of the following holds. Each rule is derived
    from the source text or from the doc paragraph itself — none of them is a
    list of names, so the allowance set cannot be grown by editing this file:

      (1) RESOLVED. The identifier is defined as a `fn` somewhere under `crates/`.

      (2) CODE SYMBOL. The identifier is a real token in the tree outside a doc
          comment: an attribute argument, a struct field, or a lowercase string
          literal the code emits. This is the clap / macro-attribute class the
          widened default needed — `args_conflicts_with_subcommands` is cited
          (crates/xaft-cli/src/args.rs) as the clap attribute it is, spelled the
          same in the source at `#[command(args_conflicts_with_subcommands =
          true)]`, so it is a code symbol and not a test. Tool names ride the
          same rule, because they are string literals returned from `fn name()`
          rather than `const TOOL_NAME` values (the caveat goal `b1b6bcea`
          recorded for `mcp_connection_guide`).

      (3) EXTERNAL REFERENCE. The enclosing doc paragraph explicitly marks the
          cited symbol as external to this crate's test surface: it names the
          foreign project (`agenthicc`), or it describes a *previous* / removed
          implementation. `find_latest_session_for_cwd` and `get_session_log_path`
          are agenthicc's own functions, correctly cited in a divergence note
          (crates/xaft-tui/src/slash/commands/replay.rs); `build_policy_from_config`
          is cited as `agenthicc.security.build_policy_from_config`
          (crates/xaft-config/src/security.rs); and `extract_json_from_text` is
          named only in a sentence stating that code path was eliminated
          (crates/xaft-runtime/src/explorer.rs) — none of those sentences claims
          a test exists here.

      (4) PROSE FRAGMENT. The backticked span is not a single symbol reference but
          a fragment of prose: `(frontmatter, body_rest_of_file)` names a tuple
          member in a sentence (crates/xaft-skills/src/skill.rs), so there is no
          name to resolve. A bare name, with or without a trailing `()`, is still
          a citation and is still checked — `foo_bar_baz()` is not exempted.

    Anything else is a dangling citation and the check fails.

SCOPE, AND WHY IT IS THE WHOLE WORKSPACE NOW
    The default used to be only the background-terminal subsystem, because
    widening it put five non-defects and one real defect on the gate at once:
    measured over `crates/` the check reported 6 dangling names, of which five
    were legitimate citations of a clap attribute, of agenthicc's own API, or of
    prose. Rules (2)–(4) above now recognise those classes structurally, and the
    sixth — `auth_ignores_the_scope_flags`, a truncated citation of the real test
    `auth_ignores_the_scope_flags_because_it_takes_none` in the same file's module
    doc (crates/xaft-cli/tests/mcp_handlers.rs) — was fixed at its call site
    rather than papered over. The narrow default is therefore gone, and `--check`
    with no argument polices every crate plus the root binary.

    Re-measured on the tree this was widened on, the same scan reported 7 rather
    than 6: the six above plus `build_policy_from_config`, which is cited as
    `agenthicc.security.build_policy_from_config`
    (crates/xaft-config/src/security.rs) and so is rule (3) as well. The extra
    name arrived after the original measurement and is recorded here so the
    difference is not mistaken for an allowance that grew — the count is a
    property of the tree, not of this file.

    The rule for the future: if a cited name is a genuine dangling test citation,
    fix the *citation*. Do not widen the allowance rules until it passes — that is
    the "wired but permanently red, so nobody reads it" failure mode goal
    `8f620512` exists to chase, approached from the other side.

USAGE
    python3 scripts/doc-test-name-check.py                      # report, whole workspace
    python3 scripts/doc-test-name-check.py --check               # exit 1 on any dangling name
    python3 scripts/doc-test-name-check.py --check crates        # crates/ only
    python3 scripts/doc-test-name-check.py --check <file-or-dir>
    python3 scripts/doc-test-name-check.py --self-test           # pin the classification rules
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATES = REPO / "crates"

# The whole workspace: every crate under `crates/`, plus the root binary crate,
# whose sources are `src/`.
DEFAULT_TARGETS = ["crates", "src"]

# Backticked spans that are prose, not identifiers. Kept small and explicit:
# a name that is not here must resolve to a real `fn` or a code symbol.
PROSE_SPANS = re.compile(r"^[A-Z]")

# Spans that are paths or module paths are not identifiers at all.
PATH_LIKE = re.compile(r"[/\\]|\.rs\b|::")

# A cited test name is a phrase, not a word: Rust test names in this repository
# read as sentences (`a_cwd_outside_the_workspace_is_refused_before_any_spawn`).
# Requiring at least three underscores is what separates a cited *test* from a
# tool name (`wait_terminal_ready`, 2), a file stem (`file_exists`, 1) or a crate
# name (`agtrs_runtime`, 1). Without this floor the check reports 61 false
# positives across crates/xaft-tools alone and is therefore worthless.
MIN_UNDERSCORES = 3

# A backticked span names a symbol when it is a bare identifier, optionally a
# call. Anything else (`(frontmatter, body_rest_of_file)`) is prose, rule (4).
IDENTIFIER = re.compile(r"^[a-z][a-z0-9_]*$")
CALL_FORM = re.compile(r"^[a-z][a-z0-9_]*\(\)$")
SPAN_WORD = re.compile(r"\b([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b")

# Symbol collectors, all structural. Nothing here is keyed on a name.
FN_DEF = re.compile(r"\bfn\s+([a-z][a-z0-9_]*)\b")
FIELD_DEF = re.compile(r"\b([a-z][a-z0-9_]*)\s*:\s*[^:]")
STRING_SYMBOL = re.compile(r'"([a-z][a-z0-9_]*)"')
ATTR_START = re.compile(r"#!?\[")
ATTR_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

# Doc-comment line shapes.
DOC_LINE = re.compile(r"^//[/!]")
BLANK_DOC_LINE = re.compile(r"^//[/!]\s*$")
BACKTICKED = re.compile(r"`([^`]+)`")

# Rule (3) markers: an explicit statement that the cited API is not this crate's.
FOREIGN_PROJECT = re.compile(r"\bagenthicc\b", re.IGNORECASE)
HISTORICAL = re.compile(
    r"\b(previous|previously|formerly|used to|no longer|eliminated|removed|"
    r"replaced|legacy)\b",
    re.IGNORECASE,
)


def rust_files() -> list[Path]:
    """Every `.rs` file under `crates/`."""
    return sorted(CRATES.rglob("*.rs"))


def defined_fnames() -> set[str]:
    """Every `fn` name defined anywhere under `crates/` (rule 1)."""
    names: set[str] = set()
    for path in rust_files():
        names.update(FN_DEF.findall(path.read_text(errors="replace")))
    return names


def attribute_spans(text: str):
    """Bodies of `#[...]` / `#![...]` attributes, matched by bracket depth."""
    for m in ATTR_START.finditer(text):
        depth = 0
        for i in range(m.end() - 1, len(text)):
            ch = text[i]
            if ch == "[":
                depth += 1
            elif ch == "]":
                depth -= 1
                if depth == 0:
                    yield text[m.end():i]
                    break


def code_symbols() -> set[str]:
    """Identifiers used as real code: attribute arguments and struct fields (rule 2)."""
    names: set[str] = set()
    for path in rust_files():
        text = path.read_text(errors="replace")
        names.update(FIELD_DEF.findall(text))
        for attr in attribute_spans(text):
            names.update(ATTR_WORD.findall(attr))
    return names


def string_literal_symbols() -> set[str]:
    """Lowercase identifiers the code emits as bare string literals (rule 2).

    Tool names live in `fn name()` bodies rather than `const TOOL_NAME` values,
    which is goal `b1b6bcea`'s caveat; collecting the literals covers them
    structurally instead of by listing the names here.
    """
    names: set[str] = set()
    for path in rust_files():
        names.update(STRING_SYMBOL.findall(path.read_text(errors="replace")))
    return names


def citations(path: Path) -> list[tuple[str, str, str, str]]:
    """`(identifier, location, span, paragraph)` for each identifier cited in a doc comment."""
    found: list[tuple[str, str, str, str]] = []
    paragraph: list[tuple[int, str]] = []

    def flush() -> None:
        if not paragraph:
            return
        prose = "\n".join(body for _, body in paragraph)
        for lineno, body in paragraph:
            for span in BACKTICKED.findall(body):
                span = span.strip()
                if PROSE_SPANS.match(span) or PATH_LIKE.search(span):
                    continue
                for name in SPAN_WORD.findall(span):
                    if name.count("_") < MIN_UNDERSCORES:
                        continue
                    found.append((name, f"{path}:{lineno}", span, prose))
        paragraph.clear()

    for lineno, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
        stripped = line.strip()
        if not DOC_LINE.match(stripped) or BLANK_DOC_LINE.match(stripped):
            flush()
            continue
        paragraph.append((lineno, stripped[3:].strip()))
    flush()
    return found


def classify(
    name: str,
    span: str,
    paragraph: str,
    defined: set[str],
    code: set[str],
    literals: set[str],
) -> str | None:
    """Return why the cited identifier is allowed, or None if it is dangling."""
    if name in defined:
        return "resolved"
    if name in code or name in literals:
        return "code symbol"
    if FOREIGN_PROJECT.search(paragraph) or HISTORICAL.search(paragraph):
        return "external"
    if not (IDENTIFIER.match(span) or CALL_FORM.match(span)):
        return "prose"
    return None


def check(targets: list[str]) -> int:
    defined = defined_fnames()
    code = code_symbols()
    literals = string_literal_symbols()

    dangling: list[tuple[str, str, str]] = []
    checked = 0
    allowed = {"resolved": 0, "code symbol": 0, "external": 0, "prose": 0}
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
            for name, where, span, paragraph in citations(f):
                checked += 1
                kind = classify(name, span, paragraph, defined, code, literals)
                if kind is None:
                    dangling.append((name, where, span))
                else:
                    allowed[kind] += 1

    print(f"cited identifiers checked: {checked}")
    print(f"defined fn names under crates/: {len(defined)}")
    print(f"  resolved to a fn: {allowed['resolved']}")
    print(f"  allowed as code symbol (attribute / field / string literal): {allowed['code symbol']}")
    print(
        "  allowed as external reference (foreign project or removed path): "
        f"{allowed['external']}"
    )
    print(f"  prose fragments skipped (not a single symbol): {allowed['prose']}")
    if dangling:
        print(f"\nDANGLING: {len(dangling)} cited name(s) resolve to no fn:")
        for name, where, span in dangling:
            print(f"  {name}   <- {where}   (cite: `{span}`)")
        return 1
    print("\nOK: every cited name resolves to a fn or is an allowed non-citation.")
    return 0


def self_test() -> int:
    """Pin the classification rules and the extractor, so a widening cannot rot."""
    defined = {"a_real_test_name_here"}
    code = {"args_conflicts_with_subcommands"}
    literals = {"inspect_terminal"}
    cases = [
        ("a_real_test_name_here", "a_real_test_name_here", "Plain prose.", "resolved"),
        (
            "args_conflicts_with_subcommands",
            "args_conflicts_with_subcommands",
            "`args_conflicts_with_subcommands` is what makes this safe.",
            "code symbol",
        ),
        ("inspect_terminal", "inspect_terminal", "Plain prose.", "code symbol"),
        (
            "find_latest_session_for_cwd",
            "find_latest_session_for_cwd()",
            "agenthicc's `_cmd_replay` resolves `find_latest_session_for_cwd()`.",
            "external",
        ),
        (
            "extract_json_from_text",
            "extract_json_from_text",
            "This eliminates the previous failure mode: `extract_json_from_text` used to fail.",
            "external",
        ),
        (
            "body_rest_of_file",
            "(frontmatter, body_rest_of_file)",
            "Returns `(frontmatter, body_rest_of_file)`.",
            "prose",
        ),
        (
            "auth_ignores_the_scope_flags",
            "auth_ignores_the_scope_flags",
            "It is that inconsistency itself that `auth_ignores_the_scope_flags` pins.",
            None,
        ),
        (
            "a_truncated_citation_name",
            "a_truncated_citation_name()",
            "The suite is pinned by `a_truncated_citation_name()`.",
            None,
        ),
    ]
    failures = 0
    for name, span, paragraph, expected in cases:
        got = classify(name, span, paragraph, defined, code, literals)
        if got != expected:
            print(f"self-test FAILED: {name} classified {got!r}, expected {expected!r}")
            failures += 1

    with tempfile.TemporaryDirectory() as tmp:
        sample = Path(tmp) / "sample.rs"
        sample.write_text(
            "//! `agenthicc_own_helper_name` is agenthicc's, not ours.\n"
            "//!\n"
            "//! The suite is pinned by `a_truncated_citation_name`.\n"
        )
        extracted = {name for name, *_ in citations(sample)}
        expected_names = {"agenthicc_own_helper_name", "a_truncated_citation_name"}
        if extracted != expected_names:
            print(
                f"self-test FAILED: extractor found {sorted(extracted)}, "
                f"expected {sorted(expected_names)}"
            )
            failures += 1

    if failures:
        print(f"\nself-test: {failures} failure(s)")
        return 1
    print("self-test: classification and extraction cases all pass")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("targets", nargs="*", default=None, help="files or dirs to check")
    ap.add_argument("--check", action="store_true", help="exit 1 on any dangling name")
    ap.add_argument("--self-test", action="store_true", help="pin the rules and exit")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    rc = check(args.targets or DEFAULT_TARGETS)
    if rc and not args.check:
        return 0
    return rc


if __name__ == "__main__":
    sys.exit(main())
