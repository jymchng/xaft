#!/usr/bin/env python3
"""Field-level diff of agenthicc's config keys against xaft's real config structs.

WHY THIS EXISTS
    docs/parity-matrix.md marks all 185 configuration-key rows 🟡 by default and
    says so in prose: "Config rows are 🟡 even where an xaft section exists,
    because per-key names and defaults are xaft-specific and have not yet been
    diffed field-by-field". That made every 🟡 a placeholder rather than a
    measurement, and made the 57 ❌ indistinguishable from real absences.

    This script performs that diff mechanically, so a status in the matrix can
    be traced to a field that either exists in crates/xaft-config/src or
    provably does not.

WHAT IT DOES
    1. Parses every `pub struct` in crates/xaft-config/src and records each
       field's WIRE name (the name a TOML author writes), honouring
       `#[serde(rename = "...")]`, container-level `#[serde(rename_all = ...)]`,
       `#[serde(flatten)]` and `#[serde(skip)]`.
    2. Parses section 7 of docs/parity-matrix.md.
    3. Joins the two and classifies every row.

CLASSIFICATION
    exact      the agenthicc key IS a wire field of the xaft struct that
               corresponds to its class                            -> ✅
    collision  the key matches some xaft field, but only in an
               unrelated struct, or the class has no counterpart  -> decision
    absent     no xaft field shares the name at all               -> ❌
    deferred   the row belongs to a class listed in
               DEFERRED_CLASSES                                   -> ❌ (by design)

WHY "COLLISION" EXISTS - the trap this script exists to avoid
    A bare name match across classes proves nothing. Measured on this matrix:
    10 rows in deferred classes matched a xaft field name, and every one was
    coincidence:

        CloakBrowserSettings.enabled   -> ToolConfig.enabled, MemoryConfig.enabled, ...
        CloakBrowserSettings.transport -> McpClientConfig.transport (an MCP wire
                                         protocol, not a browser one)
        ApiSettings.host               -> McpServerConfig.host (the MCP server
                                         bind address, not a hosted API endpoint)
        ApiSettings.api_key_env        -> ProviderConfig.api_key_env (an LLM
                                         provider key, not an agenthicc.ai key)

    Scoring those as green would have manufactured 10 false rows. So a match
    only counts when it lands in the struct that corresponds to the row's
    class, and even then it is evidence the capability exists, not proof the
    default agrees. The owning struct is printed for a human to confirm.

USAGE
    python3 scripts/config-key-audit.py            # report
    python3 scripts/config-key-audit.py --md       # emit docs/config-key-audit.md
    python3 scripts/config-key-audit.py --check    # non-zero if the matrix drifts
"""

from __future__ import annotations

import argparse
import collections
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CONFIG_SRC = ROOT / "crates" / "xaft-config" / "src"
MATRIX = ROOT / "docs" / "parity-matrix.md"

# Whole agenthicc subsystems with no xaft counterpart. G4c was explicitly told
# to defer these: browser/CDP automation and S3 storage are product features,
# not config plumbing, and adding 57 rows of them under "close the config-key
# gaps" would be scope nobody approved. They stay ❌ and are named here so
# their absence is a recorded decision rather than a silent gap.
DEFERRED_CLASSES = {
    "CloakBrowserSettings": "browser automation is a product feature; no xaft browser subsystem",
    "PlaywrightSettings": "browser/CDP automation is a product feature; no xaft browser subsystem",
    "LoopSettings": "autonomous loop runner is a product feature; xaft has no loop engine",
    "StorageS3Settings": "S3 storage is a product feature; xaft has no object-store backend",
    "StorageSettings": "remote storage backend selection has no xaft counterpart",
    "ApiSettings": "agenthicc.ai hosted API client; xaft has no account or hosted-API concept",
    "SkillsSettings": "skill discovery/download config belongs to the skills subsystem (G8)",
}

