#!/usr/bin/env python3
"""
agenthicc-surface.py — print agenthicc's current user-visible surface, per parity-matrix section.

WHY
    docs/parity-matrix.md compares xaft against agenthicc, and its rows were
    extracted from agenthicc's registries once, by hand, months before the
    2026-10-23 re-baseline. agenthicc kept moving: the CLI moved from
    `@command`/`@group` decorators to a declarative manifest and grew a
    `schedules` family and an inbox (and its fifteen global flags had never
    been extracted at all); `BUILTIN_COMMANDS` grew five slash commands; and
    PRD-250 deleted four whole settings classes and 20 keys of the classes
    that survive from `config.py`. A matrix whose agenthicc half is stale
    measures parity against software that no longer exists — rows for surface
    agenthicc deleted keep counting, and new surface never shows up.

    This script makes the next re-baseline mechanical. It reads agenthicc's own
    registries straight from source, prints the surface for each section the
    matrix extracts from them, and — with --diff / --check — names every
    agenthicc entry that has no row, every row whose surface agenthicc no
    longer has, and every row marked ➖ (removed upstream) that agenthicc has
    brought back.

IT NEVER IMPORTS AGENTHICC
    Everything is read with `ast` from the source files. Importing agenthicc
    would execute it (and its optional dependencies), would read whatever
    version happens to be installed rather than the working tree being
    measured, and would make the result depend on the Python environment. The
    only subprocess is `git` against the agenthicc tree, to record which
    commit was measured.

WHAT IT READS (paths relative to the agenthicc checkout)
    section 1  CLI commands
               src/agenthicc/cli/manifest.py `BUILTIN_COMMANDS` — every
               `_ref(module, function, *path, help=...)` is one command path
               (AGX-01 moved built-ins here; the decorators now register
               plugins only). A tree that predates the manifest falls back to
               the `@command(...)` decorators in src/agenthicc/cli/commands/.
               Plus the global flags: every `parser.add_argument("--x", ...)`
               in src/agenthicc/cli/parser.py `_add_global_flags`.
    section 2  slash commands
               src/agenthicc/commands/builtins.py `BUILTIN_COMMANDS` — every
               `Command(name=..., aliases=(...))`.
    section 6  built-in tools
               every `*_AGENT_TOOLS` list/tuple in
               src/agenthicc/tools/**/agent_tools.py, plus any list the
               aggregate `AGENT_TOOLS` in src/agenthicc/agent_tools.py
               re-exports from elsewhere (today: `MCP_AGENT_TOOLS`, defined in
               tools/mcp_guidance.py).
    section 7  configuration keys
               every field of every `@dataclass` in src/agenthicc/config.py,
               in declaration order (ClassVar excluded, private fields kept —
               the matrix has carried `_resolved_profile` since the start),
               plus the config-LOADER keys config.py honours that are not
               dataclass fields, as the pseudo-class `load_config`: `extends`,
               `AGENTHICC_CONFIG` and the `AGENTHICC_<SECTION>_<KEY>` family.
               Each loader key is a probe over config.py's AST, so a loader
               key agenthicc drops shows up as removed.
    informational (not matrix rows)
               dataclasses in src/agenthicc/**/settings.py — configuration
               agenthicc reads OUTSIDE config.py (today: the `[background]`
               table). They are outside section 7's extraction rule; they are
               printed so that a later scoping decision is made on purpose,
               not by omission.

THE --diff RULES
    A matrix row is matched to an agenthicc entry by key:
      section 1  the command path or flag in the "agenthicc command" cell;
                 rows whose source cell starts with "—" are xaft-only
                 (row 7, `session resume`) and are not compared
      section 2  the slash command, and its aliases cell
      section 6  the tool name
      section 7  `Class.key` from the `config.py::Class` source cell
    and reported as
      NEW          agenthicc has it, no row does            -> add a row
      REMOVED      a row (not ➖) names surface agenthicc    -> mark it ➖ with
                   no longer has                               a note naming
                                                               the replacement
      RESURRECTED  a ➖ row names surface agenthicc has      -> un-➖ it
      ALIASES      a section-2 row's aliases disagree
      SOURCE       a section-1/6 row's "agenthicc source" cell does not
                   cite the file that defines the entry
    Renamed surface is REMOVED + NEW; the matrix records a rename by updating
    the row in place, after which the diff is clean again.

USAGE
    scripts/agenthicc-surface.py                       # print the surface
    scripts/agenthicc-surface.py --agenthicc PATH      # another checkout
    scripts/agenthicc-surface.py --json                # machine-readable
    scripts/agenthicc-surface.py --diff                # compare with the matrix
    scripts/agenthicc-surface.py --check               # --diff; exit 1 on drift
    scripts/agenthicc-surface.py --self-test           # prove each drift mode
                                                       # is detected

    The checkout defaults to $AGENTHICC_ROOT, else ~/python_projects/agenthicc,
    else the first ancestor of this repository that has a
    python_projects/agenthicc (the layout this matrix was measured in).

EXIT
    0  printed / no drift
    1  the matrix disagrees with agenthicc (only with --check), or a self-test
       case failed
    2  the agenthicc checkout or the matrix could not be read
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MATRIX = ROOT / "docs" / "parity-matrix.md"


def default_agenthicc() -> Path:
    """$AGENTHICC_ROOT, else ~/python_projects/agenthicc, else the first
    ancestor of this repository that has a python_projects/agenthicc."""
    if os.environ.get("AGENTHICC_ROOT"):
        return Path(os.environ["AGENTHICC_ROOT"])
    candidates = [Path.home() / "python_projects" / "agenthicc"]
    candidates += [p / "python_projects" / "agenthicc" for p in ROOT.parents]
    for candidate in candidates:
        if (candidate / "src" / "agenthicc").is_dir():
            return candidate
    return candidates[0]

STATUSES = ("✅", "🟡", "❌", "➖")
REMOVED = "➖"

# The pseudo-class section 7 files the loader keys under. It names the
# function that honours them, so the matrix's `config.py::load_config` source
# cell is a real location, not an invented class.
LOADER_CLASS = "load_config"

SECTION_TITLES = {
    "1": "CLI commands",
    "2": "Slash commands",
    "6": "Built-in tools",
    "7": "Configuration keys",
}


# ------------------------------------------------------------------ helpers


def _parse(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"), filename=str(path))


def _const_str(node: ast.AST | None) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    return None


def _keyword(call: ast.Call, name: str) -> ast.AST | None:
    for kw in call.keywords:
        if kw.arg == name:
            return kw.value
    return None


def _call_name(call: ast.Call) -> str | None:
    if isinstance(call.func, ast.Name):
        return call.func.id
    if isinstance(call.func, ast.Attribute):
        return call.func.attr
    return None


def _assigned_value(module: ast.Module, name: str) -> ast.AST | None:
    """The value a module-level `name = ...` / `name: T = ...` binds."""
    for stmt in module.body:
        if isinstance(stmt, ast.Assign):
            if any(isinstance(t, ast.Name) and t.id == name for t in stmt.targets):
                return stmt.value
        elif isinstance(stmt, ast.AnnAssign):
            if isinstance(stmt.target, ast.Name) and stmt.target.id == name:
                return stmt.value
    return None


def _module_assignments(module: ast.Module) -> list[tuple[str, ast.AST]]:
    out = []
    for stmt in module.body:
        if isinstance(stmt, ast.Assign):
            for t in stmt.targets:
                if isinstance(t, ast.Name):
                    out.append((t.id, stmt.value))
        elif isinstance(stmt, ast.AnnAssign) and stmt.value is not None:
            if isinstance(stmt.target, ast.Name):
                out.append((stmt.target.id, stmt.value))
    return out


def _first_line(text: str) -> str:
    return " ".join(text.split())


def git_reference(path: Path) -> dict[str, str | int]:
    """Which agenthicc commit was measured, and how dirty its tree was."""

    def run(*args: str) -> str | None:
        try:
            done = subprocess.run(
                ["git", "-C", str(path), *args],
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            )
        except (OSError, subprocess.SubprocessError):
            return None
        return done.stdout

    head = run("rev-parse", "--short", "HEAD")
    status = run("status", "--porcelain")
    return {
        "head": head.strip() if head else "unknown",
        "dirty_paths": len(status.splitlines()) if status is not None else -1,
    }


# ------------------------------------------------------------------ section 1


def parse_manifest(path: Path) -> list[dict[str, str]]:
    """`BUILTIN_COMMANDS` in cli/manifest.py: one entry per `_ref(...)`."""
    value = _assigned_value(_parse(path), "BUILTIN_COMMANDS")
    if not isinstance(value, (ast.Tuple, ast.List)):
        raise ValueError(f"{path}: BUILTIN_COMMANDS is not a literal tuple/list")
    out = []
    for elt in value.elts:
        if not isinstance(elt, ast.Call) or _call_name(elt) != "_ref":
            continue
        args = [_const_str(a) for a in elt.args]
        if len(args) < 3 or any(a is None for a in args):
            raise ValueError(f"{path}:{elt.lineno}: _ref(...) with a non-literal argument")
        module, function, *cmd_path = args
        out.append(
            {
                "key": " ".join(cmd_path),
                "kind": "command",
                "source": f"cli/commands/{module}.py",
                "handler": f"{module}:{function}",
                "help": _first_line(_const_str(_keyword(elt, "help")) or ""),
            }
        )
    return out


def parse_command_decorators(directory: Path) -> list[dict[str, str]]:
    """Pre-manifest trees: `@command("a", "b", help=...)` in cli/commands/*.py."""
    out = []
    for path in sorted(directory.glob("*.py")):
        for node in ast.walk(_parse(path)):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            for deco in node.decorator_list:
                if not isinstance(deco, ast.Call) or _call_name(deco) != "command":
                    continue
                parts = [_const_str(a) for a in deco.args]
                if not parts or any(p is None for p in parts):
                    continue
                doc = ast.get_docstring(node) or ""
                out.append(
                    {
                        "key": " ".join(parts),
                        "kind": "command",
                        "source": f"cli/commands/{path.name}",
                        "handler": f"{path.stem}:{node.name}",
                        "help": _first_line(
                            _const_str(_keyword(deco, "help")) or doc.split("\n")[0]
                        ),
                    }
                )
    return out


def parse_global_flags(path: Path) -> list[dict[str, str]]:
    """Every `--flag` `_add_global_flags` in cli/parser.py registers."""
    module = _parse(path)
    func = next(
        (
            n
            for n in ast.walk(module)
            if isinstance(n, ast.FunctionDef) and n.name == "_add_global_flags"
        ),
        None,
    )
    if func is None:
        raise ValueError(f"{path}: no _add_global_flags function")
    found = []
    for node in ast.walk(func):
        if not isinstance(node, ast.Call) or _call_name(node) != "add_argument":
            continue
        flag = _const_str(node.args[0]) if node.args else None
        if not flag or not flag.startswith("--"):
            continue
        entry = {
            "key": flag,
            "kind": "flag",
            "source": "cli/parser.py",
            "handler": "_add_global_flags",
            "help": _first_line(_const_str(_keyword(node, "help")) or ""),
        }
        found.append(((node.lineno, node.col_offset), entry))
    # ast.walk is breadth-first, not source order; the matrix numbers flags in
    # the order the parser registers them.
    return [entry for _, entry in sorted(found, key=lambda pair: pair[0])]


def extract_cli(root: Path) -> dict:
    src = root / "src" / "agenthicc" / "cli"
    manifest = src / "manifest.py"
    if manifest.is_file():
        commands = parse_manifest(manifest)
        registry = "cli/manifest.py BUILTIN_COMMANDS"
    else:
        commands = parse_command_decorators(src / "commands")
        registry = "@command decorators in cli/commands/*.py (no manifest)"
    flags = parse_global_flags(src / "parser.py")
    return {"registry": registry, "entries": commands + flags}


# ------------------------------------------------------------------ section 2


def extract_slash(root: Path) -> dict:
    path = root / "src" / "agenthicc" / "commands" / "builtins.py"
    value = _assigned_value(_parse(path), "BUILTIN_COMMANDS")
    if not isinstance(value, (ast.List, ast.Tuple)):
        raise ValueError(f"{path}: BUILTIN_COMMANDS is not a literal list")
    out = []
    for elt in value.elts:
        if not isinstance(elt, ast.Call) or _call_name(elt) != "Command":
            continue
        name = _const_str(_keyword(elt, "name"))
        if name is None:
            raise ValueError(f"{path}:{elt.lineno}: Command(...) without a literal name")
        aliases_node = _keyword(elt, "aliases")
        aliases = []
        if isinstance(aliases_node, (ast.Tuple, ast.List)):
            aliases = [a for a in (_const_str(e) for e in aliases_node.elts) if a]
        out.append(
            {
                "key": name,
                "kind": "slash",
                "source": "commands/builtins.py",
                "aliases": aliases,
                "help": _first_line(_const_str(_keyword(elt, "description")) or ""),
            }
        )
    return {"registry": "commands/builtins.py BUILTIN_COMMANDS", "entries": out}


# ------------------------------------------------------------------ section 6

AGENT_TOOLS_LIST = re.compile(r"^[A-Z0-9_]+_AGENT_TOOLS$")


def _tool_names(value: ast.AST) -> list[str] | None:
    if not isinstance(value, (ast.List, ast.Tuple)):
        return None
    names = []
    for elt in value.elts:
        if isinstance(elt, ast.Name):
            names.append(elt.id)
        elif _const_str(elt) is not None:
            names.append(_const_str(elt))
        else:
            return None
    return names


def extract_tools(root: Path) -> dict:
    pkg = root / "src" / "agenthicc"
    lists: dict[str, tuple[str, list[str], str]] = {}
    for path in sorted((pkg / "tools").rglob("agent_tools.py")):
        rel = path.relative_to(pkg).as_posix()
        for name, value in _module_assignments(_parse(path)):
            names = _tool_names(value) if AGENT_TOOLS_LIST.match(name) else None
            if names is not None:
                lists[name] = (rel, names, rel)

    # Lists the aggregate re-exports from a module that is not an
    # agent_tools.py (MCP_AGENT_TOOLS lives in tools/mcp_guidance.py).
    aggregate = pkg / "agent_tools.py"
    if aggregate.is_file():
        module = _parse(aggregate)
        imports: dict[str, str] = {}
        for stmt in module.body:
            if isinstance(stmt, ast.ImportFrom) and stmt.module:
                for alias in stmt.names:
                    imports[alias.asname or alias.name] = stmt.module
        value = _assigned_value(module, "AGENT_TOOLS")
        starred = (
            [e.value.id for e in value.elts if isinstance(e, ast.Starred)
             and isinstance(e.value, ast.Name)]
            if isinstance(value, (ast.List, ast.Tuple))
            else []
        )
        for name in starred:
            if name in lists or name not in imports:
                continue
            mod_path = root / "src" / Path(*imports[name].split("."))
            mod_file = mod_path.with_suffix(".py")
            if not mod_file.is_file():
                continue
            names = _tool_names(_assigned_value(_parse(mod_file), name) or ast.List(elts=[]))
            if names is not None:
                rel = mod_file.relative_to(pkg).as_posix()
                lists[name] = ("agent_tools.py", names, rel)

    entries = []
    for list_name, (source, names, defined_in) in lists.items():
        for tool in names:
            entries.append(
                {"key": tool, "kind": "tool", "source": source, "list": list_name,
                 "defined_in": defined_in}
            )
    return {
        "registry": "*_AGENT_TOOLS in tools/**/agent_tools.py + agent_tools.py AGENT_TOOLS",
        "entries": entries,
    }


