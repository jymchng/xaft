#!/usr/bin/env python3
"""Undo scripts/sandbox-build.sh's OWN path rewrite inside an edited manifest.

WHY THIS EXISTS
   Teardown has to answer one question per manifest: given the pristine
   original, the post-rewrite snapshot, and whatever the operator left on
   disk, produce "the pristine original plus the operator's edits".

   `git merge-file` cannot answer it. The sandbox's rewrite and the operator's
   edit are two sides of a three-way merge with the snapshot as base, and the
   ORDINARY way an operator edits a manifest — appending a dependency to the
   end of a `[dependencies]` block that sits directly under a rewritten
   `agtrs-runtime = { path = ... }` line — lands in the same region the
   sandbox rewrote, so xdiff reports a conflict:

       base (snapshot)      agtrs-workspace = { path = ".../agtrs/agtrs-workspace" }
       ours (operator file) agtrs-workspace = { path = ".../agtrs/agtrs-workspace" }
                            my-new-dep = "1.0"          <-- adjacent insertion
       theirs (pristine)    agtrs-workspace = { path = "../../../agtrs/agtrs-workspace" }

   Both sides changed adjacent lines, so the merge conflicts and teardown takes
   the loud "NEEDS HAND-REAPPLY" branch. Nothing is lost, but a routine append
   costs a hand re-apply on every teardown, which trains operators to ignore
   the guard that is supposed to protect their work.

   The rewrite is not a merge problem at all — it is a SUBTRACTION. The
   sandbox replaced one literal path prefix with another; taking that
   substitution back out line by line yields the answer directly, with no
   conflict, while a merge can only ever report one.

WHAT IT GUARANTEES (the safety property, mechanically)
   * The result is the operator's file with the sandbox's substitutions undone.
     Nothing is deleted, reordered or added: the only lines that differ from
     the input are the ones this function selected, and it asserts exactly
     that before writing.
   * A line is only rewound when its pristine form occurs VERBATIM in the
     original, so the sandbox's own lines are the only ones this can touch. A
     line the operator edited is left alone.
   * The recorded rewrite must explain the snapshot exactly (`snapshot` ==
     `original` with prefix B replaced by prefix A). If it does not, this
     refuses and writes nothing, because a stale or wrong prefix must never be
     able to corrupt a manifest.
   * Refusal is the caller's cue to fall back to the loud branch, so a
     genuinely conflicting edit is never quietly resolved.

USAGE
   sandbox-rewind.py ORIGINAL SNAPSHOT CURRENT OUTPUT PREFIX_A PREFIX_B
   sandbox-rewind.py --self-test

   PREFIX_A is the pristine dependency prefix (`../../../agtrs/`), PREFIX_B the
   one the sandbox wrote in its place. Both are recorded by sandbox-build.sh at
   engage time in $SANDBOX_STATE/rewrite-map.
"""

from __future__ import annotations

import sys

EXIT_OK = 0
EXIT_REFUSED = 1
EXIT_USAGE = 2


class Refused(Exception):
    """The rewrite cannot be undone safely; the caller must go loud."""


def _read(path: str) -> list[str]:
    # readlines() takes an optional positional *hint*; `keepends` is a read()
    # keyword and raises TypeError here. Lines are returned with their
    # terminators either way, which is what the line-for-line gates need.
    with open(path, encoding="utf-8") as fh:
        return fh.readlines()


def rewind(original: list[str], snapshot: list[str], current: list[str],
           prefix_a: str, prefix_b: str) -> tuple[list[str], list[int]]:
    """Return (rewound lines, indexes of the lines that were rewound)."""
    if not prefix_a or not prefix_b or prefix_a == prefix_b:
        raise Refused("the recorded rewrite prefixes are missing or identical")
    if len(original) != len(snapshot):
        raise Refused(
            f"the sandbox snapshot has {len(snapshot)} lines but the pristine "
            f"original has {len(original)}; it is not the rewrite of this file"
        )

    # Gate 1: the recorded rewrite must explain the snapshot line for line.
    for idx, (orig_line, snap_line) in enumerate(zip(original, snapshot)):
        if snap_line.replace(prefix_b, prefix_a) != orig_line:
            raise Refused(
                f"line {idx + 1} of the sandbox snapshot is not explained by the "
                f"recorded rewrite, so that rewrite cannot be trusted to undo it"
            )

    # Only lines the sandbox itself produced may be rewound. Membership in the
    # pristine original is what proves the line came from the sandbox: an
    # operator-edited line has no verbatim counterpart there and is left alone.
    pristine = {line.rstrip("\n") for line in original}

    result: list[str] = []
    changed: list[int] = []
    for idx, line in enumerate(current):
        if prefix_b in line and line.replace(prefix_b, prefix_a).rstrip("\n") in pristine:
            result.append(line.replace(prefix_b, prefix_a))
            changed.append(idx)
        else:
            result.append(line)

    if len(result) != len(current):
        raise Refused("rewinding changed the line count, which it must never do")

    # Gate 2: the result must differ from the input on the selected lines ONLY.
    selected = set(changed)
    for idx, (before, after) in enumerate(zip(current, result)):
        if idx not in selected and before != after:
            raise Refused(
                f"line {idx + 1} was altered without being selected for rewinding"
            )

    # Gate 3: report anything still naming the sandbox. That is not a silent
    # condition -- it is the operator editing a rewritten line, which the
    # caller must escalate rather than guess at.
    stranded = [
        idx + 1
        for idx, line in enumerate(result)
        if prefix_b in line
    ]
    if stranded:
        raise Refused(
            "line(s) "
            + ", ".join(str(n) for n in stranded)
            + " still point into the sandbox after rewinding, so your edit and "
            "the sandbox's rewrite touch the same line"
        )

    return result, changed


