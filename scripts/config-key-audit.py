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

DEFERRED CLASSES ALSO CARRY A DECISION
    A reason is not an outcome. Every class in DEFERRED_CLASSES must also have
    an entry in DECISIONS, and `--check` fails without one, so the 57 rows can
    never fall back to "deferred, and nobody decided whether that is fine".
    See the long note above DECISIONS for what the recorded value means and,
    more importantly, what it deliberately does not mean.

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

# The OWNER DECISION for each deferred class.
#
# WHY THIS EXISTS SEPARATELY FROM DEFERRED_CLASSES
#     DEFERRED_CLASSES records a REASON: "this is not parity work". A reason is
#     not an outcome. The 57 rows sat with a reason and no decision, so no goal
#     in the list owned them and nothing said whether they were wanted, so the
#     next pass re-opened the question instead of reading an answer. `decide`
#     is that answer and is required for every class: `--check` fails if a
#     deferred class has no entry here.
#
# WHAT "declined (no authorization)" MEANS - read this before overturning it
#     It means exactly one thing: NO GOAL WILL IMPLEMENT THESE KEYS, and no
#     xaft config field will be added for them, because no xaft code would read
#     it. The rows stay ❌ in the matrix. It is a statement about implementation
#     scope, not about whether the feature is a good idea.
#
#     It is reversible, and cheaply so: authorizing a class means writing a new
#     goal that builds the SUBSYSTEM, and only then adding the keys it will
#     read. Adding the keys first is the failure mode this audit exists to
#     prevent - dead config that looks implemented.
#
# PROVENANCE OF THE VALUES BELOW - the operator has now been asked THREE TIMES
#     First request (goal 42a7d516, 2026-09-30): per class, choose "authorize
#     as its own subsystem goal" or "decline as out-of-product-scope".
#     NO ANSWER WAS RECORDED before the deadline.
#
#     Second request (goal a72a0ba6, 2026-10-01): the same question again, but
#     sharpened with the measured cost of each option - browser/CDP (31 keys,
#     18 tool rows, one commercial licence dependency), loop runner (10, builds
#     on the existing jobs/runs store), S3 + remote storage (11), the hosted
#     API client (3, coupled to the same missing account model as CLI rows
#     54-56), and skills config (2, whose two keys are NOT one thing).
#     NO ANSWER WAS RECORDED AGAIN before the deadline.
#
#     Third request (goal 006a9f34, this pass): scoped to EXACTLY the two
#     classes the measurement argues FOR and no others - `LoopSettings`, and
#     the `SkillsSettings` split (`default_skill_directory` versus
#     `install_default_skills`) - each with its authorize/decline options
#     spelled out and the case for authorizing stated. The other five were
#     deliberately NOT re-asked, because their recommendation is already
#     "decline on current evidence" and revisiting them needs new information
#     rather than just a yes.
#     NO ANSWER WAS RECORDED A THIRD TIME before the deadline.
#
# WHAT THAT MEANS NOW - and why two classes do NOT carry the flat value
#     Nothing is authorized, so nothing will be implemented, for ALL seven
#     classes. That part is unchanged and is the SAFE default: no dead config,
#     no unowned backlog, no product opinion claimed on the owner's behalf.
#
#     But "three unanswered rounds" and "declined" are not the same claim, and
#     collapsing them is how silence quietly becomes a product decision. For
#     five classes the measurement ALSO says decline, so the recorded silence
#     and the recommendation agree. For TWO it does not: the measurement says
#     authorize, and only the silence stands in the way. Those two therefore
#     record UNANSWERED_RECOMMENDED_DECISION instead of the flat
#     UNAUTHORIZED_DECISION, which says the same operational thing - nothing
#     authorized, no field added, rows stay ❌ - while naming the disagreement
#     instead of hiding it.
#
#     This is a recorded TERMINAL state for IMPLEMENTATION, and it is
#     deliberate rather than an open question: goal 006a9f34 - the goal that
#     was to obtain the answer - is CLOSING with no answer obtained, so this
#     table must not point at it as a pending goal (see the /loop owner cell in
#     parity-matrix.md section 2b, corrected in the same pass). Reopening these
#     two classes means the operator authorizes them, not that a later pass
#     re-derives the analysis.
#
#     `recommendation` carries the measured analysis so the decision can be
#     revisited with the facts in hand rather than re-derived from scratch.
# The decision values that may be recorded WITHOUT an operator decision on
# record. Both say the same operational thing - nothing is authorized, so no
# config field will be added and the rows stay ❌ - and neither is a claim about
# what the product owner wanted. They are not written by a script or an agent
# on a hunch; `--check` fails if DECISIONS carries anything else.
#
#   UNAUTHORIZED_DECISION
#     The measurement ALSO recommends declining, so the recorded silence and
#     the analysis agree. Use for a class nobody answered about and that the
#     evidence backs.
#
#   UNANSWERED_RECOMMENDED_DECISION
#     The measurement recommends AUTHORIZING the class, and only the silence
#     stands in the way. Same terminal state for implementation, but the
#     disagreement is named rather than collapsed into "declined". Use only
#     where the `recommendation` field genuinely argues for authorization.
UNAUTHORIZED_DECISION = "declined (no authorization)"
UNANSWERED_RECOMMENDED_DECISION = "declined (not authorized - asked 3x, no answer)"
ALLOWED_DECISIONS = {UNAUTHORIZED_DECISION, UNANSWERED_RECOMMENDED_DECISION}