# ------------------------------------------------------------------ section 7


def _is_dataclass(cls: ast.ClassDef) -> bool:
    for deco in cls.decorator_list:
        target = deco.func if isinstance(deco, ast.Call) else deco
        if isinstance(target, ast.Name) and target.id == "dataclass":
            return True
        if isinstance(target, ast.Attribute) and target.attr == "dataclass":
            return True
    return False


def _is_classvar(annotation: ast.AST) -> bool:
    target = annotation.value if isinstance(annotation, ast.Subscript) else annotation
    if isinstance(target, ast.Name):
        return target.id == "ClassVar"
    if isinstance(target, ast.Attribute):
        return target.attr == "ClassVar"
    return False


def dataclass_fields(path: Path) -> list[tuple[str, list[str]]]:
    out = []
    for stmt in _parse(path).body:
        if not isinstance(stmt, ast.ClassDef) or not _is_dataclass(stmt):
            continue
        fields = [
            s.target.id
            for s in stmt.body
            if isinstance(s, ast.AnnAssign)
            and isinstance(s.target, ast.Name)
            and not _is_classvar(s.annotation)
        ]
        out.append((stmt.name, fields))
    return out


def _calls(module: ast.Module):
    return (n for n in ast.walk(module) if isinstance(n, ast.Call))