def _run(original_path: str, snapshot_path: str, current_path: str,
         output_path: str, prefix_a: str, prefix_b: str) -> int:
    try:
        original = _read(original_path)
        snapshot = _read(snapshot_path)
        current = _read(current_path)
    except OSError as exc:
        print(f"sandbox-rewind: cannot read a manifest: {exc}", file=sys.stderr)
        return EXIT_REFUSED

    try:
        result, changed = rewind(original, snapshot, current, prefix_a, prefix_b)
    except Refused as exc:
        print(f"sandbox-rewind: {exc}", file=sys.stderr)
        return EXIT_REFUSED

    try:
        with open(output_path, "w", encoding="utf-8") as fh:
            fh.writelines(result)
    except OSError as exc:
        print(f"sandbox-rewind: cannot write the rewound manifest: {exc}",
              file=sys.stderr)
        return EXIT_REFUSED

    print(f"rewound {len(changed)} sandbox-rewritten line(s) of "
          f"{len(current)}; your {len(current) - len(changed)} other line(s) "
          f"are untouched")
    return EXIT_OK


# --------------------------------------------------------------------- tests

PRISTINE_A = "../../../agtrs/"
SANDBOX_B = "../../../.xaft-agtrs-sandbox/agtrs/"

ORIGINAL = f"""[package]
name = "xaft-demo"

[dependencies]
# agtrs framework
agtrs-runtime   = {{ path = "{PRISTINE_A}agtrs-runtime" }}
agtrs-workspace = {{ path = "{PRISTINE_A}agtrs-workspace" }}

# Async
tokio        = {{ workspace = true }}
async-trait  = {{ workspace = true }}
"""


def _snapshot() -> str:
    return ORIGINAL.replace(PRISTINE_A, SANDBOX_B)


def _insert_after(marker: str, line: str, text: str) -> str:
    out = []
    for l in text.splitlines(keepends=True):
        out.append(l)
        if l.startswith(marker):
            out.append(line)
    return "".join(out)


def _apply(original: str, snapshot: str, current: str):
    return rewind(original.splitlines(keepends=True),
                  snapshot.splitlines(keepends=True),
                  current.splitlines(keepends=True),
                  PRISTINE_A, SANDBOX_B)