# Which xaft struct corresponds to each agenthicc config class. A name match
# only counts as coverage when it lands in the struct for the row's own class;
# see the COLLISION note in the module docstring.
CLASS_TO_XAFT_STRUCT = {
    "SecretReference": ["ProviderConfig", "AgentPreset"],
    "RequestOptionSettings": ["ProviderConfig"],
    "ProviderProfile": ["ProviderConfig"],
    "ResolvedProviderProfile": ["ProviderConfig", "ResolvedAgentPreset"],
    "ExecutionSettings": ["ProviderConfig", "CoreConfig", "AgentPreset"],
    "ToolSettings": ["ToolConfig", "McpConfig"],
    "MemorySettings": ["MemoryConfig"],
    "SecuritySettings": ["GuardrailConfig", "CoreConfig"],
    "PluginSettings": ["PluginConfig"],
    "BehaviourSettings": ["CoreConfig", "TuiConfig"],
    "AgentSettings": ["AgentPreset"],
    "AgentsSettings": ["XaftConfig", "AgentPreset"],
    "AgenthiccConfig": ["XaftConfig"],
}

# agenthicc key -> the xaft field that provides the same capability under a
# different name. Keyed by (class, agenthicc key) so a rename in one class is
# not silently credited to another. Every entry is field-for-field equivalent;
# anything merely similar stays in the absent bucket with a written reason.
RENAMES = {
    ("ProviderProfile", "timeout_s"): ("ProviderConfig.timeout_secs",
        "same field, `_secs` suffix instead of `_s`"),
    ("ResolvedProviderProfile", "timeout_s"): ("ProviderConfig.timeout_secs",
        "same field, `_secs` suffix instead of `_s`"),
    ("RequestOptionSettings", "timeout_s"): ("ProviderConfig.timeout_secs",
        "same field, `_secs` suffix instead of `_s`"),
    ("ExecutionSettings", "timeout_s"): ("ProviderConfig.timeout_secs",
        "same field, `_secs` suffix instead of `_s`"),
    ("ProviderProfile", "default_headers"): ("ProviderConfig.headers",
        "same field, drops the `default_` prefix"),
    ("ResolvedProviderProfile", "default_headers"): ("ProviderConfig.headers",
        "same field, drops the `default_` prefix"),
    ("ExecutionSettings", "default_headers"): ("ProviderConfig.headers",
        "same field, drops the `default_` prefix"),
    ("AgenthiccConfig", "providers"): ("XaftConfig.provider",
        "same table, singular because it is a keyed map"),
    ("AgenthiccConfig", "tools"): ("XaftConfig.tool",
        "same table, singular because it is a keyed map"),
    ("AgenthiccConfig", "security"): ("XaftConfig.guardrail",
        "same table; xaft calls it guardrail"),
    ("AgenthiccConfig", "agents"): ("XaftConfig.agent",
        "same table, singular because it is a keyed map"),
    ("AgenthiccConfig", "workflows"): ("XaftConfig.workflow",
        "same table, singular because it holds one mode config"),
    ("ToolSettings", "mcp_servers"): ("XaftConfig.mcp",
        "same table; xaft nests server and client under `[mcp]`"),
}

# Keys that are satisfied by construction rather than by a field.
STRUCTURAL = {
    ("ProviderProfile", "name"): (
        "the TOML table key is the name",
        "xaft writes `[provider.openai]`, so the name is the table header and "
        "needs no field. agenthicc spells it out as `name = \"openai\"`.",
    ),
}

# Rows where a name match is real but the semantics are narrower than the
# agenthicc class implies. Recorded so a green row is never over-claimed.
CAVEATS = {
    27: "`ResolvedAgentPreset.name` is the *agent* preset name, not a provider "
        "profile name. The capability (a named resolved config) exists; the "
        "class in the matrix is looser than the field.",
}

STRUCT_RE = re.compile(r"^pub struct (\w+)\s*\{", re.M)

# The matrix status each verdict earns. A row the diff could not match is ❌,
# never 🟡: 🟡 in this section used to mean "not yet looked at", and leaving it
# there is what made 128 rows unreadable.
STATUS_FOR_VERDICT = {
    "exact": "✅",
    "rename": "✅",
    "structural": "✅",
    "collision": "❌",
    "absent": "❌",
    "deferred": "❌",
}
FIELD_RE = re.compile(r"^\s*pub (\w+)\s*:", re.M)
ATTR_RENAME_RE = re.compile(r'#\[serde\(rename\s*=\s*"([^"]+)"\s*\)\]')
ATTR_RENAME_ALL_RE = re.compile(r'#\[serde\(rename_all\s*=\s*"([^"]+)"\s*\)\]')
ATTR_FLATten_RE = re.compile(r"#\[serde\(flatten\)\]")