def _probe_extends(module: ast.Module) -> bool:
    return any(
        _call_name(c) in {"pop", "get"} and c.args and _const_str(c.args[0]) == "extends"
        for c in _calls(module)
    )


def _probe_config_env(module: ast.Module) -> bool:
    return any(
        any(_const_str(a) == "AGENTHICC_CONFIG" for a in c.args) for c in _calls(module)
    )


def _probe_env_family(module: ast.Module) -> bool:
    return any(
        _call_name(c) == "startswith" and c.args and _const_str(c.args[0]) == "AGENTHICC_"
        for c in _calls(module)
    )


# (key, what agenthicc does, probe over config.py's AST)
LOADER_PROBES = (
    (
        "extends",
        "`extends = \"base.toml\"` (or a list) in a config file: parents merged "
        "first, cycles rejected (`_resolve_extends`, PRD-113)",
        _probe_extends,
    ),
    (
        "AGENTHICC_CONFIG",
        "environment variable naming an explicit config file that replaces "
        "project discovery (`load_config`, PRD-113)",
        _probe_config_env,
    ),
    (
        "AGENTHICC_<SECTION>_<KEY>",
        "environment override for any `[section] key` "
        "(`_apply_env_overrides`)",
        _probe_env_family,
    ),
)


def extract_config(root: Path) -> dict:
    pkg = root / "src" / "agenthicc"
    config = pkg / "config.py"
    entries = []
    classes = []
    for cls, fields in dataclass_fields(config):
        classes.append({"class": cls, "fields": len(fields)})
        for field in fields:
            entries.append(
                {"key": f"{cls}.{field}", "kind": "field", "class": cls, "field": field,
                 "source": f"config.py::{cls}"}
            )
    module = _parse(config)
    loader = []
    for key, what, probe in LOADER_PROBES:
        if probe(module):
            loader.append(key)
            entries.append(
                {"key": f"{LOADER_CLASS}.{key}", "kind": "loader", "class": LOADER_CLASS,
                 "field": key, "source": f"config.py::{LOADER_CLASS}", "help": what}
            )
    classes.append({"class": LOADER_CLASS, "fields": len(loader)})

    outside = []
    for path in sorted(pkg.rglob("settings.py")):
        if path == config:
            continue
        for cls, fields in dataclass_fields(path):
            outside.append(
                {"file": path.relative_to(pkg).as_posix(), "class": cls, "fields": fields}
            )
    return {
        "registry": "@dataclass fields in config.py + config.py loader keys",
        "entries": entries,
        "classes": classes,
        "outside_rule": outside,
    }