def _self_test() -> int:
    snapshot = _snapshot()
    failures: list[str] = []

    def check(name: str, cond: bool, detail: str = "") -> None:
        if cond:
            print(f"  ok    {name}")
        else:
            print(f"  FAIL  {name}{(': ' + detail) if detail else ''}")
            failures.append(name)

    # 1. The case this script exists for: an append immediately below a
    #    rewritten line. `git merge-file` conflicts on it.
    current = _insert_after("agtrs-workspace", 'my-new-dep = "1.0"\n', snapshot)
    result, changed = _apply(ORIGINAL, snapshot, current)
    text = "".join(result)
    check("an append next to a rewritten line rewinds cleanly",
          result is not None and SANDBOX_B not in text)
    check("the append survives", 'my-new-dep = "1.0"' in text)
    check("every agtrs path is pristine again",
          text.count(PRISTINE_A) == text.count(SANDBOX_B) + ORIGINAL.count(PRISTINE_A))
    # 0-indexed: 3=[dependencies], 4=# agtrs framework, 5/6 are the two agtrs lines.
    check("only the two rewritten lines changed", changed == [5, 6],
          f"changed={changed}")

    # 2. An append at the very end of the file (e.g. after [dev-dependencies]).
    with_tail = ORIGINAL + "\n[dev-dependencies]\ntempfile = \"3\"\n"
    tail_snapshot = with_tail.replace(PRISTINE_A, SANDBOX_B)
    current = tail_snapshot + 'extra = { path = "../extra" }\n'
    result, _ = _apply(with_tail, tail_snapshot, current)
    text = "".join(result)
    check("an append at end of file rewinds cleanly",
          SANDBOX_B not in text and 'extra = { path = "../extra" }' in text)

    # 3. An insertion mid-block, away from the rewritten lines.
    current = snapshot.replace('# Async\n', '# Async\nsome-mid = "1"\n')
    result, _ = _apply(ORIGINAL, snapshot, current)
    text = "".join(result)
    check("a mid-block insertion rewinds cleanly",
          SANDBOX_B not in text and 'some-mid = "1"' in text)

    # 4. A comment at the top of the file (the shape that already worked).
    current = "# my note\n" + snapshot
    result, _ = _apply(ORIGINAL, snapshot, current)
    check("a leading comment rewinds cleanly",
          "".join(result).startswith("# my note\n"))

    # 5. A deletion of a rewritten line is the operator's intent, preserved.
    current = snapshot.replace(
        f'agtrs-runtime   = {{ path = "{SANDBOX_B}agtrs-runtime" }}\n', "")
    result, _ = _apply(ORIGINAL, snapshot, current)
    check("a deleted rewritten line stays deleted",
          "agtrs-runtime" not in "".join(result).split("# agtrs framework")[1])

    # 6. An operator edit TO a rewritten line is genuinely irreconcilable and
    #    must be refused so the caller goes loud, never silently guessed at.
    current = _snapshot().replace(
        f'agtrs-runtime   = {{ path = "{SANDBOX_B}agtrs-runtime" }}',
        f'agtrs-runtime   = {{ path = "{SANDBOX_B}agtrs-runtime", features = ["x"] }}',
    )
    try:
        _apply(ORIGINAL, snapshot, current)
        check("an edit to a rewritten line is refused", False, "it was accepted")
    except Refused as exc:
        check("an edit to a rewritten line is refused", "still point" in str(exc),
              str(exc))

    # 7. A wrong recorded prefix must be caught before it can rewrite anything.
    bogus = _snapshot().replace(SANDBOX_B, "../../../somewhere-else/agtrs/")
    try:
        rewind(ORIGINAL.splitlines(keepends=True),
               bogus.splitlines(keepends=True),
               _snapshot().splitlines(keepends=True),
               PRISTINE_A, SANDBOX_B)
        check("an unexplainable snapshot is refused", False, "it was accepted")
    except Refused as exc:
        check("an unexplainable snapshot is refused", "not explained" in str(exc),
              str(exc))

    # 8. The gate that makes the substitution safe to trust at all.
    try:
        rewind(ORIGINAL.splitlines(keepends=True),
               snapshot.splitlines(keepends=True),
               snapshot.replace("tokio        = { workspace = true }\n", ""),
               PRISTINE_A, PRISTINE_A)
        check("identical prefixes are refused", False, "they were accepted")
    except Refused:
        check("identical prefixes are refused", True)

    # 9. _read() must keep line terminators: every gate above compares lines
    #    positionally, so a dropped trailing newline would let the rewind
    #    silently concatenate the final line. Exercise the real file reader,
    #    not just the in-memory helpers -- a keyword-argument slip in _read is
    #    invisible to cases 1-8 and only surfaces during a real teardown.
    import os
    import tempfile

    def _write(directory: str, name: str, text: str) -> str:
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)
        return path

    with tempfile.TemporaryDirectory() as tmp:
        src = _write(tmp, "snapshot.toml", snapshot)
        lines = _read(src)
        check("_read keeps every line terminator",
              len(lines) == len(snapshot.splitlines(keepends=True))
              and "".join(lines) == snapshot and lines[-1].endswith("\n"))

        # The whole point of case 9: drive the real CLI path end to end on real
        # files, so a mistake in _read/_run surfaces here rather than during a
        # teardown. The operator appends immediately below a rewritten line.
        orig_f = _write(tmp, "orig.toml", ORIGINAL)
        snap_f = _write(tmp, "snap.toml", snapshot)
        cur_f = _write(tmp, "cur.toml",
                       _insert_after("agtrs-workspace", 'adj = "1"\n', snapshot))
        out_f = os.path.join(tmp, "out.toml")
        rc = _run(orig_f, snap_f, cur_f, out_f, PRISTINE_A, SANDBOX_B)
        text = open(out_f, encoding="utf-8").read() if os.path.exists(out_f) else ""
        check("_run rewinds the adjacent append on real files",
              rc == EXIT_OK and SANDBOX_B not in text and 'adj = "1"' in text,
              f"rc={rc}")

        # And a genuinely conflicting edit must REFUSE and write nothing at all.
        bad = snapshot.replace(
            f'agtrs-runtime   = {{ path = "{SANDBOX_B}agtrs-runtime" }}',
            f'agtrs-runtime   = {{ path = "{SANDBOX_B}agtrs-runtime", features = ["x"] }}')
        bad_f = _write(tmp, "bad.toml", bad)
        out2 = os.path.join(tmp, "out2.toml")
        rc = _run(orig_f, snap_f, bad_f, out2, PRISTINE_A, SANDBOX_B)
        check("_run refuses a conflicting edit and writes nothing",
              rc == EXIT_REFUSED and not os.path.exists(out2), f"rc={rc}")

    if failures:
        print(f"sandbox-rewind self-test: {len(failures)} FAILURE(S): "
              + ", ".join(failures))
        return 1
    print("sandbox-rewind self-test: all checks passed")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[1] == "--self-test":
        return _self_test()
    if len(argv) != 7:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        print("usage: sandbox-rewind.py ORIGINAL SNAPSHOT CURRENT OUTPUT "
              "PREFIX_A PREFIX_B", file=sys.stderr)
        return EXIT_USAGE
    return _run(argv[1], argv[2], argv[3], argv[4], argv[5], argv[6])


if __name__ == "__main__":
    sys.exit(main(sys.argv))