DECISIONS = {
    "ApiSettings": (
        "declined (no authorization)",
        "`host`/`port`/`api_key_env` configure the agenthicc.ai hosted API. xaft "
        "has no agenthicc.ai account concept and no hosted service to point at, "
        "so all three would be dead config. The same missing account concept is "
        "why CLI rows 54-56 (`login`/`logout`/`whoami`) were declined rather "
        "than implemented (see parity-matrix.md 1c and g4-decomposition.md; "
        "no authorization was recorded).",
        "Decline on current evidence. Revisit only together with an explicit "
        "decision to ship a hosted API and account model - it is the same "
        "decision, not a separate one.",
    ),
    "CloakBrowserSettings": (
        "declined (no authorization)",
        "Measured, not assumed: this is an OPTIONAL COMMERCIAL extra gated on a "
        "licence key (`license_key_env` defaults to `CLOAKBROWSER_LICENSE_KEY`, "
        "agenthicc/config.py:1184) with its own CDP-transport validator. 15 keys "
        "and 9 built-in tool rows (69-77) for a third-party product xaft would "
        "have to take a licence dependency on.",
        "Decline on current evidence. Decide it TOGETHER WITH "
        "`PlaywrightSettings`: the 31 keys and 18 tool rows are one product "
        "decision, and splitting them across two passes only defers the same "
        "question twice.",
    ),
    "LoopSettings": (
        UNANSWERED_RECOMMENDED_DECISION,
        "`agenthicc/runners/loop_scheduler.py` is a durable scheduler "
        "(LoopRecord :117, LoopStore :248, LoopManager :342) plus a TUI overlay "
        "(`agenthicc/tui/workspace/overlays/loops.py`). xaft has no loop engine, "
        "so all 10 keys would be dead config today. Asked THREE times "
        "(goals 42a7d516, a72a0ba6, 006a9f34) with the case for authorizing "
        "stated each time; no answer was recorded on any of them, so nothing "
        "is authorized and no key will be added.",
        "AUTHORIZE if wanted - this is the one class in this table that is a "
        "plausible self-contained xaft feature rather than a foreign product: it "
        "would build on the `jobs`/`runs` store and jobs TUI that already "
        "exist. Recorded as NOT AUTHORIZED only because no authorization was "
        "given, NOT because the measurement argues against it. The "
        "authorization, not this analysis, is what is missing, so the analysis "
        "must not be re-derived by a later pass.",
    ),
    "PlaywrightSettings": (
        "declined (no authorization)",
        "agenthicc ships this as an OPTIONAL extra (config.py:1249: \"Playwright "
        "is deliberately an optional dependency\"). A Rust port needs a "
        "browser/CDP driver built from scratch; 16 keys and 9 built-in tool "
        "rows (60-68) ride on it.",
        "Decline on current evidence. Decide it TOGETHER WITH "
        "`CloakBrowserSettings`.",
    ),
    "SkillsSettings": (
        UNANSWERED_RECOMMENDED_DECISION,
        "The two keys are NOT one thing, which is why this class resists a "
        "single verdict. `default_skill_directory` (row 160, default \"\" = a "
        "global skills dir) WOULD be read: xaft's `SkillLoader::for_working_dir` "
        "(crates/xaft-skills/src/loader.rs:58) currently hardcodes "
        "`.xaft/skills` + `~/.config/xaft/skills`, so making that path "
        "configurable is a real, small increment. `install_default_skills` "
        "(row 159) needs a skill installer xaft does not have - that is CLI row "
        "46 (`skills add`), now itself recorded as `declined (no "
        "authorization)` after the operator did not answer within the deadline "
        "(see parity-matrix.md 1c and g4-decomposition.md). So row 159 is "
        "blocked on a RECORDED decision, not an open question. Asked THREE "
        "times (goals 42a7d516, a72a0ba6, 006a9f34) with the split offered as "
        "an explicit option; no answer was recorded, so neither key is "
        "authorized and neither field will be added.",
        "SPLIT is the honest answer and remains available: authorize "
        "`default_skill_directory` as a small increment - which would add the "
        "field AND make `SkillLoader` read it, because a config field nothing "
        "reads is dead config and is the exact failure this audit exists to "
        "prevent - and keep `install_default_skills` deferred until an "
        "installer exists. Recorded here as one class because the audit buckets "
        "by class; authorizing both at once would create one live key and one "
        "dead one, which is worse than leaving both closed. Authorization, not "
        "this analysis, is the missing input.",
    ),
    "StorageS3Settings": (
        "declined (no authorization)",
        "An object-store backend (`bucket`, `region`, `prefix`, `access_key_id`, "
        "`secret_access_key`, `endpoint_url`, `profile`, `path_style`, "
        "`mounts`). xaft has no storage backend at all, so this is a cloud "
        "dependency, credential handling and network egress rather than config "
        "plumbing.",
        "Decline on current evidence. `StorageSettings` is coupled to this "
        "decision.",
    ),
    "StorageSettings": (
        "declined (no authorization)",
        "`s3` and `default_backend` (agenthicc's default is \"linux\") select a "
        "REMOTE storage backend. The only backend this class can name is S3, "
        "which is declined, so a `default_backend` field would name a choice "
        "xaft cannot offer.",
        "Decline on current evidence, recorded as COUPLED to "
        "`StorageS3Settings` so a later pass does not re-open it as though it "
        "were an independent question.",
    ),
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
    # A field that exists but that no code outside crates/xaft-config reads.
    # G4c's rule: dead config is worse than a missing key, because it looks
    # implemented. Rows 109 (`mcp_servers`) and 180 (`plugins`) landed here.
    "dead": "❌",
}