def extract(root: Path) -> dict:
    if not (root / "src" / "agenthicc").is_dir():
        raise FileNotFoundError(f"not an agenthicc checkout: {root}")
    surface = {
        "1": extract_cli(root),
        "2": extract_slash(root),
        "6": extract_tools(root),
        "7": extract_config(root),
    }
    canonical = json.dumps(
        {k: [e["key"] for e in v["entries"]] for k, v in surface.items()},
        sort_keys=True,
    )
    surface["digest"] = hashlib.sha256(canonical.encode()).hexdigest()[:16]
    return surface


# ------------------------------------------------------------------ the matrix


def parse_matrix(text: str) -> dict[str, list[dict]]:
    """Data rows of sections 1, 2, 6 and 7, keyed the way the surface is."""
    out: dict[str, list[dict]] = {k: [] for k in SECTION_TITLES}
    section = None
    for lineno, line in enumerate(text.split("\n"), 1):
        m = re.match(r"^##\s+(\d+)\.\s", line)
        if m:
            section = m.group(1)
            continue
        if line.startswith("## "):
            section = None
            continue
        if section not in out or not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.split("|")]
        if len(cells) < 6 or not cells[1].isdigit():
            continue
        status = cells[-2]
        if status not in STATUSES:
            # Ownership tables (§1c, §2b, §6b) start with a row number too;
            # their last column is prose, which is what makes them not rows.
            continue
        name = cells[2].strip("`").strip()
        row = {"line": lineno, "number": int(cells[1]), "status": status, "name": name,
               "cells": cells}
        if section == "1":
            row["key"] = name
            row["xaft_only"] = cells[3].startswith("—")
        elif section == "2":
            row["key"] = name
            row["aliases"] = re.findall(r"`([^`]+)`", cells[3])
        elif section == "6":
            row["key"] = name
        elif section == "7":
            m = re.match(r"`config\.py::(\w+)`", cells[3])
            if not m:
                continue
            row["key"] = f"{m.group(1)}.{name}"
        out[section].append(row)
    return out


