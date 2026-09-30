#!/usr/bin/env python3
"""
flatten_agtrs_workspace.py — turn an agtrs workspace into standalone packages.

WHY
    A path dependency that lives inside another workspace's directory tree is
    attributed by Cargo to the ANCESTOR workspace, even when it declares its own
    [workspace] and even when the ancestor lists it in `exclude`. Verified
    empirically: `cargo metadata` in xaft fails with

        error inheriting `agtrs-runtime` from workspace root manifest's
        `workspace.dependencies.agtrs-runtime`
        Caused by: `dependency.agtrs-runtime` was not found in `workspace.dependencies`

    even though the very same directory resolves perfectly when Cargo is invoked
    from inside it. The `exclude` entry changes nothing (tested with no exclude,
    with the subtree excluded, and with the exact package path excluded).

    The way out is to remove the nested workspace entirely: make every agtrs
    crate a plain, standalone package that inherits nothing. Then there is no
    nested workspace for Cargo to mis-attribute, and xaft can path-depend on
    them directly.

WHAT IT DOES
    For the workspace root and each member manifest:
      * `version.workspace  = true`  ->  `version  = "0.1.0"`
      * `edition.workspace  = true`  ->  `edition  = "2024"`
      * `rust-version.workspace = true` -> `rust-version = "1.86"`
      * `dep = { workspace = true }`  ->  `dep = <the root's definition>`
      * `dep = { workspace = true, features = [...] }`
                                       ->  `dep = { <root def>, features = [...] }`
      * root path deps (`path = "X"`, relative to the workspace root) are
        re-anchored to `../X` when inlined into a member one level down.
      * the root's `[workspace.package]` and `[workspace.dependencies]`
        tables are dropped, leaving no package there. The root stays a VIRTUAL
        workspace that still owns `members` (and `resolver`) -- see the
        comment on the stub below for why `members` must be kept.

    Everything else is left byte-for-byte alone. Run it on a throwaway copy.

USAGE
    flatten_agtrs_workspace.py <workspace-root-dir>
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    sys.exit("needs Python 3.11+ for tomllib")


def load_root(root: Path) -> tuple[dict, dict, str]:
    data = tomllib.loads((root / "Cargo.toml").read_text())
    ws = data.get("workspace", {})
    resolver = str(ws.get("resolver", "2"))
    return ws.get("package", {}), ws.get("dependencies", {}), resolver


def render(name: str, spec: dict, *, reanchor: bool) -> str:
    """Render a dependency spec as TOML inline-table text."""
    if isinstance(spec, str):
        return f'{name} = "{spec}"'
    parts = []
    for key, val in spec.items():
        if key == "path" and reanchor:
            val = f"../{val}"          # member lives one level below the root
        if isinstance(val, bool):
            parts.append(f"{key} = {str(val).lower()}")
        elif isinstance(val, str):
            parts.append(f'{key} = "{val}"')
        elif isinstance(val, list):
            parts.append(f"{key} = [{', '.join(chr(34)+v+chr(34) for v in val)}]")
        else:
            parts.append(f"{key} = {val}")
    return f"{name} = {{ {', '.join(parts)} }}"


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = Path(sys.argv[1]).resolve()
    pkg, deps, resolver = load_root(root)

    version = str(pkg.get("version", "0.0.0"))
    edition = str(pkg.get("edition", "2021"))
    rust_version = str(pkg.get("rust-version", ""))

    # Any member that inherits these gets a literal. All agtrs members sit
    # exactly one directory below the root, which is what makes the "../"
    # re-anchoring below correct.
    members = re.findall(r'^\s*"([^"]+)",\s*$',
                         (root / "Cargo.toml").read_text(), flags=re.M)

    changed = 0
    for member in members:
        manifest = root / member / "Cargo.toml"
        if not manifest.is_file():
            sys.exit(f"missing member manifest: {manifest}")
        text = manifest.read_text()
        original = text

        # 1. field inheritance -> literals
        text = re.sub(r'^version\.workspace\s*=\s*true$', f'version = "{version}"',
                      text, flags=re.M)
        text = re.sub(r'^edition\.workspace\s*=\s*true$', f'edition = "{edition}"',
                      text, flags=re.M)
        if rust_version:
            text = re.sub(r'^rust-version\.workspace\s*=\s*true$',
                          f'rust-version = "{rust_version}"', text, flags=re.M)

        # 2. dependency inheritance -> the root's definition
        def inline(match: re.Match) -> str:
            name = match.group(1)
            extra = (match.group(2) or "").strip().lstrip(",").strip()
            spec = deps.get(name)
            if spec is None:
                sys.exit(f"{manifest}: `{name}` inherits from the workspace but is "
                         f"not in [workspace.dependencies] — cannot flatten safely")
            reanchor = isinstance(spec, dict) and "path" in spec
            spec = dict(spec) if isinstance(spec, dict) else spec

            # A member may narrow the root's feature set, e.g.
            #   tokio = { workspace = true, features = ["test-util"] }
            # The rendered table must UNION the two lists, never repeat the key.
            fm = re.fullmatch(r'features\s*=\s*\[([^\]]*)\]', extra)
            if fm and isinstance(spec, dict):
                base = list(spec.get("features", []))
                want = [v.strip().strip('"') for v in fm.group(1).split(",") if v.strip()]
                spec["features"] = base + [f for f in want if f not in base]
                extra = ""
            return render(name, spec, reanchor=reanchor) + (
                (", " + extra) if extra else "")

        text = re.sub(
            r'^([A-Za-z0-9_-]+)\s*=\s*\{\s*workspace\s*=\s*true\s*(,\s*features\s*=\s*\[[^\]]*\])?\s*\}',
            inline, text, flags=re.M)

        leftover = re.search(r'workspace\s*=\s*true', text)
        if leftover:
            sys.exit(f"{manifest}: unhandled inheritance near "
                     f"{text[max(0,leftover.start()-60):leftover.end()+20]!r}")

        if text != original:
            manifest.write_text(text)
            changed += 1

    # 3. strip the root's workspace tables; nothing is left but a stub.
    #
    # The stub MUST remain a real workspace that still OWNS its members.
    #
    # An empty `[workspace]` looks equivalent and is not: cargo then finds a
    # workspace root at this level, finds the member is not in `members`, and
    # refuses every operation that resolves that member:
    #
    #     error: current package believes it's in a workspace when it's not:
    #     current:   .../agtrs-anthropic/Cargo.toml
    #     workspace: .../agtrs/Cargo.toml
    #     this may be fixable by adding `agtrs-anthropic` to the
    #     `workspace.members` array of the manifest located at: .../Cargo.toml
    #
    # Plain `cargo build`/`check`/`test` never notice, because they resolve
    # the dependency graph without asking for that member's own metadata. But
    # `cargo fmt` does: cargo-fmt enumerates PATH DEPENDENCIES, not just
    # workspace members, and runs `cargo metadata --no-deps --manifest-path
    # <member>` for each one. That per-member call is the one that fails, so
    # the whole `cargo fmt --all` aborted with a usage error before formatting
    # a single xaft file. Verified: `strace` shows cargo-fmt making exactly
    # that call for agtrs-anthropic.
    #
    # Keeping `members` costs nothing, because flattening already replaced every
    # `version.workspace` / `workspace.dependencies` inheritance with literals,
    # so the members no longer inherit anything from this table. `resolver` is
    # carried over because this is now a virtual manifest again, and a virtual
    # manifest with no resolver defaults to resolver 1 with a warning.
    member_lines = "".join(f'    "{m}",\n' for m in members)
    stub = (
        "# Flattened by scripts/flatten_agtrs_workspace.py.\n"
        "# The original [workspace.package] and [workspace.dependencies] tables\n"
        "# were removed so that no member inherits from them -- every\n"
        "# `version.workspace` / `dep = { workspace = true }` has been rewritten\n"
        "# to a literal above. `members` is kept deliberately: this stub still\n"
        "# OWNS the member directories, and dropping them makes cargo reject\n"
        "# every per-member `cargo metadata` call -- which is exactly what\n"
        "# `cargo fmt` does when it walks path dependencies.\n"
        "[workspace]\n"
        f'resolver = "{resolver}"\n'
        "members = [\n"
        f"{member_lines}"
        "]\n"
    )
    (root / "Cargo.toml").write_text(stub)

    print(f"flattened {changed} member manifest(s) across {len(members)} members")
    return 0


if __name__ == "__main__":
    sys.exit(main())