def kebab(name: str) -> str:
    return name.replace("_", "-")


def snake(name: str) -> str:
    return name.replace("-", "_")


def block_after(text: str, open_brace: int) -> str:
    """Return the text between `{` at `open_brace` and its matching `}`.

    Brace counting is naive about braces inside string literals. That is fine
    here because the bodies we scan are struct definitions whose doc comments
    and types contain no unbalanced braces; it would not be fine for arbitrary
    Rust, which is why this is a script scoped to one directory rather than a
    general parser.
    """
    depth = 1
    i = open_brace + 1
    while depth and i < len(text):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        i += 1
    return text[open_brace + 1 : i - 1]


def parse_xaft_fields() -> dict[str, dict[str, str]]:
    """Return {struct_name: {wire_field: rust_field}} for xaft's config structs."""
    structs: dict[str, dict[str, str]] = {}
    for path in sorted(CONFIG_SRC.glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for m in STRUCT_RE.finditer(text):
            name = m.group(1)
            body = block_after(text, m.end() - 1)

            # Container-level rename_all applies to every field in the struct.
            container = text[max(0, m.start() - 400) : m.start()]
            all_m = ATTR_RENAME_ALL_RE.search(container)

            fields: dict[str, str] = {}
            # Track the region between the previous field and this one so a
            # rename attribute is only ever attributed to the field it is
            # actually attached to. Scanning "everything before the field"
            # silently maps every field in a struct to the first rename in it.
            prev_end = 0
            for fm in FIELD_RE.finditer(body):
                attrs = body[prev_end : fm.start()]
                prev_end = fm.end()
                field = fm.group(1)
                if ATTR_FLATten_RE.search(attrs) and not ATTR_RENAME_RE.search(attrs):
                    # A flattened map contributes its inner keys at runtime, not
                    # a field of this name; record it so a match is possible.
                    fields.setdefault("<flattened>", field)
                    continue
                rm = ATTR_RENAME_RE.search(attrs)
                if rm:
                    wire = rm.group(1)
                elif all_m and all_m.group(1) == "kebab-case":
                    wire = kebab(field)
                else:
                    wire = field
                fields[wire] = field
            structs[name] = fields
    return structs


def index_wire_names(structs: dict[str, dict[str, str]]) -> dict[str, list[str]]:
    """Invert to {wire_name: [structs that declare it]}, ignoring placeholders."""
    index: dict[str, list[str]] = collections.defaultdict(list)
    for struct, fields in structs.items():
        for wire in fields:
            if wire.startswith("<"):
                continue
            index[wire].append(struct)
    return index


ROW_RE = re.compile(r"^\|\s*(\d+)\s*\|\s*`([^`]+)`\s*\|\s*`config\.py::(\w+)`\s*\|(.*?)\|(.+?)\|\s*$")
CLASS_HEADER_RE = re.compile(r"^### `(\w+)` \((\d+) keys\)\s*$")


def parse_matrix_section() -> list[dict[str, str]]:
    """Parse section 7 (Configuration keys) of the parity matrix."""
    lines = MATRIX.read_text(encoding="utf-8").split("\n")
    try:
        start = next(i for i, l in enumerate(lines) if l.startswith("## 7. Configuration keys"))
    except StopIteration:
        sys.exit("could not find '## 7. Configuration keys' in the parity matrix")

    rows: list[dict[str, str]] = []
    for line in lines[start + 1 :]:
        if line.startswith("## "):
            break
        m = ROW_RE.match(line)
        if m:
            rows.append(
                {
                    "row": m.group(1),
                    "key": m.group(2),
                    "cls": m.group(3),
                    "xaft": m.group(4).strip(),
                    "status": m.group(5).strip(),
                }
            )
    return rows


def classify(rows, index, structs) -> list[dict[str, str]]:
    out = []
    for r in rows:
        key = r["key"]
        norm = snake(key)
        owners = index.get(key) or index.get(norm) or []
        corresponding = CLASS_TO_XAFT_STRUCT.get(r["cls"], [])
        # The evidence only counts if the match is in a struct that stands for
        # this row's class. `enabled` on ToolConfig is not evidence that
        # CloakBrowserSettings.enabled exists.
        real = [s for s in owners if s in corresponding]

        if r["cls"] in DEFERRED_CLASSES:
            verdict = "deferred"
            note = DEFERRED_CLASSES[r["cls"]]
            if real:
                note += f" (name collides with {', '.join(real)}, an unrelated struct)"
        elif (r["cls"], key) in RENAMES:
            target, why = RENAMES[(r["cls"], key)]
            verdict = "rename"
            note = f"xaft `{target}` - {why}"
        elif (r["cls"], key) in STRUCTURAL:
            target, why = STRUCTURAL[(r["cls"], key)]
            verdict = "structural"
            note = f"{target} - {why}"
        elif real:
            verdict = "exact"
            note = f"xaft field `{key}` on `{', '.join(real)}`"
        elif owners:
            verdict = "collision"
            note = (
                "no field in "
                + (", ".join(corresponding) or "any mapped struct")
                + "; the name exists only in unrelated `"
                + ", ".join(owners)
                + "`"
            )
        else:
            verdict = "absent"
            note = (
                "no xaft field shares this name; nearest struct is "
                + (", ".join(corresponding) or "n/a")
            )

        r = dict(r)
        r["verdict"] = verdict
        r["owners"] = ",".join(sorted(real or owners))
        r["evidence"] = note
        if verdict == "rename":
            r["target"] = RENAMES[(r["cls"], key)][0]
        elif verdict == "structural":
            r["target"] = STRUCTURAL[(r["cls"], key)][0]
        else:
            r["target"] = r["owners"]
        if int(r["row"]) in CAVEATS:
            r["evidence"] += f"  CAVEAT: {CAVEATS[int(r['row'])]}"
        out.append(r)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--md", action="store_true", help="write docs/config-key-audit.md")
    ap.add_argument("--apply", action="store_true", help="rewrite the matrix from the diff")
    ap.add_argument("--check", action="store_true", help="exit non-zero on matrix drift")
    args = ap.parse_args()

    structs = parse_xaft_fields()
    index = index_wire_names(structs)
    rows = classify(parse_matrix_section(), index, structs)

    counts = collections.Counter(r["verdict"] for r in rows)
    print(f"xaft config structs : {len(structs)}")
    print(f"distinct wire fields: {len(index)}")
    print(f"matrix config rows  : {len(rows)}")
    print()
    for verdict in ("exact", "rename", "structural", "collision", "absent", "deferred"):
        print(f"  {verdict:11} {counts[verdict]:3}")
    covered = counts["exact"] + counts["rename"] + counts["structural"]
    print(f"\n  covered ={covered}   not covered ={len(rows) - covered}")
    print()

    by_class: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for r in rows:
        by_class[r["cls"]][r["verdict"]] += 1
    print(f"{'class':26} {'rows':>5} {'cov':>4} {'coll':>5} {'absent':>7} {'defer':>6}")
    for cls in sorted(by_class):
        c = by_class[cls]
        cov = c["exact"] + c["rename"] + c["structural"]
        print(f"{cls:26} {sum(c.values()):5} {cov:4} {c['collision']:5} {c['absent']:7} {c['deferred']:6}")

    if args.md:
        write_markdown(rows, by_class, counts, index)
        print(f"\nwrote {ROOT / 'docs' / 'config-key-audit.md'}")

    if args.apply:
        changed = apply_to_matrix(rows)
        print(f"\nrewrote {changed} row(s) in docs/parity-matrix.md "
              "(aggregates still need a manual pass)")

    if args.check:
        drift = []
        for r in rows:
            expected = STATUS_FOR_VERDICT[r["verdict"]]
            if r["status"] != expected:
                drift.append((r["row"], r["key"], r["status"], expected, r["verdict"]))
        if drift:
            print(f"\n{len(drift)} row(s) whose recorded status disagrees with the diff:")
            for row, key, got, want, verdict in drift[:60]:
                print(f"  row {row:>4} {key:34} matrix={got} diff={want} ({verdict})")
            return 1
        print("\nall config rows agree with the field-level diff")
    return 0


def apply_to_matrix(rows) -> int:
    """Rewrite section 7's status and xaft-location columns from the diff."""
    by_row = {int(r["row"]): r for r in rows}
    lines = MATRIX.read_text(encoding="utf-8").split("\n")
    changed = 0
    for i, line in enumerate(lines):
        m = ROW_RE.match(line)
        if not m:
            continue
        r = by_row.get(int(m.group(1)))
        if r is None:
            continue
        status = STATUS_FOR_VERDICT[r["verdict"]]
        if r["status"] == status and r["xaft"] == location_for(r):
            continue
        lines[i] = (
            f"| {m.group(1)} | `{m.group(2)}` | `config.py::{m.group(3)}` "
            f"| {location_for(r)} | {status} |"
        )
        changed += 1
    MATRIX.write_text("\n".join(lines), encoding="utf-8")
    return changed


def location_for(r) -> str:
    """The xaft column: where the capability lives, or why it does not."""
    if r["verdict"] in ("exact", "rename", "structural"):
        return f"crates/xaft-config `{r['target']}`"
    if r["verdict"] == "deferred":
        return "— (deferred subsystem)"
    return "—"


def write_markdown(rows, by_class, counts, index) -> None:
    lines = [
        "# Config-key audit (generated)",
        "",
        "<!-- GENERATED by scripts/config-key-audit.py — do not edit by hand. -->",
        "",
        "Field-level diff of every configuration-key row in `docs/parity-matrix.md`",
        "against the structs in `crates/xaft-config/src`. Regenerate with:",
        "",
        "```",
        "python3 scripts/config-key-audit.py --md",
        "```",
        "",
        "A row is covered when the capability exists in xaft, by one of four",
        "measured routes:",
        "",
        "* `exact` - the agenthicc key is a wire field of the struct that",
        "  corresponds to its class;",
        "* `rename` - a documented different xaft field provides the same",
        "  capability (`timeout_s` -> `timeout_secs`, `providers` -> `provider`);",
        "* `structural` - satisfied by construction rather than by a field (the",
        "  TOML table key is the name);",
        "* anything else is not covered.",
        "",
        "A name match in an UNRELATED struct is not coverage. Ten rows in deferred",
        "classes match a xaft field name by coincidence (`ApiSettings.host` ->",
        "`McpServerConfig.host`) and are recorded as collisions, not wins.",
        "",
        f"**{len(rows)} rows: {counts['exact']} exact, {counts['rename']} rename, "
        f"{counts['structural']} structural, {counts['collision']} collision, "
        f"{counts['absent']} absent, {counts['deferred']} deferred by design.**",
        "",
        "## Per-class summary",
        "",
        "| agenthicc class | rows | covered | collision | absent | deferred |",
        "|---|---|---|---|---|---|",
    ]
    for cls in sorted(by_class):
        c = by_class[cls]
        cov = c["exact"] + c["rename"] + c["structural"]
        lines.append(
            f"| `{cls}` | {sum(c.values())} | {cov} | {c['collision']} "
            f"| {c['absent']} | {c['deferred']} |"
        )
    lines += [
        "",
        "## Deliberately deferred classes",
        "",
        "These are whole agenthicc subsystems, not missing keys. They stay ❌ on",
        "purpose; closing them is new product work, not parity work.",
        "",
        "| class | rows | why it is out of scope |",
        "|---|---|---|",
    ]
    for cls, why in sorted(DEFERRED_CLASSES.items()):
        n = sum(by_class[cls].values()) if cls in by_class else 0
        lines.append(f"| `{cls}` | {n} | {why} |")
    lines += [
        "",
        "## Per-row evidence",
        "",
        "| row | agenthicc key | class | verdict | xaft location |",
        "|---|---|---|---|---|",
    ]
    for r in rows:
        loc = f"`{r['target']}`" if r.get("target") else "—"
        lines.append(
            f"| {r['row']} | `{r['key']}` | `{r['cls']}` | {r['verdict']} | {loc} |"
        )
    lines += ["", "## Per-row reasoning", "", "| row | key | verdict | evidence |", "|---|---|---|---|"]
    for r in rows:
        lines.append(f"| {r['row']} | `{r['key']}` | {r['verdict']} | {r['evidence']} |")
    (ROOT / "docs" / "config-key-audit.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