def diff(surface: dict, matrix: dict[str, list[dict]]) -> dict[str, dict[str, list]]:
    result = {}
    for sec in SECTION_TITLES:
        entries = {e["key"]: e for e in surface[sec]["entries"]}
        rows = [r for r in matrix[sec] if not r.get("xaft_only")]
        by_key: dict[str, list[dict]] = {}
        for r in rows:
            by_key.setdefault(r["key"], []).append(r)

        new = [e for k, e in entries.items() if k not in by_key]
        removed = [r for r in rows if r["key"] not in entries and r["status"] != REMOVED]
        resurrected = [r for r in rows if r["key"] in entries and r["status"] == REMOVED]
        duplicates = [rs for rs in by_key.values() if len(rs) > 1]
        sources = []
        if sec in ("1", "6"):
            # The "agenthicc source" cell must cite the file that defines the
            # entry; a cell that names a directory, or a file the surface has
            # moved out of, is how rows 59/60 carried `cli/commands/` for months.
            for r in rows:
                e = entries.get(r["key"])
                if e is not None and r["status"] != REMOVED and e["source"] not in r["cells"][3]:
                    sources.append((r, e))
        aliases = []
        if sec == "2":
            for r in rows:
                e = entries.get(r["key"])
                if e is not None and sorted(e["aliases"]) != sorted(r["aliases"]):
                    aliases.append((r, e))
        matched = sum(1 for r in rows if r["key"] in entries and r["status"] != REMOVED)
        result[sec] = {
            "new": new,
            "removed": removed,
            "resurrected": resurrected,
            "duplicates": duplicates,
            "aliases": aliases,
            "sources": sources,
            "matched": matched,
            "rows": len(matrix[sec]),
            "xaft_only": len(matrix[sec]) - len(rows),
            "marked_removed": sum(1 for r in rows if r["status"] == REMOVED),
            "agenthicc": len(entries),
        }
    return result


def drift_count(result: dict) -> int:
    return sum(
        len(r["new"]) + len(r["removed"]) + len(r["resurrected"])
        + len(r["duplicates"]) + len(r["aliases"]) + len(r["sources"])
        for r in result.values()
    )


def print_diff(result: dict) -> None:
    for sec, r in result.items():
        print(
            f"section {sec} — {SECTION_TITLES[sec]}: agenthicc {r['agenthicc']} entries; "
            f"matrix {r['rows']} rows ({r['matched']} matched, {r['marked_removed']} ➖, "
            f"{r['xaft_only']} xaft-only)"
        )
        for e in r["new"]:
            print(f"  NEW          `{e['key']}` ({e.get('source', '')}) has no row")
        for row in r["removed"]:
            print(
                f"  REMOVED      row {row['number']} `{row['key']}` (line {row['line']}) is "
                f"{row['status']}, but agenthicc no longer has it — mark it ➖"
            )
        for row in r["resurrected"]:
            print(
                f"  RESURRECTED  row {row['number']} `{row['key']}` is ➖, but agenthicc has it"
            )
        for rows in r["duplicates"]:
            nums = ", ".join(str(x["number"]) for x in rows)
            print(f"  DUPLICATE    rows {nums} all name `{rows[0]['key']}`")
        for row, e in r["sources"]:
            print(
                f"  SOURCE       row {row['number']} `{row['key']}` cites {row['cells'][3]}, "
                f"agenthicc defines it in `{e['source']}`"
            )
        for row, e in r["aliases"]:
            print(
                f"  ALIASES      row {row['number']} `{row['key']}` lists {row['aliases']}, "
                f"agenthicc has {e['aliases']}"
            )