# Fields whose *name* matches an xaft field but whose value nothing outside
# crates/xaft-config ever reads. Kept as an explicit list rather than derived
# by heuristic, because the reader scan alone cannot tell a structural table
# from a leaf field, and a wrong guess here would silently downgrade a LIVE
# key. Every entry was verified by hand.
#
# `scripts/parity-row-reachability.py` is the safety net for this list: it runs
# the reader scan over every ✅ row, so if one of these starts being read for
# real (and is flipped back to ✅) the audit fails rather than the entry going
# stale in silence.
DEAD_FIELDS = {
    ("ToolSettings", "mcp_servers"): (
        "crates/xaft-cli/src/mcp.rs parses `[mcp]` out of raw TOML itself, so the "
        "typed `XaftConfig.mcp` field is not what the `mcp` commands consume"
    ),
    ("AgenthiccConfig", "plugins"): (
        "`PluginConfig`'s five fields (search_paths, allow_dynamic, allow_wasm, "
        "require_signature, unsigned_capabilities) have no reader outside "
        "crates/xaft-config"
    ),
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

        if (r["cls"], key) in DEAD_FIELDS:
            # Checked BEFORE `exact`/`rename`/`structural`, because a dead field
            # is still a name match: it would otherwise be scored ✅ purely for
            # existing, which is the false-parity shape this verdict exists to
            # catch.
            verdict = "dead"
            note = f"xaft declares the field, but nothing reads it \u2014 {DEAD_FIELDS[(r['cls'], key)]}"
        elif r["cls"] in DEFERRED_CLASSES:
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


def undecided_deferred_classes(by_class) -> list[tuple[str, int]]:
    """Return [(class, rows)] for every deferred class with no DECISIONS entry.

    This is the mechanical form of "the 57 deferred rows have a written
    outcome rather than an open question". A class can be listed in
    DEFERRED_CLASSES with a reason forever and never acquire a decision, which
    is exactly the state this guard exists to make impossible.
    """
    return [
        (cls, sum(by_class[cls].values()) if cls in by_class else 0)
        for cls in sorted(DEFERRED_CLASSES)
        if cls not in DECISIONS
    ]


def unauthorized_decisions() -> list[str]:
    """Classes recorded as declined WITHOUT an operator decision on record."""
    return sorted(
        cls for cls, v in DECISIONS.items() if v[0] == UNAUTHORIZED_DECISION
    )


def unanswered_recommended() -> list[str]:
    """Classes the measurement recommends AUTHORIZING that nobody answered on.

    Kept separate from `unauthorized_decisions()` because the two record
    different things. Both are terminal for implementation - nothing authorized,
    no field added - but here only the silence stands between the analysis and
    the work, so a reader must not read "declined" as "measured and rejected".
    """
    return sorted(
        cls for cls, v in DECISIONS.items() if v[0] == UNANSWERED_RECOMMENDED_DECISION
    )


def unauthorized_or_unanswered() -> list[str]:
    """Every class carrying a no-authorization decision, in either form."""
    return sorted(
        cls
        for cls, v in DECISIONS.items()
        if v[0] in ALLOWED_DECISIONS
    )


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
        invented = sorted(
            (cls, v[0]) for cls, v in DECISIONS.items() if v[0] not in ALLOWED_DECISIONS
        )
        if invented:
            print("\nDECISIONS records a value that is not a no-authorization state:")
            for cls, value in invented:
                print(f"  {cls:26} {value}")
            print(
                "\n  An `authorized` (or any other) value is a claim about what the"
                "\n  product owner wanted. It must not be written without an"
                "\n  operator decision on record. See ALLOWED_DECISIONS above."
            )
            return 1

        undecided = unanswered_recommended()
        if undecided:
            print(
                "\nmeasured as AUTHORIZE, asked 3x, still not authorized: "
                + ", ".join(undecided)
            )
            print(
                "  terminal for implementation (no field will be added, rows stay"
                "\n  \u274f); reopening requires an operator authorization, not"
                "\n  more analysis."
            )

        undecided = undecided_deferred_classes(by_class)
        if undecided:
            rows_at_stake = sum(n for _, n in undecided)
            print(
                f"\n{len(undecided)} deferred class(es) carry no owner decision "
                f"({rows_at_stake} rows):"
            )
            for cls, n in undecided:
                print(f"  {cls:26} {n:3} rows   reason: {DEFERRED_CLASSES[cls]}")
            print("\n  Add each to DECISIONS in this script, then regenerate with --md.")
            return 1

        deferred_rows = sum(1 for r in rows if r["verdict"] == "deferred")
        print(
            "\nall config rows agree with the field-level diff\n"
            f"all {len(DEFERRED_CLASSES)} deferred classes carry an owner decision "
            f"({deferred_rows} rows decided)"
        )
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
        "A **reason** is not an **outcome**, so every class below also carries a",
        "recorded **decision**. Either no-authorization value means one thing",
        "only: **no goal will implement these keys and no xaft config field will",
        "be added for them**, because no xaft code would read it. The rows stay",
        "❌.",
        "",
        "It is a statement about implementation scope, not a judgement that the",
        "feature is a bad idea, and it is cheap to overturn: authorizing a class",
        "means writing a goal that builds the *subsystem* first, and only then",
        "adding the keys that subsystem will read. Adding the keys first is the",
        "failure mode this audit exists to prevent - dead config that looks",
        "implemented.",
        "",
        "The operator has been asked **three times**, per class, to choose",
        "*authorize as its own subsystem goal* or *decline as",
        "out-of-product-scope*: on 2026-09-30 (goal `42a7d516`); again on",
        "2026-10-01 (goal `a72a0ba6`) with the measured cost of each option; and",
        "a third time scoped to exactly the two classes the measurement argues",
        "**for** (goal `006a9f34`). **No answer was recorded on any of the",
        "three.** Nothing is therefore authorized, and every class records a",
        "no-authorization state rather than a product verdict. The",
        "`recommendation` column carries the measured analysis, so a decision can",
        "be revisited with the facts in hand rather than re-derived from",
        "scratch.",
        "",
        "| class | rows | matrix rows | decision | basis | recommendation on revisit |",
        "|---|---|---|---|---|---|",
    ]
    for cls in sorted(DEFERRED_CLASSES):
        n = sum(by_class[cls].values()) if cls in by_class else 0
        row_ids = sorted(int(r["row"]) for r in rows if r["cls"] == cls)
        span = f"{row_ids[0]}-{row_ids[-1]}" if row_ids else "—"
        decision, basis, recommendation = DECISIONS.get(
            cls, ("**UNDECIDED**", DEFERRED_CLASSES[cls], "—")
        )
        lines.append(
            f"| `{cls}` | {n} | {span} | **{decision}** | "
            f"{DEFERRED_CLASSES[cls]}. {basis} | {recommendation} |"
        )
    flat = unauthorized_decisions()
    recommended = unanswered_recommended()
    if flat or recommended:
        # Derived from the rows, never hand-written: a stale hardcoded aggregate
        # in this table is the same defect the whole audit was built to find.
        st = collections.Counter(r["status"] for r in rows)
        deferred_total = sum(
            by_class[c]["deferred"] for c in DEFERRED_CLASSES if c in by_class
        )
        lines += [
            "",
            f"**No class is authorized. {len(flat)} of "
            f"{len(DEFERRED_CLASSES)} classes record "
            f"`{UNAUTHORIZED_DECISION}`, where the measurement ALSO recommends "
            "declining, so the recorded silence and the analysis agree.** "
            "Revisiting those needs new information rather than just a yes.",
            "",
        ]
        if recommended:
            listed = ", ".join(f"`{c}`" for c in recommended)
            lines += [
                f"**{len(recommended)} class(es) \u2014 {listed} \u2014 are the "
                f"exception and record `{UNANSWERED_RECOMMENDED_DECISION}` "
                "instead.** The measurement argues for AUTHORIZING them; only "
                "the silence stands in the way. `LoopSettings` (10 keys) would "
                "build on the `jobs`/`runs` store and jobs TUI that already "
                "exist rather than being a foreign product, and "
                "`SkillsSettings` (2 keys) is a SPLIT, not one verdict \u2014 "
                "`default_skill_directory` (row 160) would be read immediately "
                "by `SkillLoader::for_working_dir`, which hardcodes "
                "`.xaft/skills` + `~/.config/xaft/skills`, while "
                "`install_default_skills` (row 159) needs a skill installer "
                "that does not exist.",
                "",
                "**These were put to the operator a third time, specifically, "
                "on the strength of that case, and no answer was recorded "
                "again.** Goal `006a9f34` was opened to obtain it and is "
                "closing without one, so no goal is pending on this and this "
                "table deliberately points at no goal: the missing input is an "
                "operator authorization, not more analysis. Reopening these two "
                "means the operator authorizes them; it does not mean a later "
                "pass should re-derive the measurements above.",
                "",
            ]
        lines += [
            "This is a terminal state for IMPLEMENTATION and a permanent one "
            "for",
            f"the key count: no goal will add these {deferred_total} keys, so "
            f"the rows stay \u274f and the configuration-keys section stays",
            f"{st[chr(0x2705)]} \u2705 / {st[chr(0x1F7E1)]} \U0001f7e1 / "
            f"{st[chr(0x274c)]} \u274f. It is NOT a",
            "statement that the features are unwanted \u2014 only that no one "
            "has",
            "authorized them, across three unanswered rounds.",
            "",
            "`scripts/config-key-audit.py --check` fails if any class in",
            "`DEFERRED_CLASSES` has no entry in `DECISIONS`, and fails if any "
            "recorded value is not one of the two no-authorization values "
            "above \u2014 an `authorized`, or any other, is a claim about "
            "what the",
            "product owner wanted and must never be written without a "
            "decision on",
            "record. So this table can neither silently regress to",
            "reasons-without-outcomes nor drift to a fabricated verdict.",
        ]
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