# ------------------------------------------------------------------ printing


def print_surface(surface: dict, ref: dict, root: Path, sections: list[str]) -> None:
    print(f"agenthicc: {root}")
    print(f"  HEAD {ref['head']}, {ref['dirty_paths']} uncommitted path(s)")
    print(f"  surface digest {surface['digest']}  (sha256 of the entry keys, sections 1/2/6/7)")
    for sec in sections:
        s = surface[sec]
        print(f"\nsection {sec} — {SECTION_TITLES[sec]}: {len(s['entries'])} entries")
        print(f"  registry: {s['registry']}")
        if sec == "1":
            for kind in ("command", "flag"):
                group = [e for e in s["entries"] if e["kind"] == kind]
                print(f"  {kind}s: {len(group)}")
                for e in group:
                    print(f"    {e['key']:32} {e['source']:28} {e['help'][:60]}")
        elif sec == "2":
            n_alias = sum(len(e["aliases"]) for e in s["entries"])
            print(f"  aliases: {n_alias}")
            for e in s["entries"]:
                alias = ", ".join(e["aliases"]) or "—"
                print(f"    {e['key']:18} {alias:18} {e['help'][:60]}")
        elif sec == "6":
            by_source: dict[str, list[str]] = {}
            for e in s["entries"]:
                label = e["source"]
                if e["defined_in"] != e["source"]:
                    label += f" ({e['list']}, defined in {e['defined_in']})"
                by_source.setdefault(label, []).append(e["key"])
            for source, tools in by_source.items():
                print(f"  {source} ({len(tools)})")
                print(textwrap.fill(", ".join(tools), 96, initial_indent="    ",
                                    subsequent_indent="    "))
        elif sec == "7":
            print(f"  classes: {len(s['classes'])}")
            for c in s["classes"]:
                fields = [e["field"] for e in s["entries"] if e["class"] == c["class"]]
                print(f"  {c['class']} ({c['fields']})")
                print(textwrap.fill(", ".join(fields), 96, initial_indent="    ",
                                    subsequent_indent="    "))
            if s["outside_rule"]:
                total = sum(len(c["fields"]) for c in s["outside_rule"])
                print(
                    f"  outside the section-7 rule (read outside config.py; NOT matrix rows): "
                    f"{total} field(s)"
                )
                for c in s["outside_rule"]:
                    print(f"    {c['file']}::{c['class']} ({len(c['fields'])})")


# ------------------------------------------------------------------ self-test

FAKE_TREE = {
    "src/agenthicc/__init__.py": "",
    "src/agenthicc/cli/manifest.py": """
        BUILTIN_COMMANDS = (
            _ref("alpha", "run_alpha", "alpha", help="Alpha"),
            _ref("alpha", "alpha_beta", "alpha", "beta", help="Alpha beta"),
        )
    """,
    "src/agenthicc/cli/commands/legacy.py": """
        @command("legacy", "cmd", help="Legacy")
        def legacy_cmd(ctx): ...
    """,
    "src/agenthicc/cli/parser.py": """
        def _add_global_flags(parser):
            parser.add_argument("--gamma", help="Gamma")
            parser.add_argument("--delta", action="store_true", help=("Del" "ta"))
    """,
    "src/agenthicc/commands/builtins.py": """
        BUILTIN_COMMANDS: list[Command] = [
            Command(name="/one", aliases=("/uno",), description="One"),
            Command(name="/two", description="Two"),
        ]
    """,
    "src/agenthicc/tools/fs/agent_tools.py": """
        FS_AGENT_TOOLS = [read_file, write_file]
    """,
    "src/agenthicc/tools/web/agent_tools.py": """
        WEB_AGENT_TOOLS: tuple[str, ...] = ("web_open",)
    """,
    "src/agenthicc/tools/guide.py": """
        GUIDE_AGENT_TOOLS = [guide]
    """,
    "src/agenthicc/agent_tools.py": """
        from agenthicc.tools.fs.agent_tools import FS_AGENT_TOOLS
        from agenthicc.tools.guide import GUIDE_AGENT_TOOLS
        AGENT_TOOLS = [*FS_AGENT_TOOLS, *GUIDE_AGENT_TOOLS]
    """,
    "src/agenthicc/config.py": """
        from dataclasses import dataclass
        from typing import ClassVar

        @dataclass
        class FooSettings:
            a: int = 1
            _b: int = 2
            c: ClassVar[int] = 3

        @dataclass(frozen=True)
        class BarSettings:
            d: str = ""

        class NotADataclass:
            e: int = 1

        def _resolve_extends(data):
            return data.pop("extends", None)

        def _apply_env_overrides(environ):
            return [k for k in environ if k.startswith("AGENTHICC_")]
    """,
    "src/agenthicc/background/settings.py": """
        @dataclass
        class BackgroundSettings:
            enabled: bool = True
    """,
}

FAKE_MATRIX = """\
## 1. CLI commands

| # | agenthicc command | agenthicc source | xaft location | Status |
|---|---|---|---|---|
| 1 | `alpha` | `cli/commands/alpha.py` | x | ✅ |
| 2 | `gone` | `cli/commands/gone.py` | x | ❌ |
| 3 | `session resume` | — (xaft-only) | x | ✅ |
| 4 | `--gamma` | `cli/parser.py` | x | ❌ |
| 5 | `--delta` | `cli/parser.py` | x | ➖ |
| 6 | `retired` | `cli/commands/r.py` | removed upstream | ➖ |

| Rows | Family | Why | Owner |
|---|---|---|---|
| 2 | `gone` | prose | someone |

## 2. Slash commands

| # | agenthicc command | aliases | xaft location | Status |
|---|---|---|---|---|
| 1 | `/one` | — | x | ✅ |
| 2 | `/two` | — | x | ❌ |

## 6. Built-in tools

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 1 | `read_file` | `tools/fs/agent_tools.py` | `read_file` | ✅ |
| 2 | `write_file` | `tools/fs/agent_tools.py` | — | ❌ |
| 3 | `web_open` | `tools/web/agent_tools.py` | — | ❌ |
| 4 | `guide` | `agent_tools.py` | — | ❌ |

## 7. Configuration keys

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 1 | `a` | `config.py::FooSettings` | x | ✅ |
| 2 | `_b` | `config.py::FooSettings` | x | ❌ |
| 3 | `extends` | `config.py::load_config` | x | ✅ |
| 4 | `AGENTHICC_<SECTION>_<KEY>` | `config.py::load_config` | x | 🟡 |
| 5 | `AGENTHICC_CONFIG` | `config.py::load_config` | x | ✅ |
"""


def self_test() -> int:
    """Prove the extraction finds what it claims, and every drift mode fails."""
    failures: list[str] = []

    def expect(label: str, ok: bool) -> None:
        print(f"  {'ok  ' if ok else 'FAIL'} {label}")
        if not ok:
            failures.append(label)

    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for rel, body in FAKE_TREE.items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(body).lstrip("\n"), encoding="utf-8")

        s = extract(root)
        keys = {sec: [e["key"] for e in s[sec]["entries"]] for sec in SECTION_TITLES}
        expect("manifest commands are read, nested paths joined",
               keys["1"][:2] == ["alpha", "alpha beta"])
        expect("the manifest wins over leftover decorators", "legacy cmd" not in keys["1"])
        expect("global flags are read, implicit string concatenation included",
               keys["1"][2:] == ["--gamma", "--delta"]
               and s["1"]["entries"][3]["help"] == "Delta")
        expect("slash commands and their aliases are read",
               keys["2"] == ["/one", "/two"] and s["2"]["entries"][0]["aliases"] == ["/uno"])
        expect("tool lists of names and of strings are both read",
               {"read_file", "write_file", "web_open"} <= set(keys["6"]))
        expect("a list re-exported by the aggregate from a non-agent_tools module is read",
               "guide" in keys["6"])
        expect("dataclass fields are read, ClassVar dropped, private kept, plain class ignored",
               [k for k in keys["7"] if not k.startswith(LOADER_CLASS)]
               == ["FooSettings.a", "FooSettings._b", "BarSettings.d"])
        expect("loader probes find what config.py honours and nothing else",
               [k for k in keys["7"] if k.startswith(LOADER_CLASS)]
               == ["load_config.extends", "load_config.AGENTHICC_<SECTION>_<KEY>"])
        expect("settings.py dataclasses are reported outside the rule",
               [c["class"] for c in s["7"]["outside_rule"]] == ["BackgroundSettings"])

        # Pre-manifest trees fall back to the decorators.
        (root / "src/agenthicc/cli/manifest.py").unlink()
        legacy = [e["key"] for e in extract(root)["1"]["entries"] if e["kind"] == "command"]
        expect("a tree with no manifest falls back to @command decorators",
               legacy == ["legacy cmd"])

    # Drift detection against a matrix carrying one instance of every mode.
    fake = _fake_surface()
    result = diff(fake, parse_matrix(FAKE_MATRIX))
    r1, r2, r6, r7 = result["1"], result["2"], result["6"], result["7"]
    expect("NEW: an agenthicc command with no row",
           [e["key"] for e in r1["new"]] == ["alpha beta"])
    expect("REMOVED: a row agenthicc no longer has, not yet ➖",
           [r["key"] for r in r1["removed"]] == ["gone"])
    expect("a ➖ row agenthicc no longer has is not drift",
           "retired" not in {r["key"] for r in r1["removed"]})
    expect("RESURRECTED: a ➖ row agenthicc has brought back",
           [r["key"] for r in r1["resurrected"]] == ["--delta"])
    expect("xaft-only rows are not compared", r1["xaft_only"] == 1)
    expect("ownership-table rows are not matrix rows", r1["rows"] == 6)
    expect("ALIASES: a slash row whose aliases disagree",
           [row["key"] for row, _ in r2["aliases"]] == ["/one"])
    expect("tools: a fully-covered section is clean",
           not r6["new"] and not r6["removed"] and not r6["resurrected"])
    expect("config: a dataclass field with no row is NEW",
           [e["key"] for e in r7["new"]] == ["BarSettings.d"])
    expect("config: a loader key agenthicc no longer honours is REMOVED",
           [r["key"] for r in r7["removed"]] == ["load_config.AGENTHICC_CONFIG"])
    dup = parse_matrix(FAKE_MATRIX + "| 6 | `a` | `config.py::FooSettings` | y | ❌ |\n")
    expect("DUPLICATE: two rows for one key",
           len(diff(fake, dup)["7"]["duplicates"]) == 1)
    expect("drift_count adds every mode up", drift_count(result) == 6)
    moved = diff(fake, parse_matrix(FAKE_MATRIX.replace(
        "| 1 | `read_file` | `tools/fs/agent_tools.py` |", "| 1 | `read_file` | `tools/` |")))
    expect("SOURCE: a row citing the wrong file is reported",
           [row["key"] for row, _ in moved["6"]["sources"]] == ["read_file"])
    expect("an aggregate-only list is cited by its aggregate, agent_tools.py",
           not r6["sources"])
    clean = diff(fake, parse_matrix(
        FAKE_MATRIX.replace("| 2 | `gone` | `cli/commands/gone.py` |",
                            "| 2 | `alpha beta` | `cli/commands/alpha.py` |")
        .replace("| ➖ |\n| 6 | `retired`", "| ❌ |\n| 6 | `retired`")
        .replace("| 1 | `/one` | — |", "| 1 | `/one` | `/uno` |")
        .replace("| 5 | `AGENTHICC_CONFIG` |", "| 5 | `d` |")
        .replace("| `d` | `config.py::load_config` |", "| `d` | `config.py::BarSettings` |")
    ))
    expect("a matrix that covers the surface reports no drift", drift_count(clean) == 0)

    if failures:
        print(f"\nself-test FAILED: {len(failures)} case(s)")
        return 1
    print("\nself-test passed: extraction and every drift mode are detected")
    return 0


def _fake_surface() -> dict:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for rel, body in FAKE_TREE.items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(body).lstrip("\n"), encoding="utf-8")
        return extract(root)


# ------------------------------------------------------------------ main


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1].strip())
    ap.add_argument("--agenthicc", type=Path, default=None,
                    help="agenthicc checkout (default: $AGENTHICC_ROOT, else "
                         "~/python_projects/agenthicc, else the first ancestor of "
                         "this repository holding python_projects/agenthicc)")
    ap.add_argument("--matrix", type=Path, default=MATRIX)
    ap.add_argument("--section", choices=sorted(SECTION_TITLES), action="append")
    ap.add_argument("--json", action="store_true", help="print the surface as JSON")
    ap.add_argument("--diff", action="store_true", help="compare with the matrix")
    ap.add_argument("--check", action="store_true", help="--diff, exit 1 on drift")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    root = (args.agenthicc or default_agenthicc()).expanduser()
    try:
        surface = extract(root)
    except (OSError, ValueError, SyntaxError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    ref = git_reference(root)
    sections = args.section or sorted(SECTION_TITLES)

    if args.json:
        payload = {"agenthicc": str(root), "reference": ref, "digest": surface["digest"]}
        payload.update({sec: surface[sec] for sec in sections})
        print(json.dumps(payload, indent=2, ensure_ascii=False))
        return 0

    if not (args.diff or args.check):
        print_surface(surface, ref, root, sections)
        return 0

    if not args.matrix.is_file():
        print(f"error: matrix not found: {args.matrix}", file=sys.stderr)
        return 2
    matrix = parse_matrix(args.matrix.read_text(encoding="utf-8"))
    result = {k: v for k, v in diff(surface, matrix).items() if k in sections}
    print(f"agenthicc {root} at {ref['head']} ({ref['dirty_paths']} uncommitted path(s)), "
          f"surface digest {surface['digest']}")
    print(f"matrix    {args.matrix}\n")
    print_diff(result)
    n = drift_count(result)
    if n:
        print(f"\n{n} disagreement(s) between the matrix and agenthicc's registries")
        return 1 if args.check else 0
    print("\nthe matrix covers agenthicc's current surface: every entry has a row, "
          "and every row agenthicc no longer has is ➖")
    return 0


if __name__ == "__main__":
    sys.exit(main())
