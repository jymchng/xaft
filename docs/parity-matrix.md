# Parity Matrix — agenthicc → xaft

Tracks **user-visible surface parity** between the Python reference implementation
`agenthicc` (`~/python_projects/agenthicc`) and the Rust port `xaft`.

Scope is deliberately the *surface a user can touch*: CLI commands, slash commands,
TUI triggers and keybindings, built-in tools, configuration keys, and modes. Internal
architecture is **not** required to match — xaft is free to be idiomatic Rust.

Legend: ✅ present · 🟡 partial / divergent semantics · ❌ absent ·
➖ removed upstream (agenthicc deleted it; the row is kept for the record and
excluded from every count)

---

## Re-baseline — 2026-10-23, against agenthicc `336d4bd` plus its working tree

The agenthicc half of sections 1, 2, 6 and 7 was re-derived from agenthicc's
**current** registries: HEAD `336d4bd` plus the uncommitted working tree as it
stood on 2026-10-23 (123 changed paths, including the then-untracked
`cli/manifest.py`; surface digest `758d044965764e5f`). The extraction is a
checked-in script that reads agenthicc's source with `ast` and never imports it:

```
python3 scripts/agenthicc-surface.py              # print agenthicc's current surface
python3 scripts/agenthicc-surface.py --check      # diff it against this matrix
python3 scripts/agenthicc-surface.py --self-test  # prove every drift mode is caught
```

`--check` names every agenthicc entry with no row, every row whose surface
agenthicc no longer has that is not yet ➖, and every ➖ row agenthicc has
brought back; on this document it is clean. The next re-baseline is: run
`--check`; add a row with the section's next number for each NEW entry, judged
against xaft's code; mark each REMOVED row ➖ with what replaced it (section 7
also records the removal in `scripts/config-key-audit.py`); then let
`parity-matrix-stats.py`, `config-key-audit.py`, `cli-row-ownership.py` and
`parity-row-reachability.py` (all with `--check`) name every aggregate and
owner table left to update.

| Section | rows before | rows after | ✅ | 🟡 | ❌ | ➖ (not counted) |
|---|---|---|---|---|---|---|
| 1. CLI commands | 60 | 105 | 57 → 63 | 2 → 9 | 1 → 33 | 0 |
| 2. Slash commands | 26 | 31 | 26 → 26 | 0 → 0 | 0 → 5 | 0 |
| 6. Built-in tools | 78 | 78 | 63 → 63 | 0 → 0 | 15 → 15 | 0 |
| 7. Configuration keys | 185 | 154 | 153 → 138 | 1 → 2 | 31 → 14 | 37 |
| whole matrix | 384 | 403 | 326 → 317 (85% → 79%) | 3 → 11 | 55 → 75 | 37 |

What moved upstream, and what this pass did about it:

* **CLI (§1).** Built-in commands moved from `@command`/`@group` decorators to a
  declarative manifest, `cli/manifest.py` (AGX-01; the decorators now register
  plugins only). New surface, rows 61–90: bare `jobs`, `jobs priority`, the five
  `jobs inbox` commands, `jobs notifications`, bare `runs`, the `schedules`
  family (bare plus 11 subcommands), five more `worktrees` commands and
  `agents actions` / `action`. Rows 91–105 are the fifteen global flags of
  `cli/parser.py::_add_global_flags`, which the original extraction never took.
  **Renamed:** `trust cli` became `trust list` / `add` / `revoke` (PRD-250
  S-02); row 47 is now `trust add` and its siblings are rows 82–83.
  **Re-judged:** row 58 `run` — agenthicc's `run` only submits background
  sessions (`--background`), which xaft's foreground `run` does not do, so ✅
  became 🟡. Nothing in section 1 was removed upstream.
* **Slash commands (§2).** `BUILTIN_COMMANDS` grew from 26 to 31: `/schedules`,
  `/schedule`, `/inbox`, `/background` (alias `/bg`) and `/detach`, rows 27–31,
  all ❌.
* **Built-in tools (§6).** Unchanged: the same 78 tools in the same eight lists.
* **Configuration keys (§7).** agenthicc `fb63025` (PRD-250 S-03) deleted every
  setting nothing read: the classes `MemorySettings`, `ApiSettings`,
  `StorageS3Settings` and `StorageSettings`, and 20 keys of classes that
  survive — `hooks`, `memory`, `api` and `storage` on `AgenthiccConfig`,
  `max_concurrent_intents`, `agent_pool_size` and `provider_capabilities` on
  `ExecutionSettings`, `sandbox_mode` and both resource ceilings on
  `SecuritySettings`, `verbose` and `confirm_exits` on `BehaviourSettings`, and
  every `PluginSettings` key but `strict_cli_shadow` (extension trust moved to
  `agenthicc trust` records). Those **37 rows are ➖**, each naming what replaced
  it; 17 of them had been ✅. New surface: three `BehaviourSettings` resume-preview
  bounds (rows 186–188, ❌) and the config-loader keys the original extraction
  never took (`load_config`, rows 189–191): `extends` and `AGENTHICC_CONFIG` are
  ✅ — xaft's `extends` and `XAFT_CONFIG`, commit `2428e19`, which had recorded
  "no matrix row yet" — and the `AGENTHICC_<SECTION>_<KEY>` family is 🟡.
  `config-key-audit.py`'s three remaining deferred classes were exactly the
  deleted `ApiSettings` / `Storage*`, so its DEFERRED/DECISIONS tables are
  empty and the decisions are kept, with the reason, in `RETIRED_DECISIONS`.

Deliberately **not** rows, recorded so the omission is a decision rather than a
gap: agenthicc's `[background]` table (`background/settings.py`, 42 fields across
`[background]`, `.manager`, `.notifications` and its webhooks) is configuration
read outside `config.py`, so it is outside section 7's extraction rule — the
script lists it under "outside the section-7 rule" — and the provider-specific
environment shortcuts (`PROVIDER_ENV_SHORTCUTS`, e.g. `OPENAI_MODEL`) have no
rows either. Section 8 (workflows) was not re-derived; its 12 rows are counted
as they stand.

Of the 42 open CLI rows, 39 were opened by this pass and are recorded in §1c as
**authorized, unclaimed**: the operator's 2026-10-23 authorization covers them
and no branch has taken them yet. `scripts/cli-row-ownership.py` lists them by
number on every run.

---

## How to read this document

Each row cites the **agenthicc** source location that defines the feature and the
**xaft** crate/module that implements (or should implement) it. Status is updated as
gaps close. See `knowledge/tui-parity-gap-analysis.md` for the detailed TUI gap
narrative (gaps 1–11) that this matrix indexes.

**Source of truth.** agenthicc entries come from its own registries, and since
the 2026-10-23 re-baseline sections 1, 2, 6 and 7 are re-derived mechanically by
`scripts/agenthicc-surface.py`: `BUILTIN_COMMANDS` in `src/agenthicc/cli/manifest.py`
plus the global flags of `cli/parser.py` (the original extraction read the
`@command`/`@group` decorators in `src/agenthicc/cli/commands/*`), the
`BUILTIN_COMMANDS` list in `src/agenthicc/commands/builtins.py`, the
`*_AGENT_TOOLS` lists in `src/agenthicc/tools/**/agent_tools.py`, and the
dataclasses and loader keys in `src/agenthicc/config.py`. Section 3 still comes
from `src/agenthicc/modes/builtin.py`. xaft entries were
extracted from the clap enums in `crates/xaft-cli/src/args.rs`, the `SlashCommand`
enum in `crates/xaft-tui/src/slash/mod.rs`, `TOOL_NAME` consts in `crates/xaft-tools`,
the config structs in `crates/xaft-config`, and `crates/xaft-tui/src/mode/builtins.rs`.

---

## 0. Is a ✅ here actually reachable? (`scripts/parity-row-reachability.py`)

A ✅ is a claim that **a user can do this**, not that a name appears
somewhere. Three separate defects have been the same defect:

* G5 found `/usage` marked ✅ because the name was in
  `CommandMeta::aliases`, while `SlashCommandParser::parse` resolves a trigger
  by COMMAND_TABLE *key* lookup — so the only string a user could type
  answered `Unknown command: /usage`.
* Commit `5f06350` shipped a doc comment advertising a bare `xaft runs` that
  clap rejected with "requires a subcommand".
* Rows 109 and 180 (below) were ✅ because `crates/xaft-config` *declared*
  `XaftConfig.mcp` and `XaftConfig.plugins`, while no other crate ever read
  either field.

So the ✅ mark is earned here, per section, against the artifact that
*defines* reachability — never against a grep of the source:

| Section | Evidence | Re-verified by |
|---|---|---|
| 1 CLI commands | `docs/cli-command-tree.txt`, walked out of `XaftCli::command()`; a global-flag row (91–105) is checked against the clap derive in `args.rs`, through the script's `FLAG_HOMES` | `the_checked_in_cli_tree_still_matches_clap`; flags are read from the source on every run |
| 2 Slash commands | the `MATRIX_DONE` list in `crates/xaft-tui/tests/slash_parity_integration.rs` | that list *is* the proof: it parses and dispatches each command |
| 6 Built-in tools | `docs/tool-registry.txt`, snapshotted from a `ToolRegistryBuilder` registry | `the_checked_in_tool_names_still_match_the_registry` |
| 7 Configuration keys | `scripts/config-key-audit.py` field-level verdict **plus** a real reader outside `crates/xaft-config`; a `load_config` row (189–190) needs the key read by `crates/xaft-config/src/loader.rs` | `source_dir_crate_reads`, `loader_reads` |

```
python3 scripts/parity-row-reachability.py --check     # exits 1 on a false ✅
python3 scripts/parity-row-reachability.py --self-test # proves each audit can fail
```

Current state (2026-10-23, after the re-baseline): **290 ✅ rows audited, 241
proven reachable, 49 unreachable.** The 49 are all section-7 rows
(provider/request-option and execution keys such as `turn_timeout_s`,
`transport_max_retries`, `extra_headers`) that `crates/xaft-config` declares
but no other crate reads — false ✅ marks that predate this pass and are owned
by the config-wiring work. Before the re-baseline the figure was 51 of 299:
rows 44 (`max_concurrent_intents`) and 46 (`agent_pool_size`) were among the
unreachable, and agenthicc deleted both keys, so they left the audit as ➖
rather than by being wired. The same run reports what it did **not** audit —
11 🟡 and 67 ❌ rows, and the 37 ➖ rows on a line of their own — see the 🟡
note below.

Notes on what deliberately does **not** count as reachability:

* `const TOOL_NAME = "..."` grep — row 78 `mcp_connection_guide` is
  registered correctly but returns a bare literal, so the grep reports a live
  tool as missing. The built registry is used instead.
* 🟡 rows are not reachability-checked at all, because "partially
  implemented" is not a reachability claim. Commit `213e635` found ten
  dishonest 🟡 tool rows pointing at `list_files`, `create_directory` and
  friends — registered and reachable, so a reachability check passes every
  one of them. Sharing a verb is not sharing a capability, and deciding
  whether a 🟡 "genuinely implements part of" the agenthicc tool is a
  semantic judgement (the section-7 precedent is a hand field-level audit,
  not a grep). Section 6 holds **0** 🟡 rows — goals `3bb3d459` and
  `471ea5cf` downgraded all ten to ❌, which is a legitimate terminal state —
  and section 7 holds two, each a recorded partial (row 95 `browser_type`,
  row 191 the environment-override family). A checker that went quiet about a row leaving the ✅ set is
  the same false-comfort failure as the `/usage` row it exists to catch, so
  every run prints its unchecked 🟡/❌ counts and names each 🟡 by row number.
* `/config <section>` serialises the whole `XaftConfig` generically, so it
  "reads" every field. That proves a field is *displayable*, not that it is
  *used*; section 7 requires a field-specific access.
* Row 120 `project_memory_path` is read only by `src/main.rs` — the binary
  crate lives at the repo root, outside `crates/`, so the reader scan includes
  that tree. Without it the audit reported a live key as dead.

**Downgraded by this audit.** Rows **109** (`mcp_servers`) and **180**
(`plugins`) were ✅ and are now ❌: both are declared in
`crates/xaft-config` and read by nothing. Per G4c a field nothing reads is
dead config, which is worse than a missing one because it looks implemented.
Neither is implemented here — the config keys themselves belong to whichever
goal owns the MCP-client and plugin subsystems.

---

## 1. CLI commands

agenthicc's `cli/manifest.py` registers **89** built-in commands
(`BUILTIN_COMMANDS`; AGX-01 moved built-ins off the `@command`/`@group`
decorators, which now register plugins only), and
`cli/parser.py::_add_global_flags` adds **15** global flags: **104** agenthicc
entries, one row each, plus the xaft-only row 7. Every path xaft accepts is in
[cli-command-tree.txt](cli-command-tree.txt), walked out of clap. Re-derive the
agenthicc half with `python3 scripts/agenthicc-surface.py --section 1`.

| # | agenthicc command | agenthicc source | xaft location | Status |
|---|---|---|---|---|
| 1 | `config show` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Show` | ✅ |
| 2 | `config init` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Init` | ✅ |
| 3 | `config validate` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Validate` | ✅ |
| 4 | `config profiles` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Profiles` | ✅ |
| 5 | `session list` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::List` | ✅ |
| 6 | `session show` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Show` | ✅ |
| 7 | `session resume` | — (xaft-only; agenthicc resumes via `jobs resume`) | crates/xaft-cli `SessionSubcommand::Resume` | ✅ |
| 8 | `session send` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Send` | ✅ |
| 9 | `session control` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Control` | ✅ |
| 10 | `session create` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Create` | ✅ |
| 11 | `session events` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Events` | ✅ |
| 12 | `session export` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Export --service` | ✅ |
| 13 | `session serve` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Serve` | ✅ |
| 14 | `sessions list` | `cli/commands/sessions.py` | `xaft session list` | ✅ |
| 15 | `sessions show` | `cli/commands/sessions.py` | `xaft session show` | ✅ |
| 16 | `sessions export` | `cli/commands/sessions.py` | `xaft session export` (crates/xaft-cli `commands/sessions.rs`) | ✅ |
| 17 | `sessions inspect` | `cli/commands/sessions.py` | `xaft session inspect` (crates/xaft-cli `commands/sessions.rs`) | ✅ |
| 18 | `jobs list` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 19 | `jobs status` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 20 | `jobs cancel` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) — see §1b | ✅ |
| 21 | `jobs approve` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 22 | `jobs reject` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 23 | `jobs input` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 24 | `jobs labels` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 25 | `jobs rename` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 26 | `jobs retry` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 27 | `jobs resume` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) — see §1b | ✅ |
| 28 | `jobs archive` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 29 | `jobs restore` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) — see §1b | ✅ |
| 30 | `jobs delete` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 31 | `jobs purge` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 32 | `mcp list` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 33 | `mcp add` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 34 | `mcp get` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 35 | `mcp remove` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 36 | `mcp connect` | `cli/commands/mcp.py` | `xaft mcp connect <name>` — crates/xaft-cli `commands/mcp.rs` over the live client in `mcp_live/` (stdio, Streamable HTTP, HTTP+SSE, WebSocket); see §1e | ✅ |
| 37 | `mcp disconnect` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 38 | `mcp auth` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 39 | `mcp logout` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 40 | `mcp refresh` | `cli/commands/mcp.py` | `xaft mcp refresh <name>` — same handler and client; prints the catalog snapshot; see §1e | ✅ |
| 41 | `mcp doctor` | `cli/commands/mcp.py` | `xaft mcp doctor [name] [--json]` — same handler and client (the TUI's `/doctor` slash is a separate, xaft-only surface); see §1e | ✅ |
| 42 | `runs list` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 43 | `runs show` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 44 | `runs cancel` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 45 | `runs resume` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 46 | `skills add` | `cli/commands/skills.py` | `xaft skills add` — `xaft-skills` `installer.rs` + crates/xaft-cli `commands/skills.rs`; installs where `SkillLoader` reads; see §1e | ✅ |
| 47 | `trust add` | `cli/commands/trust.py` | `xaft trust add [PATH]` — crates/xaft-cli `trust.rs`, `commands/trust.rs`, consumed by the `.xaft/cli.toml` plugin loader (`cli_plugins.rs`). **Renamed in place by the 2026-10-23 re-baseline:** the row was extracted as `trust cli`, which PRD-250 S-02 replaced with `trust list` / `add` / `revoke`; `add` is the one that does what `trust cli` did, and its siblings are rows 82 and 83; see §1e | ✅ |
| 48 | `workflows list` | `cli/commands/workflows.py` | `xaft workflows list` — `xaft-config` `WorkflowRegistry` + `xaft-cli` `commands/workflows.rs` | ✅ |
| 49 | `workflows run` | `cli/commands/workflows.py` | `xaft workflows run <name> --intent` — same handler, real `RunRequest`; output divergences in §1d | ✅ |
| 50 | `worktrees list` | `cli/commands/worktrees.py` | `xaft worktrees list [--json]` — crates/xaft-cli `commands/worktrees.rs` over `worktrees/` (`ManifestStore`); see §1e | ✅ |
| 51 | `worktrees show` | `cli/commands/worktrees.py` | `xaft worktrees show <id> [--json]` — same; see §1e | ✅ |
| 52 | `worktrees integrate` | `cli/commands/worktrees.py` | `xaft worktrees integrate <id> <task>` — `ParallelCoordinator::integrate_task` + `WorktreeManager` (system `git`); see §1e | ✅ |
| 53 | `worktrees recover` | `cli/commands/worktrees.py` | `xaft worktrees recover <id>` — `ParallelCoordinator::recover` + `git worktree list --porcelain`; see §1e | ✅ |
| 54 | `login` | `cli/commands/auth.py` | crates/xaft-tui slash `/login` (no CLI subcommand) | 🟡 |
| 55 | `logout` | `cli/commands/auth.py` | crates/xaft-tui slash `/logout` (no CLI subcommand) | 🟡 |
| 56 | `whoami` | `cli/commands/auth.py` | — | ❌ |
| 57 | `init` | `cli/commands/init.py` | `xaft init [--force] [--write]` — crates/xaft-cli `commands/init.rs` + `project_init.rs`; see §1e | ✅ |
| 58 | `run` | `cli/commands/background.py` | crates/xaft-cli `Commands::Run` — **re-judged by the 2026-10-23 re-baseline:** agenthicc's `run` is the background-session submitter (`run --background --intent … [--workflow] [--priority] [--json]`; without `--background` it only prints a usage hint), while `xaft run` executes a task in the foreground — what agenthicc does with the bare entry point and its flags (rows 95, 100, 101). xaft has no background supervisor to submit to (§1e) | 🟡 |
| 59 | `agents` | `cli/commands/background.py` | `xaft agents [--run RUN] [--json]` — crates/xaft-cli `commands/agents.rs` + the interactive `background_manager.rs` over the `jobs`/`runs`/orchestration stores; see §1e | ✅ |
| 60 | `attach` | `cli/commands/runs.py` | `xaft attach [SESSION_ID] [--run RUN] [--json]` — crates/xaft-cli `commands/attach.rs` (`JobStore::attach_foreground` + the TUI); see §1e | ✅ |
| 61 | `jobs` | `cli/commands/background.py` | the manager it opens is `xaft agents` (row 59), but bare `xaft jobs` is clap's "requires a subcommand" usage error, because `JobsCommand::subcommand` is not optional; agenthicc registers `jobs` and `agents` as two names for one handler | 🟡 |
| 62 | `jobs priority` | `cli/commands/background.py` | — agenthicc sets a background session's queue priority (`urgent` / `normal` / `low`) through the supervisor's durable fair-share queue (PRD-225) and records an audit event | ❌ |
| 63 | `jobs inbox list` | `cli/commands/background.py` | — lists pending background approvals, questions and input requests from the durable operator inbox (PRD-227), filterable by project, type, age and status, without exposing submitted answers | ❌ |
| 64 | `jobs inbox answer` | `cli/commands/background.py` | — answers one pending question or input request by exact item id, guarded by an expected version and an idempotency key | ❌ |
| 65 | `jobs inbox approve` | `cli/commands/background.py` | — approves the exact pending tool request, optionally remembering the decision or granting a scope | ❌ |
| 66 | `jobs inbox reject` | `cli/commands/background.py` | — rejects the exact pending tool request, with an optional message | ❌ |
| 67 | `jobs inbox expire` | `cli/commands/background.py` | — expires one pending interaction | ❌ |
| 68 | `jobs notifications` | `cli/commands/background.py` | — inspects delivery of background-run notifications: local notices and signed HTTPS webhooks from a SQLite outbox (PRD-226) | ❌ |
| 69 | `runs` | `cli/commands/runs.py` | `xaft runs` — the bare form of `runs list` (`RunsCommand::resolve`), pinned by `bare_and_explicit_runs_forms_both_exist` | ✅ |
| 70 | `schedules` | `cli/commands/schedules.py` | — agenthicc's repository-scoped scheduled workflows (PRD-228); the bare form lists a repository's schedules | ❌ |
| 71 | `schedules list` | `cli/commands/schedules.py` | — lists the schedules for a repository | ❌ |
| 72 | `schedules create` | `cli/commands/schedules.py` | — creates a schedule (workflow, recurrence, coalesce policy), disabled until resumed | ❌ |
| 73 | `schedules show` | `cli/commands/schedules.py` | — inspects one schedule and its recent run history | ❌ |
| 74 | `schedules pause` | `cli/commands/schedules.py` | — pauses future occurrences | ❌ |
| 75 | `schedules resume` | `cli/commands/schedules.py` | — resumes once the user confirms the next execution time | ❌ |
| 76 | `schedules run-now` | `cli/commands/schedules.py` | — queues an auditable immediate occurrence | ❌ |
| 77 | `schedules retry` | `cli/commands/schedules.py` | — retries a failed occurrence as a new auditable attempt | ❌ |
| 78 | `schedules update` | `cli/commands/schedules.py` | — revises a schedule after explicit confirmation | ❌ |
| 79 | `schedules delete` | `cli/commands/schedules.py` | — stops future occurrences and tombstones the definition | ❌ |
| 80 | `schedules service-status` | `cli/commands/schedules.py` | — reports whether schedule execution is armed (the scheduler service lease) | ❌ |
| 81 | `schedules history` | `cli/commands/schedules.py` | — shows bounded occurrence history | ❌ |
| 82 | `trust list` | `cli/commands/trust.py` | `xaft trust list` — the same store and loader as row 47; see §1e | ✅ |
| 83 | `trust revoke` | `cli/commands/trust.py` | `xaft trust revoke PATH` — the same store and loader as row 47; see §1e | ✅ |
| 84 | `worktrees dispatch` | `cli/commands/worktrees.py` | — dispatches one orchestration task, approving its environment setup for this run (PRD-203, PRD-229). xaft's `ParallelCoordinator::dispatch` exists (§1e), but no command reaches it | ❌ |
| 85 | `worktrees actions` | `cli/commands/worktrees.py` | — lists the named actions a repository's environment manifest (`.agenthicc/environment.toml`) declares for one worker worktree (PRD-229) | ❌ |
| 86 | `worktrees action` | `cli/commands/worktrees.py` | — runs one approved named worker action | ❌ |
| 87 | `worktrees setup` | `cli/commands/worktrees.py` | — runs trusted or explicitly approved worker setup commands | ❌ |
| 88 | `worktrees trust` | `cli/commands/worktrees.py` | — trusts the current environment-manifest digest | ❌ |
| 89 | `agents actions` | `cli/commands/worktrees.py` | — `worktrees actions` through the agents surface: the actions declared for one worker session | ❌ |
| 90 | `agents action` | `cli/commands/worktrees.py` | — runs one explicitly approved action for one worker session | ❌ |

**Global flags.** agenthicc registers these on its top-level parser, so they
apply to the bare TUI invocation and to every command. xaft's live on `xaft
run` (whose task-less form opens the TUI), so a ✅ here is checked against the
clap derive through `FLAG_HOMES` in `scripts/parity-row-reachability.py`, not
against the command tree (§0).

| # | agenthicc flag | agenthicc source | xaft location | Status |
|---|---|---|---|---|
| 91 | `--goal` | `cli/parser.py` | — selects the `goal_flow` workflow and submits GOAL as the session's first intent, recorded as a durable goal run (PRD-204); xaft has no `goal_flow` (§8 row 6), and nothing writes its `runs` store | ❌ |
| 92 | `--detach` | `cli/parser.py` | — returns once the background supervisor has durably accepted a `--goal` run, printing how to track (`agents --run`) and attach to it (PRD-205) | ❌ |
| 93 | `--priority` | `cli/parser.py` | — queue priority (`urgent` / `normal` / `low`) for a detached `--goal` run (PRD-225) | ❌ |
| 94 | `--json` | `cli/parser.py` | — machine-readable output for a goal start or control operation, so it only means something with `--goal`. `xaft run --json` is a different capability: NDJSON run events, implying `--headless` | ❌ |
| 95 | `--headless` | `cli/parser.py` | `xaft run --headless` — without a task it speaks agenthicc's headless stdin protocol, one intent per line in and JSON lines out (`commands/headless_stream.rs`) | ✅ |
| 96 | `--workflow` | `cli/parser.py` | `xaft run --workflow NAME` — headless, NAME runs for each stdin line; with a task the run uses NAME; task-less it starts the TUI with NAME selected for every message, like `/workflow NAME` (`commands/run.rs::start_workflow` → `TuiApp::with_initial_workflow`). An unknown name is a usage error | ✅ |
| 97 | `--mode` | `cli/parser.py` | `xaft run --mode MODE` — agenthicc's names and aliases, validated before anything starts (an unknown mode is a usage error listing the modes); the TUI starts in MODE (status line, command-line task, every later message) and a headless run — each stdin-protocol line too — runs under its prompt patch, tool filter and approval filter (`commands/run.rs::apply_mode`, `TuiApp::with_initial_mode`). Recording it on a detached run belongs to `--detach` (row 92) | ✅ |
| 98 | `--config` | `cli/parser.py` | `xaft run --config PATH` (task-less into the TUI too: `InteractiveStart` carries the loaded config), `xaft workflows run\|create\|resume -c PATH`, `xaft config show` / `validate` / `profiles --config PATH`, and `XAFT_CONFIG` (row 190) for every command | ✅ |
| 99 | `--version` | `cli/parser.py` | `xaft --version` (clap's `version` attribute on `XaftCli`), and `xaft version` | ✅ |
| 100 | `--continue` | `cli/parser.py` | `xaft run --continue` — resumes the directory's latest session, task-less into the TUI or with a task (`resolve_resume_id`) | ✅ |
| 101 | `--resume` | `cli/parser.py` | `xaft run --resume ID` — the same path with an explicit session id | ✅ |
| 102 | `--record-cassette` | `cli/parser.py` | — records every LLM call and approval of the session to `DIR/<session-id>/` (default `~/.agenthicc/cassettes`) for deterministic replay (`testing/cassette.py`) | ❌ |
| 103 | `--set` | `cli/parser.py` | `xaft run --set section.key=value` — repeatable, TOML-parsed, unknown keys rejected (commit `2428e19`), and since `5235b8c` carried into a task-less TUI session too; but only on `xaft run`, where agenthicc's is global and applies to every command | 🟡 |
| 104 | `--set-secret` | `cli/parser.py` | `xaft run --set-secret section.key=ENV_VAR` — the same reach, and the same remaining gap, as row 103 | 🟡 |
| 105 | `--dangerously-skip-permissions` | `cli/parser.py` | `xaft run --dangerously-skip-permissions` — the TUI (task-less too) swaps in `AutoApproveGate` after a danger confirmation; a headless run — `--headless`, `--json`, no terminal, `xaft workflows run\|create\|resume` (which take the flag and `-y`) — otherwise fails closed like agenthicc's `_HeadlessApprovalService` (`mode_gate::HeadlessApprovalGate`, `XaftRuntime::effective_approval_gate`) | ✅ |

**Subtotal: 105 rows.** ❌ 32 · 🟡 6 · ✅ 67

### 1b. `jobs` rows that were ✅ and were not (found while raising coverage — now closed)

Writing the missing handler tests for `xaft jobs` — the store model was well
covered but `handle_jobs` itself had **zero** tests, so 132 lines of
user-visible code had never been executed — turned up three rows marked ✅ on
the strength of a working store and an unreachable handler. **All three are
now fixed and back to ✅**; the section is kept rather than deleted because the
shape of the defect, not just the three rows, is the lesson.

| Row | Command | Was ❌ because | Fixed by |
|---|---|---|---|
| 20 | `jobs cancel` | `JobStore::cancel` implements the two-step Active → Cancelling → Cancelled path, with a comment saying "so `jobs cancel` succeeds from any active state, which is what users expect". **`handle_jobs` never called it.** It called `mutate(..., JobStatus::Cancelled)`, i.e. a raw one-hop `transition`, which only succeeds from `Cancelling`. Cancelling a **running** job — the single most common thing a user does — always failed. | `handle_jobs` routes through `store.cancel()`. `agenthicc`'s `BackgroundSupervisor.cancel` (`background/supervisor.py:574`) drives the same two hops and returns `cancelled`/`archived` unchanged, which is what `JobStore::cancel` already mirrored. |
| 27 | `jobs resume` | `handle_jobs` resumed by transitioning to `JobStatus::Queued`, but `can_transition_to` **never lists `Queued` as a target for any source state**, and `transition` short-circuits only when the job is *already* `Queued`. Every case the subcommand's doc comment promises — "Resume a cancelled, failed, or orphaned background session" — failed. | A new `JobStore::resume` picks the target the way agenthicc's `BackgroundSupervisor.resume`/`handoff` (`background/supervisor.py:455`) do: `failed → retrying`, and `cancelled`/`orphaned`/`archived` → `starting`. Both edges were **already** in the table; no new transition was invented. `queued` is deliberately *not* a target — in agenthicc too, a session only enters `queued` when it is created. |
| 29 | `jobs restore` | `JobStore::restore` remembered the pre-delete state in an in-process `restore_targets` map, then filtered it through `t.can_transition_to(*t)`. `can_transition_to` forbids **every** self-transition (no arm lists its own state as a target), so the filter always rejected. Two independent defects: the filter was wrong, *and* the map was unreachable anyway, because `jobs delete` and `jobs restore` are separate CLI invocations and a fresh `JobStore` never had anything in it. Every restored job silently became `Queued`. | The pre-trash state moved onto the job as `Job::pre_delete_status` (`#[serde(default)]`, so old snapshots still load), and `restore` honours it. A job with nothing recorded falls back to `queued`, which is runnable rather than frozen. |

**The tests that pinned the bugs are inverted, not supplemented.**
`crates/xaft-cli/tests/jobs_handlers.rs` previously asserted `TASK_FAILED` for
cancel-of-a-running and resume-of-a-cancelled, and asserted that restore landed
on `Queued` — each with a comment saying it must be inverted when fixed. Those
three are now the positive assertions (`cancel_of_a_running_job_succeeds_via_the_two_step_path`,
`resume_admits_the_four_states_agenthicc_admits`,
`delete_then_restore_round_trips_through_the_store`), so the correct behaviour
is what is pinned.

Two guards were added alongside the inversions, because both fixes are silent
failures otherwise: `resume_of_an_active_or_terminal_job_is_refused_and_leaves_it_alone`
(the old code rejected *everything* by accident, so the rejection path needed to
become explicit rather than disappear), and
`a_snapshot_predating_the_field_still_loads_and_restores_to_queued` (without
`#[serde(default)]`, one stale `jobs.json` would make every `xaft jobs`
invocation fail to open the store).

**The generalisable shape is the one goal `d30842d57` was created for:** a row
marked done on the strength of a capability existing *somewhere*, while the
reachable surface a user actually touches is empty. Here the capability was one
function call away in the same module. `handle_jobs` had **zero** tests, so
every exit code the `jobs` family produced was unverified — the coverage gap
and the false-parity gap were the same gap.

### 1c. Open CLI rows and their owner (measured, not re-derived)

Section 1 has **105 rows: 63 done, 42 open** (33 absent, 9 partial). The 63
done rows need no owner. This table accounts for all 42 open rows, so no row in
section 1 lacks an owner. (The twelve rows this table used to carry for the
`mcp` live, `skills add`, `trust`, `worktrees`, `init`, `agents` and `attach`
families were closed by the `parity/cli-family` branch once the operator
authorized every agenthicc subsystem; §1e records how each was closed.)
`scripts/cli-row-ownership.py --check` enforces both halves mechanically: a
claimed row that is ✅ (or ➖) is an error, and an open row with no claim is an
error.

**39 of the 42 were opened by the 2026-10-23 re-baseline** — row 58 re-judged,
and 38 of the 45 new rows 61–105. The operator's 2026-10-23 authorization of
every agenthicc subsystem (§Summary) covers all of them, but no branch has taken
any yet. A goal label would point at work nobody owns, which is the stale-owner
defect this table exists to prevent, so their owner says exactly that —
`authorized, unclaimed` — and `cli-row-ownership.py` lists those rows by number
on every run instead of letting them pass as owned.

Sizes below are **measured line counts of the agenthicc source each row
actually needs**, not of the thin CLI wrapper that exposes it. That distinction
is the whole reason a row is or is not a small increment: several rows are one
clap variant away in the wrapper and hundreds of lines away in the subsystem
behind it.

| Rows | Family | Why this is not a small increment | Owner goal |
|---|---|---|---|
| 54, 55, 56 | `login` / `logout` / `whoami` | a real OAuth 2.0 PKCE browser flow (`auth.py`, 328 lines) against `https://agenthicc.ai/oauth/*`. **Re-measured:** there is no such endpoint in xaft to talk to — `grep -rniE 'agenthicc\.ai|xaft\.(ai|dev|com)/' crates/*/src` returns 0 hits, and the only literal URLs in xaft's own crates are `https://api.anthropic.com` and `https://api.openai.com/v1` (plus `http://localhost:11434/v1` in an `xaft config add-provider` example — a local model server, not an account service). `login`/`logout` are only 🟡 because slash stubs exist. **Cost is lower than first recorded:** `reqwest`+TLS is already in `Cargo.lock` and already built (via `agtrs-openai`/`agtrs-anthropic`) and `tokio` has `features = ["full"]`, so the token exchange and the loopback callback need no new crate — only `keyring` would be. The blocker is the **account model**, not the plumbing. | **declined (no authorization)** — no account model; see `g4-decomposition.md` |
| 58, 62 | `run` (background submission), `jobs priority` | agenthicc's `run` is `supervisor.submit(...)`: a durable background session run by a detached worker process and queued with a priority (PRD-141, PRD-225). The two wrappers are about 50 and 10 lines of `cli/commands/background.py`; the subsystem behind them is **6,767 lines** (`background/supervisor.py` 2334, `worker.py` 2708, `store.py` 1568, `queue.py` 157). xaft's `jobs` store is complete but has **no runtime writer** (§1e), so this is the subsystem every other background row waits on. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it |
| 61 | bare `jobs` | **small:** make `JobsCommand::subcommand` optional and route the bare form to the `xaft agents` manager, the way `RunsCommand::resolve` routes bare `runs` to `runs list`. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it |
| 63–67 | `jobs inbox list` / `answer` / `approve` / `reject` / `expire` | the durable operator inbox (PRD-227): `background/inbox.py`, **1,012 lines**, plus `input_inbox.py`, 755, with exact-version and idempotency guards. Its items are written by background sessions waiting on an approval or an answer, so it needs rows 58/62's supervisor first; slash row 29 (`/inbox`) is its TUI face. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it; needs rows 58, 62 first |
| 68 | `jobs notifications` | `background/notifications.py`, **922 lines**: a SQLite outbox projected from the session event journal, delivering local notices and signed HTTPS webhooks configured by `[background.notifications]` (a table outside section 7's extraction rule; see the re-baseline note). It reports on supervisor lifecycle transitions. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it; needs rows 58, 62 first |
| 70–81 | `schedules`, bare and its 11 subcommands | repository-scoped scheduled workflows (PRD-228): `schedules/` is **2,256 lines** (store 936, scheduler 470, manager 283, model 281, recurrence 226) behind a 397-line CLI, and every occurrence runs as a background session. Slash rows 27 and 28 (`/schedules`, `/schedule`) are the same subsystem. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it; needs rows 58, 62 first |
| 84–90 | `worktrees dispatch` / `actions` / `action` / `setup` / `trust`, `agents actions` / `action` | reproducible worktree environments and named actions (PRD-229): `worktrees/environment.py`, **1,074 lines**, plus `environment_actions.py`, 171 — a digest-trusted `.agenthicc/environment.toml`, approved setup commands and named actions per worker worktree. `dispatch` also needs a worker to dispatch to; xaft's `ParallelCoordinator::dispatch` exists with no command over it. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it |
| 91–94 | `--goal`, `--detach`, `--priority`, `--json` | the goal runner (PRD-204, PRD-205, PRD-212): `runs/` is **1,729 lines** (manager 653, model 561, cli 261, store 232), and `--goal` runs `goal_flow`, which xaft does not have (§8 row 6). `--detach` and `--priority` hand the run to rows 58/62's supervisor; `--json` only shapes this entry point's output. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it; needs §8 row 6 and rows 58, 62 |
| 103, 104 | `--set`, `--set-secret` | **small:** both work on `xaft run` (task-less too, since `5235b8c`), but agenthicc parses them as global options before the subcommand, so every command that loads configuration honours them. xaft would need a global clap argument threaded into every `ConfigLoader::load` call site. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it |
| 102 | `--record-cassette` | session recording: `testing/recording_transport.py` (275 lines), `recording_approval.py` (138) and `cassette.py` (392) wrap the provider transport and the approval service and write a replayable JSONL record per session. xaft has no recorder. | **authorized, unclaimed** — the operator's 2026-10-23 authorization covers it; no branch has taken it |

**Row 46 was declined for want of authorization; it is now implemented.** The
earlier notes recorded three costs — an HTTP fetch, a Git clone and, decisively,
the trust boundary of writing into the directories `SkillLoader` feeds into the
system prompt. With the operator's authorization each was answered rather than
avoided:

- **HTTP**: `reqwest` 0.12 is now a direct dependency of `xaft-skills`, declared
  with the feature set the provider crates already compile, so no second copy is
  built. The fetch sits behind a `SkillFetcher` trait so the whole install
  pipeline is tested offline.
- **Git**: a shell-out to the system `git`, as agenthicc does — `clone --depth 1
  --no-tags` with prompts disabled, `protocol.ext` forbidden and only `https`
  allowed, under a 300 s ceiling.
- **The trust boundary**: the install target *is* the prompt path, by
  construction — `SkillLoader::project_skills_dir` / `global_skills_dir` are the
  one definition both the loader and the installer use, and every candidate is
  validated with the loader's own `parse_skill_text` before it is renamed into
  place. The trust decision is the user's explicit `xaft skills add` invocation,
  exactly as in agenthicc, and it is guarded the way agenthicc guards it: HTTPS
  only, 1 MiB and UTF-8 caps, symlinks refused, existing skills never
  overwritten, all-or-nothing collision checks for repository installs.

The loader gained directory skills (`<dir>/SKILL.md`), the layout agenthicc and
the wider agent-skills ecosystem install, so an installed skill's supporting
files travel with it. Config row 159 (`install_default_skills`) remains ❌: it
seeds bundled skills at startup, which is a separate capability from this
command.

**Rows 54, 55 and 56 are declined, and the decisive reason is not the one
originally recorded.** The earlier note blamed "no keyring crate"; that is
true but incidental — `reqwest` with TLS is already a compiled dependency
(via `agtrs-openai`/`agtrs-anthropic`) and `tokio` is already `features =
["full"]`, so the OAuth token exchange and a loopback callback server would
need no new crate beyond `keyring`. The real blocker is that **xaft has no
account and no auth server to authenticate against**: `auth.py` hardcodes
`AGENTHICC_AUTH_URL = "https://agenthicc.ai/oauth/authorize"`, and xaft's only
`base_url`s are third-party model providers (anthropic, openai, ollama).
There is nothing for `login` to log into. Row 56 is not a partial case either:
the plan assumed "xaft can already print local identity", but xaft has no
identity concept at all — `xaft version` prints the package version and
nothing else — so agenthicc's `{email}  plan={plan}  token_expires={exp}` is
unproducible, and substituting a different document under the same name would
be false parity. The operator was asked to choose (authorize the subsystem /
`whoami`-only / decline) and **did not answer within the deadline**, so this
is `declined (no authorization)` — a recorded outcome, not a judgement that
the feature is unwanted. No stub was shipped: a `login` that reports success
without a token is worse than an absent one. **No parity row status changes
as a result** — 54/55 stay 🟡 because the slash stubs genuinely exist, and 56
stays ❌.

**The per-family sizes, baselines and increment order live in
[`g4-decomposition.md`](g4-decomposition.md).** That document is the analysis;
this table is the index into it. Where the two disagree, this table is the
current one. The count has moved twice, and the two moves are a *different
kind* of change — which is why the history is recorded rather than just the
result:

* **`23 → 21`** when rows 48 and 49 (`workflows list` / `workflows run`)
  landed. Rows genuinely closed.
* **`21 → 24 → 21`** across §1b. Rows 20, 27 and 29 were *counted as open*
  because they were ✅ in the table but ❌ in the binary — a **false ✅**, not
  a real gap. They have now been genuinely fixed (§1b) rather than
  reclassified, so the honest figure returns to 21.

**The return to 21 is not churn, and both documents say so.** The middle
step was a counting correction, not new work, and the count went back down
because three rows were *fixed*, not because rows were reclassified away. The
same history is recorded in
[`g4-decomposition.md`](g4-decomposition.md#baseline-correction), which quotes
the same live figure and carries the same warning.

**Both documents now state 21, and `g4-decomposition.md` carries the §1c
pointer in return.** An earlier version of this paragraph read "the `24`
currently recorded in `g4-decomposition.md` is stale by exactly three rows and
should read `21`" — that was true when written and is now a stale instruction
itself, because commit `512549b` propagated the correction. It is replaced here
rather than left in place for the same reason: a note telling the next pass to
fix a number that is already correct sends it to re-derive a baseline that
nothing is wrong with, which is precisely the defect §1b was created to end, one
document over.

The authority is the script, not either document's prose:

```
$ python3 scripts/cli-row-ownership.py --check
open CLI rows: 42   owned: 42   removed upstream (➖, need no owner): 0
```

It parses section 1's own rows with the same parser as
`scripts/parity-matrix-stats.py` and fails if the §1c table drifts from them in
either direction, so neither document's number can be the last word. Re-run it
rather than quoting this block.

### 1d. Output-shape divergences recorded for ✅ rows

A ✅ row asserts the capability is reachable, not that every byte matches. This
table records where a ✅ row's output deliberately differs from agenthicc's,
so a later pass does not "fix" the difference by inventing a value.

| Row | agenthicc prints | xaft prints | Why | Pinned by |
| --- | --- | --- | --- | --- |
| `workflows run` (row 49) | `Workflow <name> <status> (session <id>, run <run-id>)` | **phased workflows: identical.** Fixed orchestrators (`standard` / `dynamic` / `meta`): `Workflow <name> <exit-code> (session <id>)` | A phased run now has a durable identity — its checkpoint `.xaft/workflow-runs/<run-id>.json`, written by `xaft_runtime::phased` — so `workflows run` prints agenthicc's exact line, with `status` from the checkpoint (`complete`, `exited`, `failed`, …). The three fixed orchestrators keep no checkpoint, so for them the session id is printed once and never under a `run` label. | `a_phased_run_prints_agenthiccs_line_with_a_real_run_id`, `the_outcome_line_for_a_fixed_orchestrator_names_the_session_once`, `a_failed_run_reports_its_own_exit_code` (`crates/xaft-cli/src/commands/workflows.rs`) |
| `workflows run` (row 49) | `Phases: a → b` and `Error: ...` | the same two lines for phased runs, plus `Summary:` and, on failure, `Resume with: xaft workflows resume <run-id>` | additive: the phase list and error now come from the run's checkpoint | same |
| `workflows list` (row 48) | `source` one of `builtin` / `user` / `project` | the same three labels | xaft now discovers workflow files in `~/.config/xaft/workflows/` (`user`) and `.xaft/workflows/` (`project`); a `[workflows.<name>]` table also reads `project` | `project_files_shadow_user_files_and_builtins` (`crates/xaft-config/src/workflows/mod.rs`), `list_reports_a_custom_workflows_source_as_project` |
| `mcp list` (row 32) | `<invalid>: transport=unknown enabled=True` | `<invalid>: transport=unknown enabled=true` | The only byte difference is Python's `True` against Rust's `bool`, and it applies to every `enabled=` xaft prints. The `<invalid>` placeholder itself is agenthicc's, not xaft's: `_redacted_servers` emits `raw.get("name", "<invalid>")` for a stanza it cannot build, and `mcp_list` then formats it with `item.get('transport', 'unknown')` / `item.get('enabled', True)`. | `a_nameless_stanza_is_invalid_and_keeps_its_neighbour`, `a_explicitly_empty_name_is_still_a_valid_entry`, `a_non_string_name_is_invalid` (`crates/xaft-cli/tests/mcp.rs`), `list_stdout_reports_a_nameless_stanza_as_invalid_and_keeps_the_valid_one`, `list_json_stdout_marks_the_nameless_stanza_invalid` (`crates/xaft-cli/tests/mcp_handlers.rs`) |

**Row 32 also fixes where the invalid-entry decision is made, and that placement
is load-bearing.** `McpClientConfig` (`crates/xaft-config/src/types.rs`) must
stay tolerant of a partially-specified `[[mcp.client]]` stanza: `name` defaults
to `""` and `transport` to `"stdio"`. The reason is that `src/main.rs` calls
`ConfigLoader::load` *before dispatching any command*, so a stanza missing a
required field rejects the whole file and every command exits 3 — `xaft
version` included. That is the exact shape of the defect this row's contract
was unreachable through: `read_mcp_servers` has always degraded a bad stanza to
`McpEntry::Invalid`, and `list` has always printed it, but neither could run
because the process could not start.

So the classification happens per stanza in the lenient reader
(`xaft_cli::mcp::parse_servers` reads `name` off the raw TOML table, because the
serde default means the typed parse can no longer tell "no name" from
`name = ""`), and the typed loader's only job is to let the process start. Do
not "harden" `McpClientConfig` by making either field mandatory again: that
reinstates the whole-file rejection and turns row 32 back into a contract the
binary cannot honour.

The loader side is guarded by tests that go through the real load path, not the
`--config` seam — `mcp_client_name_defaults_to_empty_via_project_config_discovery`
and `mcp_client_transport_defaults_to_stdio_via_project_config_discovery` in
`crates/xaft-config/tests/config_integration.rs` drive `ConfigLoader::load` with
`project_dir` pointed at a directory holding `.xaft/xaft.toml`. Every test in
`crates/xaft-cli/tests/mcp_handlers.rs` passes `--config`, which reaches
`read_mcp_servers` directly and never touches the typed loader, so a loader
regression is invisible from that file by construction.

**That is a standing false-green hazard, so the two readers are now pinned side
by side.** Goals 67 and 68 closed the two *omission* rows of the asymmetry
(`transport`, then `name`, both since serde-defaulted), but a *type mismatch*
still splits them: a stanza with `enabled = "yes"` deserialises to nothing on
the strict side — `ConfigLoader::load` errors and `main` exits 3 before
dispatch — while `read_mcp_servers` degrades that one stanza to
`McpEntry::Invalid` and exits 0. So a green run of `mcp_handlers.rs` says
nothing about whether `xaft version` can start against the same file.
`the_lenient_seam_stays_green_while_the_startup_loader_rejects_the_same_file`
(`crates/xaft-cli/tests/mcp_handlers.rs`) writes one file and asserts both
verdicts, and `crates/xaft-cli/src/mcp.rs` now carries the table of the rows
that still differ. If that test ever fails on its loader assertion the readers
have been made to agree and the hazard has narrowed — the moment to revisit the
note, not to delete it.

**Rows 32 and 34: `env_headers` values are masked at RENDER time, and the file is
deliberately NOT rejected.** `McpClientConfig::redacted()` copies `env_headers`
into its output on the stated basis that an entry maps a header to an
environment-variable *name* and therefore "reveals nothing". That basis was true
for agenthicc and was simply not enforced in xaft, so a hand-edited
`env_headers = { X-Api-Key = "sk-…" }` was accepted and printed verbatim by both
`mcp get` and `mcp list --json` (measured on `target/debug/xaft`: the planted
secret appeared in all three printing paths).

The choice made, recorded here because the two options differ observably:

* **Chosen — render-time masking.** A value that is not `^[A-Z_][A-Z0-9_]*$` is
  replaced with `<redacted: not an environment variable name>`; a conforming
  value is printed unchanged, and header *names* always survive. So `mcp get`
  still shows which variable backs which header — the information the command
  exists to provide — while a pasted credential cannot reach stdout.
* **Rejected — loader-time rejection.** It would be a *divergence* from
  agenthicc, not a port, and by the mechanism the two paragraphs above describe
  it would exit 3 for **every** command including `xaft version`, because
  `ConfigLoader::load` runs before dispatch. It would also contradict
  `mcp add --token-env`, which already rejects a bad name on the write path.

**The measurement that corrected the premise:** agenthicc's own guard is *not*
load-time. `McpServerConfig.validate()` rejects the same pattern, but it is
called only from `McpSessionManager` (`tools/mcp_manager.py:328` and `:850`) —
i.e. when a server is connected. `mcp list`/`mcp get` build the config with
`from_dict()`, which never validates, so agenthicc's listing path has the same
hole. xaft is therefore *stricter than agenthicc here*, and that divergence is
intentional.

The rule has exactly one implementation, `xaft_config::is_env_var_name`, which
`xaft_cli::mcp`'s private `valid_env_name` now delegates to, so the name
`mcp add --token-env` accepts and the name the redactor shows cannot drift
apart (`mcp_add_and_the_redactor_agree_on_what_an_env_var_name_is` asserts this
sample-for-sample through the public write path). The enforcement is load-bearing
rather than always-green: removing the `is_env_var_name` branch turns
`get_never_prints_a_planted_env_header_value`,
`get_json_never_prints_a_planted_env_header_value`,
`list_json_never_prints_a_planted_env_header_value`,
`a_bad_env_header_value_is_rendered_not_rejected_by_the_startup_loader` and two
unit tests red. The last of those also pins the loader-side half — the file is
accepted, and the value does not survive printing — so the decision cannot be
reversed silently by someone who reads only one of the two paths.

**CLOSED — the `config show` leak in this family.** `xaft config show --format
json|toml` used to serialise the whole `XaftConfig`, so it printed
`provider.<name>.api_key`, `provider.<name>.headers` values, `mcp.client[].token`,
`tools.mcp_servers[].token` and any literal pasted into `env_headers` verbatim —
measured on `target/debug/xaft`, where four planted values appeared in **both**
the `json` and the `toml` output. agenthicc never had the defect: its `config
show` prints `cfg.redacted_dict()`. Both formats now serialise
`XaftConfig::redacted()`, the xaft equivalent of that method.

The policy is reconciled with agenthicc's, not the layout: the walk reuses
`key_is_sensitive`, which is the same test `redact_value` applies to provider
option tables, so this family has **one** definition of a secret-shaped key
rather than two that can drift. Four exceptions, each pinned in
`crates/xaft-config/src/redact.rs`, because the key rule alone is wrong for
them: an `env_headers` *value* survives only if it is an
environment-variable name (`X-Api-Key` contains no `api_key` substring, so the
key rule would miss `X-Api-Key = "sk-…"` entirely); `headers` /
`default_headers` / `extra_headers` keep their names and mask every value;
`api_key_env` / `token_env` name the secret and are kept; and a **non-string**
under a sensitive key is never masked, because `guardrail.secret_leakage` is a
bool whose key contains `secret` and masking it would replace a real switch with
`"<redacted>"`. TOML has no null, so that arm also drops the unset members the
JSON arm keeps as `null` (`"url": null`) — without it the serializer errors and
the handler's `unwrap_or_default()` would print nothing, silently turning
`--format toml` into a no-op.

End-to-end on `target/debug/xaft`: five planted credentials, absent from both
formats; `ANTHROPIC_KEY_VAR` and `MCP_TOKEN` still shown; five values masked.
The same defect class **remains** in the TUI's `/config` slash command
(`crates/xaft-tui/src/slash/commands/config.rs` serialises `&*self.config`
directly), which is a different surface and is tracked separately.

**Rejected alternatives for row 49's `run` field**, recorded so the choice is
not re-litigated: minting a uuid per run would print a real-looking id that
nothing can look up — the same "carries no information" defect in a prettier
form, and dead config by the G4c rule; printing agenthicc's literal `unknown`
preserves the exact string shape but leaves a permanently-empty field that
reads as a bug on every invocation. The session id is the durable handle
(`xaft session show <id>` and `xaft sessions inspect <id>` both resolve it), so
it is printed once.


### 1e. Rows closed on `parity/cli-family` (measured against agenthicc HEAD `336d4bd`)

Twelve rows — 36, 40, 41, 46, 47, 50–53, 57, 59, 60 — were open for want of a
subsystem or of authorization. The operator has since authorized every
agenthicc feature, so each family was ported with the subsystem behind it rather
than as a wrapper. Every command below is a path in `docs/cli-command-tree.txt`
and is driven end to end by handler tests that need no network.

| Family | Rows | What was built | Pinned by |
| --- | --- | --- | --- |
| `mcp connect` / `refresh` / `doctor` | 36, 40, 41 | `mcp_live/`: a port of `McpSessionManager` (typed status record with agenthicc's ten keys and twelve states, explicit start with retries inside a 10 s startup deadline, `needs_auth` without a connection attempt when a credential variable is unset, catalog snapshots with agenthicc's canonical SHA-256 and revision rule, `doctor`) over stdio, Streamable HTTP (session id and protocol-version headers), legacy HTTP+SSE (origin-checked endpoint) and WebSocket (`mcp` subprotocol) transports. Output is rendered through `pyjson`, byte-identical to Python's `json.dumps`. | `crates/xaft-cli/tests/mcp_live.rs` (real sh stdio server, in-process HTTP/SSE/WS servers), `mcp_live` unit tests |
| `skills add` | 46 | see the closure note in §1c | `crates/xaft-skills/src/installer.rs` tests, `crates/xaft-cli/tests/skills_add.rs` |
| `trust` | 47, 82, 83 | `trust.rs` (PRD-250's digest-bound `ExtensionTrust`: `$XAFT_HOME/trust.json` in agenthicc's wire format, trusted-by-location user dirs, edit-revokes) and `cli_plugins.rs` (agenthicc's `cli.toml` command shorthand), so the trust store has a real consumer: an untrusted `.xaft/cli.toml` is reported and its commands do not exist. | `crates/xaft-cli/tests/trust.rs`; `tests/cli_plugins.rs` spawns the real binary |
| `worktrees list` / `show` / `integrate` / `recover` | 50–53 | the uncompiled WIP in `worktrees/` finished and wired: `WorktreeManager` (system `git`, bounded timeouts) and `ParallelCoordinator` (create, add_task, ready_tasks, dispatch, inspect_task, integrate_task, rebase_task, cleanup_task, recover), plus agenthicc's missing `queued` task status and `environment_receipts` field. | `crates/xaft-cli/tests/worktrees.rs` and the `worktrees::{manager,coordinator}` unit tests, all against real temporary Git repositories |
| `init` | 57 | `project_init.rs`: `AGENTS.md` plus an inert, all-comment `.xaft/xaft.toml` derived from `XaftConfig::default()`; symlink and non-file refusals; atomic, mode-preserving writes. | `crates/xaft-cli/tests/init.rs` (including that the template does not change the effective config) |
| `agents`, `attach` | 59, 60 | `commands/agents.rs` (`--json`, `--run RUN [--json]` joining runs, jobs and orchestrations into agenthicc's `RunAgentRecord` shape), `background_manager.rs` (the interactive manager: select, attach, cancel, resume, archive, delete; 1 s refresh) and `commands/attach.rs` (`JobStore::attach_foreground` — active sessions go through `cancelling` to `cancelled` with `foreground handoff requested` — then the TUI resuming the session in its own directory). | `crates/xaft-cli/tests/agents_attach.rs`, `background_manager` unit tests |

**Row 47 was renamed in place by the 2026-10-23 re-baseline.** The row was
extracted when agenthicc's command was `trust cli` (hash `.agenthicc/cli/*.py`
into a project-local `trusted_cli.json`). agenthicc HEAD replaced it with
PRD-250 S-02's `trust list | add | revoke` over a user-owned store, generalised
to every extension kind. xaft implements that current shape and no literal
`trust cli` command. Until the re-baseline, `scripts/parity-row-reachability.py`
mapped `trust cli` → `trust add` in `COMMAND_RENAMES` so the stale name was
checked against a path clap accepts; now row 47 *is* `trust add` — the command
that does what `trust cli` did — its siblings are rows 82 (`trust list`) and 83
(`trust revoke`), and the rename entry is gone. The only executable extension
xaft has is the `cli.toml` table, so that is all `trust list` enumerates —
agenthicc's Python extension directories have no xaft loader to gate.

**Rows 50–53 and 59/60 read stores whose in-product producers are not ported.**
Orchestrations are created in agenthicc by the `spawn_worker_agents` agent tool
and the `parallel_code_plan` workflow, and background sessions by the background
supervisor (`run --background`); none of those is a matrix row and none exists in
xaft yet. These commands are complete over their stores — the tests create
orchestrations through the public `ParallelCoordinator` API and jobs through
`JobStore`, then drive the commands against them — which is the same standing as
the `jobs` (18–31) and `runs` (42–45) rows, whose stores have no runtime writer
either (§1d). Until a producer lands, a user's `worktrees list` and `agents`
will usually be empty.

Divergences, each deliberate and pinned by a test:

| Family | Rows | agenthicc | xaft | Why |
| --- | --- | --- | --- | --- |
| `mcp connect` / `refresh` / `doctor` | 36, 40, 41 | `NetworkGuard` only when `network_allow_list` is non-empty, so an empty list allows every host | the host must pass `[security]` (`ToolPolicy`): with `sandbox_mode` on, an empty list allows **no** remote host | xaft's `SecurityConfig` documents exactly that meaning; one key must not mean two things, and the matrix required no all-hosts guard |
| `mcp connect` / `refresh` / `doctor` | 36, 40, 41 | one malformed stanza makes every server fail (`allow_invalid=False`) | the malformed stanza is `failed` with its validation message; the rest work | agenthicc's own session path already does this |
| `mcp connect` / `refresh` / `doctor` | 36, 40, 41 | `error: … KeyError: "No MCP server …"`; a startup timeout's `last_error` is `""` | `error: … No MCP server registered with name 'x'`; `did not finish starting within 10s` | Python exception-class names and empty `TimeoutError` text carry nothing |
| `skills add`, `init` | 46, 57 | `error: …` and exit 0 | `error: …` and exit 1 | scripted installs and provisioning must see a failure |
| `skills add` | 46 | a local skill directory installs only its `SKILL.md`; `a/b` is GitHub even if `./a/b` exists; a root repository skill is named `repository` | the directory is copied whole; an existing local path wins; the skill is named after the repository | supporting files, no surprise network clone, a meaningful name |
| `trust` | 47, 82, 83 | `trust cli` / Python extensions; an unreadable store is overwritten by the next `trust add` | `trust list` / `add` / `revoke` over `cli.toml`; an unreadable store is never overwritten | the current agenthicc shape; no silent loss of records |
| `trust` | 47, 82, 83 | a `run` template is formatted, then shell-split; a later registration can replace a built-in command | split, then filled — every value is one argv element; built-ins cannot be shadowed | no argument injection from values; a repository cannot redefine `xaft run` |
| `worktrees show` | 51 | a malformed id escapes as a traceback | `Unable to show orchestration: invalid orchestration id`, exit 1 | |
| `init` | 57 | `.agenthicc/.agenthicc.toml` | `.xaft/xaft.toml` | the path xaft's loader reads |
| `agents` | 59 | only agents with a background session are listed | a run's main session and its dispatched workers are listed with the run's or task's status when no job exists | they are agents whether or not a job was recorded |
| `attach` | 60 | `attach --run` needs a background session for the run's main session | with none, the session is handed over directly in the run's repository root | there is no worker to stop |

---

## 2. Slash commands

> **agenthicc source:** `src/agenthicc/commands/builtins.py` (`BUILTIN_COMMANDS`).

agenthicc registers **31** built-in slash commands (`BUILTIN_COMMANDS`) plus
**4** aliases — five commands and one alias more than when this section was
extracted (rows 27–31, added by the 2026-10-23 re-baseline). xaft's
`SlashCommand` enum has **41** variants, several of which have no agenthicc
counterpart (they are Claude-Code-style extras: `/diff`, `/commit`, `/pr`,
`/rewind`, `/permissions`, `/theme`, `/vim`, `/emacs`, `/doctor`, `/memory`,
`/agents`; §2a). Re-derive the agenthicc half with
`python3 scripts/agenthicc-surface.py --section 2`.

| # | agenthicc command | aliases | xaft location | Status |
|---|---|---|---|---|
| 1 | `/replay` | — | `crates/xaft-tui` `SlashCommand::Replay` | ✅ |
| 2 | `/cancel` | `/interrupt` | crates/xaft-tui `SlashCommand::Cancel` | ✅ |
| 3 | `/loop` | — | crates/xaft-tui `SlashCommand::Loop` → `loops::LoopManager`, ticked by the TUI event loop (`AppState::tick_loop_scheduler`) | ✅ |
| 4 | `/loops` | — | crates/xaft-tui `SlashCommand::Loops` → `loops::LoopJobsMenu` overlay (table without an overlay host) | ✅ |
| 5 | `/clear` | — | crates/xaft-tui `SlashCommand::Clear` | ✅ |
| 6 | `/commands` | — | crates/xaft-tui `SlashCommand::Commands` | ✅ |
| 7 | `/tools` | — | crates/xaft-tui `SlashCommand::Tools` | ✅ |
| 8 | `/workflows` | — | `crates/xaft-tui` `SlashCommand::Workflows` | ✅ |
| 9 | `/config` | — | crates/xaft-tui `SlashCommand::Config` | ✅ |
| 10 | `/expand` | — | crates/xaft-tui `SlashCommand::Expand` | ✅ |
| 11 | `/help` | — | crates/xaft-tui `SlashCommand::Help` | ✅ |
| 12 | `/history` | — | crates/xaft-tui `SlashCommand::History` | ✅ |
| 13 | `/init` | — | crates/xaft-tui `SlashCommand::Init` | ✅ |
| 14 | `/mcp` | — | crates/xaft-tui `SlashCommand::Mcp` | ✅ |
| 15 | `/model` | — | crates/xaft-tui `SlashCommand::Model` | ✅ |
| 16 | `/models` | — | crates/xaft-tui `SlashCommand::Models` | ✅ |
| 17 | `/skills` | — | crates/xaft-tui `SlashCommand::Skills` + `slash/commands/skills.rs` | ✅ |
| 18 | `/status` | — | crates/xaft-tui `SlashCommand::Status` | ✅ |
| 19 | `/startup` | — | crates/xaft-tui `SlashCommand::Startup` over `xaft_runtime::startup::StartupCoordinator` (phases recorded by `app.rs` / `xaft-cli`) | ✅ |
| 20 | `/ps` | `/processes` | crates/xaft-tui `SlashCommand::Ps` → `terminals::TerminalListMenu` over the session's ONE `TerminalManager` | ✅ |
| 21 | `/workers` | — | crates/xaft-tui `SlashCommand::Workers` over `xaft_runtime::workers` manifests (recorded by `explore_repository` fan-outs) | ✅ |
| 22 | `/stop` | `/stop-terminal` | crates/xaft-tui `SlashCommand::Stop` over the same `TerminalManager` | ✅ |
| 23 | `/usage` | — | crates/xaft-tui `SlashCommand::Cost` | ✅ |
| 24 | `/mode` | — | crates/xaft-tui `SlashCommand::Mode` | ✅ |
| 25 | `/workflow` | — | `crates/xaft-tui` `SlashCommand::Workflow` | ✅ |
| 26 | `/compact` | — | crates/xaft-tui `SlashCommand::Compact` | ✅ |
| 27 | `/schedules` | — | — opens the repository's scheduled workflows (PRD-228) as a table or overlay; the subsystem is §1 rows 70–81 | ❌ |
| 28 | `/schedule` | — | — creates or controls one schedule from the TUI (`create …`, `run-now ID`, `pause ID`, `resume ID NEXT-RUN`, `delete ID --confirm`) | ❌ |
| 29 | `/inbox` | — | — reviews and answers background approvals, questions and input requests, filtered by type, project, age and status (PRD-227; §1 rows 63–67) | ❌ |
| 30 | `/background` | `/bg` | — hands the foreground session to the background supervisor, which then owns it as a durable background session (PRD-141). xaft's nearest capability is `Ctrl+B`, which moves the running pipeline to an in-process background slot of the same TUI and is gone when the TUI exits | ❌ |
| 31 | `/detach` | — | — a second name for row 30's handoff | ❌ |

**Subtotal: 31 rows.** ❌ 5 · ✅ 26

### 2b. Open slash rows and their owner (measured during G5)

**Rows 27–31 are open; they were added by the 2026-10-23 re-baseline.** All
five are ❌ and each is the TUI face of a CLI family §1c already sizes: `/schedules`
and `/schedule` are the `schedules` subsystem (§1 rows 70–81), `/inbox` is the
background inbox (rows 63–67), and `/background` / `/detach` hand a session to
the background supervisor (rows 58, 62), which xaft does not have. Their owner is
the same as those rows': **authorized, unclaimed**. A ✅ here needs the row's
command added to `MATRIX_DONE` in `crates/xaft-tui/tests/slash_parity_integration.rs`
in the same change (§0).

**None** of the 26 rows the section had before the re-baseline is open any
more. Rows 1, 8, 17 and 25 closed in
earlier passes; rows 3, 4, 19, 20, 21 and 22 — the six this table used to
carry as blocked, declined or deferred — closed in the 2026-10-23 parity push,
after the operator authorized implementing every agenthicc subsystem, the
previously declined and deferred ones included. All ten are kept here with
their evidence, and the owner/blocker history below is kept as the record of
why each row stood where it did.

Owners are cited by **stable goal id** (the first 8 hex of the goal's
`phase_id`), not by the `G<n>` label alone. A label records which increment
was intended; the id survives the goal list being renumbered or a label being
reused. Citing the label alone is what let the cells below go stale: they
named `G8` and `G9`, neither of which is scoped to the surface its row needs —
`G8` covers skill *loading*, `G9` background sessions and the session service,
and neither was verified against startup phases — and nothing in the table
noticed. Rows 17 and 19 were re-pointed when the convention was adopted, and
rows 20, 22 and keybinding row 11 were re-pointed by goal `be6c98b9`
(`be6c98b904564e448a8e13f2e2a98b71`), whose measurement showed that both owners
this table used to cite for them are now closed — `0cb7f6cf` is [verified] as
goal 48 and `010f6df6` as goal 88 — and a [verified] goal cannot own pending
work. Row 21 was the
last bare label — `G4b-7, then G9` — and is re-pointed here: neither label
resolved to an owner of the row, so the cell now names the goal that must land
the blocking layer *and* a goal whose own brief names the row. No cell below is
a bare `G<n>` label any more, so the next drift has to come from a goal id
being reused rather than from a label being read as an owner.

| # | command | blocked on | owner |
|---|---|---|---|
| 1 | `/replay` | ✅ **closed during G5a-1.** `crates/xaft-tui` `SlashCommand::Replay` + `slash/commands/replay.rs`. Mirrors agenthicc's three steps: with no argument it resolves the most recent session **for the working directory, excluding the active session**; an unknown id reports `Session {id[:16]} not found.` plus a location hint; otherwise the replay is pushed. It reads the session store's own conversation — the key `xaft_session::conversation_key_for` derives, i.e. the same store and the same key `xaft session inspect` reads — and **not** the `--resume` path's in-memory keys. Pinned by `crates/xaft-tui/tests/replay_slash_command.rs` (7 tests) plus the handler's unit tests. | closed |
| 3 | `/loop` | ✅ **closed in the 2026-10-23 parity push (branch `parity/tui-loops`).** The operator authorized every agenthicc subsystem, the deferred ones included, so the `LoopManager` this cell said was missing now exists: `crates/xaft-tui/src/loops/` ports agenthicc's PRD-193 scheduler — `LoopRecord` (agenthicc's `loop.json` field names), `LoopStore` (atomic, `0700`/`0600`, `core.data_dir/loops/<session>/loop.json`), `LoopManager` (create/replace, `status`/`pause`/`resume`/`stop`, coalescing, completion-to-start cadence, bounded exponential backoff, `max_consecutive_failures`, expiry, rehydration on explicit resume). It is **tick-driven** rather than an asyncio task: the event loop calls `AppState::tick_loop_scheduler` every pass, and a due iteration leaves through the very path a typed message takes (a prompt through `handle_submit` and the run channel, a `/command` through the slash dispatcher), so it gets the active mode, the approval gate and the conversation. `TaskComplete` re-arms it and `RuntimeError` counts a failure. Configured by `[loops]` (§7 rows 143-152, 185). Divergences, each recorded in code: `busy_poll_s` re-checks idleness every `busy_poll_s` (agenthicc's `min(due, busy_poll_s)` collapses to 0 once overdue); `/quit` is refused as a scheduled payload alongside agenthicc's `/loop`/`/workflow`/`/compact`; run *failures* (not only dispatch exceptions) count toward the failure limit; a fresh TUI owns its loop under a provisional id and **rebinds** it to the real session id after the first run, so `xaft --resume <id>` rehydrates it. Pinned by `loops/manager.rs` (16), `loops/store.rs` (7), `loops/model.rs` (4), `slash/commands/loop_cmd.rs` (5) and `crates/xaft-tui/tests/loop_slash_command.rs` (9), and by `MATRIX_DONE`. | closed |
| 4 | `/loops` | ✅ **closed in the 2026-10-23 parity push.** `SlashCommand::Loops` opens `loops::LoopJobsMenu` — agenthicc's `LoopJobsOverlay`: every valid persisted job newest-first, `Enter` makes the selected job due now (through the scheduler, never a second execution path) and closes with the result in the transcript, `d` deletes after an `Enter`/`y` confirmation, `r` refreshes, `j`/`k`/arrows/`PgUp`/`PgDn`/`Home`/`End` select. A job owned by another **live** session is refused for both actions. agenthicc decides liveness with its session lease; xaft has no session lease, so the owning TUI keeps an `owner.json` (its PID) next to the record and liveness is `kill(pid, 0)` — PID reuse can only make a dead owner look alive, which refuses (the safe direction). Without an overlay host the command prints agenthicc's `Scheduled Loops` table. The overlay is drawn by the renderer (see row 20's note on `MenuWidget::overlay_lines`). | closed |
| 8 | `/workflows` | ✅ **closed during G10.** `xaft-config::workflows::WorkflowRegistry` — three built-ins plus `[workflows.<name>]` custom tables. | closed |
| 17 | `/skills` | ✅ **closed during G5a-2.** `SlashCommand::Skills` + `slash/commands/skills.rs` list the skills the session loaded — one row per skill: the `$name` the `$` picker inserts, the description, and the source path `Skill` already carries — with agenthicc's own `(no skills found)` row when the list is empty and its 80-character description budget. The listing reads the **same `Arc`** `AppState::init_skills` publishes to the picker (`AppState::skills`), so the two cannot disagree about what exists; `AppState::new` builds the registry *before* the loader runs, so a handler holding a private copy would answer "no skills found" forever. `/skills` takes no arguments, and `/skills reload` is refused by the parser with the reason rather than accepted and ignored. **Two of agenthicc's three branches stay declined, not blocked:** the per-agent permission filter (agenthicc's `SkillPermissionSet` / `SkillDef.is_allowed_for`) — xaft *does* declare `AgentPreset::allowed_skills` / `denied_skills` and an `allows_skill` predicate (`crates/xaft-config/src/types.rs:435,501,460`), but **nothing on the injection path consults them**: `xaft-runtime/src/runtime.rs:337` injects every skill `load_all()` returns and `allows_skill` has no production caller, so a filtered listing would print a subset the run does not honour; and `/skills reload` (no reload hook — `SkillTriggerHandler.working_dir` is annotated "Reserved for future skill reload"). Pinned by `crates/xaft-tui/tests/skills_slash_command.rs` (8 tests) + 7 handler/catalog unit tests. | closed |
| 19 | `/startup` | ✅ **closed in the 2026-10-23 parity push** — the decline this cell recorded was answered by the operator's authorization of every agenthicc subsystem. `xaft_runtime::startup::StartupCoordinator` ports agenthicc's coordinator: `ReadinessState` (`not_started`/`loading`/`ready`/`degraded`/`failed`/`cancelled`), monotonic elapsed time, `deferred`, and an error passed through agenthicc's `_redact_error` (bearer tokens and `token`/`secret`/`password`/… values masked, 500-char bound). Every phase wraps real work: `configuration` (timed in `xaft-cli` around `ConfigLoader::load` and handed in with `TuiApp::with_startup`), `runtime`, `session_service` (**degraded** with the reason when SQLite cannot be opened), `selected_restore` (on `--resume`), `terminal_manager`, `loop_scheduler`, `first_frame`, `time_to_interactive_input`, and — deferred until after the first frame on a fresh session, as agenthicc defers extension discovery — `workspace_index` and `skills`, which load on spawned tasks and are delivered to the event loop. `/startup` prints agenthicc's `name: state (0.012s)` line per phase in name order, plus its documented `deferred` marker and the redacted error; no coordinator ⇒ `Startup diagnostics are unavailable.`, none recorded ⇒ `No startup phases have been recorded.` The ephemeral line shows `◌ Startup: loading …` while deferred phases run. Not ported: agenthicc's cancellable phase tasks and `wait_for` barrier — no xaft operation waits on a startup phase (the provider is built per run), so the coordinator is a thread-safe report store and `close()` cancels anything still loading at exit. Pinned by `startup.rs` (6) and `slash/commands/startup.rs` (3). | closed |
| 20 | `/ps` (`/processes`) | ✅ **closed in the 2026-10-23 parity push** (the owner was goal `7bde3a73`, G5a-4). The composition root now builds **one** `Arc<TerminalManager>` in `app.rs::run_inner`, injects it into the runtime with `XaftRuntime::with_terminal_manager`, and attaches the same `Arc` to `AppState` (`AppState::attach_terminal_manager` → `CommandContext.services.terminal_manager`) — the wiring the notes below asked for, so `/ps` lists exactly the terminals the agent's `run_command {"background": true}` started. The owner id is the resumed session's id, or a TUI-session id minted at startup for a fresh session (agenthicc mints its session id up front; xaft's agent session id does not exist until the first run). `/ps` is agenthicc's `_cmd_ps`: no manager ⇒ `Background terminal management is unavailable.`; `--json` ⇒ every record's `to_dict()` with sorted keys; with an overlay host ⇒ `terminals::TerminalListMenu` pre-selecting `[terminal-id]`; otherwise the `Background Terminals` table or `No background terminals for this session.` The overlay is **visible**: `MenuWidget::overlay_lines` gives a widget rows the renderer draws below the input bar in the trigger dropdown's reserved budget (`OVERLAY_MAX_ROWS` = 11), and the event loop redraws it when its projection changes. Before this, `MenuDriver` widgets captured keys but nothing drew them (PRD-61 G7 was never landed). Terminals still live at exit are stopped with the session (`TerminalManager::close`, bounded at 10 s). Pinned by `terminals.rs` (7), `slash/commands/ps.rs` (7), `tests/terminal_slash_commands.rs` (5), the three new `request_stop*` manager tests, and `MATRIX_DONE`. | closed |
| 21 | `/workers` | ✅ **closed in the 2026-10-23 parity push.** agenthicc's `_cmd_workers` is `ManifestStore().list()` filtered to `parent_session_id == ctx.session_id` (every manifest when there is no session id yet), rendered as `Parallel Workers` — Orchestration / Task / Status / Worktree — or as the manifests with `--json`. `SlashCommand::Workers` does exactly that over `xaft_runtime::workers`, which reads and writes manifests in agenthicc's **`ParallelManifest.to_dict()` wire format**, leniently (a corrupt or newer file is skipped, never fatal). What writes them is xaft's real parallel fan-out: the planner's `explore_repository` runs up to eight explorer subagents concurrently, and `WorkerRecorder` now records each call as one orchestration (one task per file, `running` → `completed`/`failed`, the orchestration `complete`/`failed`) under the workspace's `.xaft/workers/` — the per-workspace `.xaft/` the terminal store also uses. **Recorded divergence:** xaft's parallel workers today are those read-only explorer subagents, which share the session's workspace, so their Worktree cell is `—`; agenthicc's worktree-isolated *coding* workers belong to the `worktrees` subsystem (§1 rows 50–53). `/workers` also reads that subsystem's user-level store (`$XAFT_HOME/orchestrations`, else `~/.xaft/orchestrations`, the root the WIP `ManifestStore` documents), so those orchestrations appear with their worktrees the moment it writes them, with no TUI change. Pinned by `workers.rs` (4) and `slash/commands/workers.rs` (3). | closed |
| 22 | `/stop` (`/stop-terminal`) | ✅ **closed in the 2026-10-23 parity push**, over the same single manager as row 20. agenthicc's `_cmd_stop` message for message: no manager ⇒ the unavailable line; `/stop` ⇒ `Stop requested for {n} background terminal(s).`; `/stop all` without `--confirm`/`--force` ⇒ the repeat-to-confirm line; `/stop <id>` ⇒ `Stop requested for {id}.` or `No running owned terminal found for {id}.`; `--force` skips the grace period. The synchronous door is the new `TerminalManager::request_stop` / `request_stop_all` (agenthicc's names): they schedule the existing async `stop` on the runtime and keep its ownership rule — only a live child this manager spawned is ever signalled. | closed |
| 25 | `/workflow` | ✅ **closed during G10.** Session-local `WorkflowSelection`, read by `AppState::resolved_workflow()` at run-request build time. `reset` clears it. G10's first attempt built **two** independent selections — one for the slash registry, one for the field — so `/workflow` printed a confirmation while the run ignored it; fixed by sharing one `Arc`, and pinned by `crates/xaft-tui/tests/workflow_selection_wiring.rs` (4 of its 7 tests fail against the split version). | closed |

Keybinding row 1 (`Shift+Tab` mode cycle) was 🟡 because the cycle order and
alias semantics were *asserted* to differ from agenthicc's Safe → Plan →
Yolo. G7 measured it instead of trusting the row: `CYCLE_NAMES` is
`["safe", "plan", "auto"]`, `auto` is agenthicc's `Yolo` (reached through the
`auto → Yolo` alias), and `ModeManager::cycle` maps the active mode through
`canonical_mode_name` before advancing, so a user sitting in `ask` or `review`
still lands on the next real cycle step. The cycle is identical and the row is
now ✅, pinned by
`mode_parity_integration.rs::shift_tab_walks_safe_plan_yolo_and_wraps` and
`…::shift_tab_from_an_alias_enters_the_cycle_not_the_registry`. The mode
*semantics* that were still divergent — section 3 rows 1, 7 and 8 — closed in
the 2026-10-23 parity push; see §3b.

Keybinding row 11 (`s` in the terminal overlay, §5) is ✅ as of the 2026-10-23
parity push, with rows 20 and 22: `/ps` opens `terminals::TerminalListMenu`
over the session's one manager, and `s` (or `Ctrl+X`) on the selected row calls
`TerminalManager::request_stop` — which is what agenthicc's
`TerminalListOverlay.handle_key` does on `s`. The matrix row says "suspend";
agenthicc's key is a *stop* (graceful, escalating to `SIGKILL` after the grace
period), and xaft matches the behaviour, not the label. Pinned end to end by
`tests/terminal_slash_commands.rs::ps_opens_the_overlay_and_s_stops_the_selected_terminal`,
which starts a real process, opens `/ps`, presses `s`, and waits for the
process to be `stopped`. Its earlier attributions (`0cb7f6cf`, then
`7bde3a73`) are historical.

#### Closed: the TUI half of the terminal subsystem (2026-10-23)

The notes that follow — the composition-root blocker, the runtime-side wiring,
and the decision to defer the TUI half — are kept as history. The open item
they all ended on ("nothing threads the manager into
`AppState`/`CommandContext` yet") is closed: `app.rs::run_inner` builds one
`Arc<TerminalManager>`, hands it to `XaftRuntime::with_terminal_manager`, and
attaches the *same* `Arc` with `AppState::attach_terminal_manager`, from which
every `CommandContext` gets it through `SessionServices`. Two managers never
coexist for one session. A fresh TUI owns its terminals under a TUI-session id
minted at startup (the agent session id does not exist until the first run);
a resumed TUI uses the resumed session's id, so it sees the records it left
behind as orphaned, exactly as `TerminalManager::new`'s ownership recovery
intends. Rows 20, 22 and keybinding 11 are ✅ on that wiring.

#### Rows 20 and 22 are also blocked on the composition root, not just the TUI (measured during goal `010f6df6`; decided by goal `be6c98b9`) — historical, closed 2026-10-23

The owner cells above describe rows 20 and 22 as blocked on *"a rendering
surface over the terminal manager, which now exists"*. That is true of the
manager — `crates/xaft-tools/src/terminals/` is committed (`b790a11`, wired by
`99ed876`) and `cargo test -p xaft-tools` passes its 45 terminal tests — but it
is **not** the only blocker, and it is not the first one a pass hits. Measured at
HEAD `f9fd0d8`:

- **Nothing in production constructs a `TerminalManager`.**
  `grep -rn "TerminalManager::new\|TerminalManagerConfig::new" crates/ src/`
  matches only `tests/terminal_tools.rs` and the manager's own inline
  `#[cfg(test)]` modules. `ToolRegistryBuilder::new()` sets
  `terminal_manager: None` (registry.rs:169), and the two real registry builds —
  `runtime.rs:381` (reader) and `runtime.rs:389` (writer) — never call
  `.with_terminal_manager(…)`. `XaftRuntime` (`runtime.rs:49`) has no such
  field at all.
- **So the agent-visible tools already refuse at runtime**, even though section 6
  marks rows 36/43/44/45 ✅. `run_command {"background": true}` returns
  `"error": "background terminals are unavailable in this execution context"`
  (run_command.rs:288-297) and the four terminal tools return
  `"terminal manager unavailable"` (terminals/tools.rs:64). Those four rows are
  ✅ as a *registry* surface and are **not reachable in the product** — the
  §2c false-parity shape, one section over.

A `/ps` and `/stop` built over `CommandContext.terminal_manager: Option<…>`
would therefore render `No background terminals for this session.` on every
invocation, which is precisely the always-empty-store trap §2b records for
`worktrees list`: a real command over a store nothing writes to. Constructing a
*separate* manager inside the TUI is worse than doing nothing, because
`TerminalManager::new` marks every inherited **active** record for the same
session as `Orphaned` (manager.rs:227-243) — so a TUI-owned manager over the
same store root would actively refuse to stop terminals the agent started, and a
stale PID is never signal-eligible anyway.

The real work is therefore the **composition root**: build one
`Arc<TerminalManager>` where both `session_id` and `working_dir` are known
(`xaft-tui/src/app.rs::run_inner`), share it into the runtime (a
`with_terminal_manager` builder on `XaftRuntime`, alongside the existing
`with_stores`) **and** into `AppState`/`CommandContext`. Two managers must never
coexist for one session. That wiring is owned by goal `f762f2c3` below; rows 20
and 22 remain open until it lands. Do not build the two slash commands first —
they would be correct against an empty store and therefore still lie.

##### The wiring has landed — what changed, measured after goal `f762f2c3`

The two bullets above are now **historical**: they measured HEAD `f9fd0d8`, and
the first half of the wiring they ask for — the runtime side — now exists in
`crates/xaft-runtime/src/runtime.rs`. The second half (sharing the same manager
into `AppState`/`CommandContext`) is still the open item named at the end of
this note. `crates/xaft-runtime/src/runtime.rs`:

* `XaftRuntime` carries `terminal_manager: Option<Arc<TerminalManager>>`, with a
  `with_terminal_manager(manager)` builder beside `with_stores` — the injection
  point a composition root uses when it outlives a single run.
* `run_task` resolves **one** manager per run, before it builds any registry:
  `self.resolve_terminal_manager(session.id.as_str(), working_dir)`. `run_task`
  is the only frame where both the session id (minted inside it) and the run's
  working directory are known on *every* path — headless, `--json` and TUI —
  which is why the manager is resolved there rather than in `bootstrap`.
* That single `Arc` is handed to the coder registry's
  `ToolRegistryBuilder::with_terminal_manager(...)`, so the four agent-visible
  tools and `run_command {"background": true}` no longer refuse in the product.
  `build_reader` takes no manager, because the four terminal tools are
  coder-registry tools; rows 36/43/44/45 are therefore reachable in the built
  product rather than only in a registry built by a test.
* Resolution happens **after** the dry-run shortcut, so `--dry-run` still
  touches no store. `resume_session` propagates the injected manager into the
  resumed runtime, so the one-manager guarantee survives `--continue`.
* An injected manager wins outright: no second manager is built beside it, which
  is the property the ownership-recovery design requires.

Consequence worth knowing before building rows 20/22 on top of it: a run leaves
a store at `<working_dir>/.xaft/`, `TerminalManagerConfig`'s own default root,
so terminal state is per-workspace rather than in `core.data_dir` beside
`jobs/`, `runs/` and the session database. That is the manager's documented
default and what its 45 tests exercise; relocating it is a separate decision,
not part of this wiring.

Still open at the time, and unchanged by the above: nothing threaded the
manager into `AppState`/`CommandContext` yet, so rows 20/22 and keybinding 11
kept ❌ and their DEFERRED status, owned by goal `7bde3a73` (G5a-4). (Closed
since — see "Closed: the TUI half of the terminal subsystem" above.) The pins for the wiring
are the four tests in `runtime.rs`:
`a_manager_is_built_over_the_session_and_working_directory_of_the_run`,
`an_injected_manager_is_the_one_a_run_resolves`,
`a_run_builds_a_terminal_manager_over_its_working_directory` and
`a_run_never_builds_a_second_manager_beside_an_injected_one`.

#### The decision: the TUI half is DEFERRED, not declined (recorded by goal `be6c98b9`) — historical, closed 2026-10-23

Goal `be6c98b9` was briefed to decide the TUI half of this subsystem explicitly
— slash row 20 `/ps`, slash row 22 `/stop` and keybinding row 11 — because goal
`df972384d`'s TEST clause required that surface be "done or explicitly deferred",
and goal 88 (`010f6df6`) had measured the blocker and returned with neither.
**This note is that deferral.** All three rows keep ❌; no row's status changes on
account of it, and no code was written for it.

* **Not implemented now.** A `/ps` and `/stop` written today would compile and
  pass a handler test, and would still be empty in the product: nothing
  constructs a `TerminalManager` outside tests (measured above, re-confirmed at
  HEAD `24e6f24`). Marking the two rows ✅ on that basis is the §2c defect — a ✅
  whose reachable surface is empty — and is precisely what
  `scripts/parity-row-reachability.py` exists to catch.
* **Not declined.** Unlike `/loop` (row 3) and `/startup` (row 19) — both since
  authorized and implemented — the subsystem
  behind these rows is not hypothetical: `crates/xaft-tools/src/terminals/` is
  committed and tested, the four agent-visible tools resolve in a built
  registry, and the only missing piece is the composition root. Declining would
  discard a surface that is one wiring increment away, which is why these rows
  carry an owner instead of a decline.
* **Owner: goal `7bde3a73` (G5a-4)**, sequenced after goal `f762f2c3`, which is
  the wiring it depends on. Goal 86's TEST clause — "TUI surface done or
  explicitly deferred to a follow-up goal" — is satisfied by that pairing:
  deferred here, owned there.
* **Re-measured at `24e6f24`:** `grep -rn "Ps\|StopTerminal"
  crates/xaft-tui/src/slash/` returns 0 hits; the `SlashCommand` enum has no
  `Ps`/`Stop` variant; `COMMAND_TABLE` (`slash/parser.rs:266`) has no `ps`/`stop`
  key; §5 row 11 is ❌ with `—` in its xaft column; and all fifteen
  `TerminalManager::new` sites are under a test — three in
  `crates/xaft-tools/tests/terminal_tools.rs` and twelve in the manager's own
  inline `#[cfg(test)]` block, which opens at `src/terminals/manager.rs:1362`
  while the earliest of those twelve calls is at `:1492`. `with_terminal_manager`
  has three call sites — two in `tests/terminal_tools.rs` and one in production,
  `registry.rs:444`, itself inside `if let Some(manager)` — plus the two
  definitions (`registry.rs:215`, `shell/run_command.rs:137`). So
  the question is whether anything supplies a manager, and nothing does:
  `ToolRegistryBuilder::new()` sets `terminal_manager: None`
  (`registry.rs:182`), and neither real registry build
  (`crates/xaft-runtime/src/runtime.rs:381`, `:389`, the reader and the
  writing registry) calls `with_terminal_manager` — both take the `None`
  default.

agenthicc's two contracts, measured for whoever implements this
(`agenthicc/commands/builtins.py`), so the pass does not re-read them:

- `_cmd_ps` (:540-588) — no manager ⇒ `Background terminal management is
  unavailable.`; `--json` ⇒ `json.dumps([record.to_dict() …], sort_keys=True)`;
  otherwise it hands `manager` to a `TerminalListOverlay(manager, on_close,
  selected_id=selected)`; empty ⇒ `No background terminals for this session.`;
  else a `Background Terminals` table of ID / State / Command / Return with a
  null `returncode` rendered as `—`.
- `_cmd_stop` (:641-671) — no manager ⇒ the same unavailable line; honours
  `--force`; `all` requires `--confirm`/`--force` or it prints
  `This will stop every visible background terminal. Repeat as /stop all
  --confirm (or --force) to confirm.`; no target ⇒ `Stop requested for {count}
  background terminal(s).`; a named target ⇒ `Stop requested for {target}.`,
  else `No running owned terminal found for {target}.`

Keybinding row 1 (`Shift+Tab`) is re-confirmed here rather than assumed: `G7`
is [verified] and did the measuring, both named tests still exist
(`mode_parity_integration.rs:266` and `:284`), and the row is ✅ in
section 5. So its `G7` attribution is historical and correct, unlike rows
20 and 22 above.

### 2c. A ✅ row that was not actually reachable (found and fixed during G5)

Row 23 (`/usage`) was marked ✅ because `/usage` appears in the `aliases` list
of the `/cost` entry — but the parser resolves a trigger by **table key**, not
by scanning that alias list. `usage` had no key of its own, so `/usage`
answered `Unknown command: /usage` while the matrix, `/help` and `/commands`
all advertised it. The row was claiming parity that did not exist.

Fixed by adding the missing `COMMAND_TABLE` entry, and guarded three ways so
the class cannot recur:

- `parser.rs::every_declared_alias_parses_as_a_command` — every alias declared
  in `CommandMeta::aliases` must have its own key, or it is unreachable.
- `tests/slash_parity_integration.rs::every_agenthicc_command_the_matrix_marks_done_actually_parses`
  — every agenthicc command this section marks ✅ must parse *and* have a
  registered handler, using agenthicc's own spelling rather than xaft's.
- `…::matrix_done_commands_survive_full_dispatch` — the same set must survive
  the full `AppState` dispatch path, which is the level at which the earlier
  missing-`status`/`tools`-row defect lived.

This is the failure mode G4b-7 warns about for `worktrees list`: a read-only
view of something that is always empty looks finished and is not.

### 2a. xaft-only slash commands (no agenthicc counterpart)

These are supersets, not gaps — they do not block parity but are recorded for completeness.

- `/diff`
- `/commit`
- `/pr`
- `/rewind`
- `/permissions`
- `/theme`
- `/vim`
- `/emacs`
- `/doctor`
- `/memory`
- `/agents`
- `/login`
- `/logout`
- `/quit`

---

## 3. Modes

> **agenthicc source:** `src/agenthicc/modes/builtin.py`, `modes/registry.py`,
> `docs/guides/modes.md`.

agenthicc has **3 selectable** mode identities plus **1 internal** one, and
**4** aliases. It cycles **Safe → Plan → Yolo → Safe**
(`ModeManager.cycle()` walks `registry.all()`, which is registration order,
and wraps). xaft registers **6** modes but cycles **3** of them. Every row
below was re-measured against the source during G7; see **3b** for what moved
and what deliberately did not.

| # | agenthicc mode | alias | agenthicc semantics | xaft location | Status |
|---|---|---|---|---|---|
| 1 | `Safe` | — | Reads/search/git-read run directly; every `WRITE`, `GIT_WRITE`, `EXECUTE`, `NETWORK` and `UNDECLARED` tool requires **explicit user approval** (`RuntimeMode.approval_required`, enforced by the runtime capability gate). | crates/xaft-tui mode `safe` — no tool filter; `approval_filter` = `safe_requires_approval` (every tool outside `SAFE_DIRECT_TOOLS`) → `RunRequest::mode_approval_filter` → `xaft_runtime::mode_gate` wraps each so the executor routes it through the approval gate, and the TUI never auto-approves it (`ApprovalQueue::push_manual`) | ✅ |
| 2 | `Plan` | — | Read-only planning; `blocked_capabilities` **hard-blocks** the five restricted capabilities. | crates/xaft-tui mode `plan` — 15-tool read-only allowlist + a `MUST NOT` system patch | ✅ |
| 3 | `Yolo` | — | Full automatic; every tool allowed, no prompt, no patch. | crates/xaft-tui mode `auto` — no `tool_filter`, no system patch | ✅ |
| 4 | `Yolo` | alias `auto` | `MODE_ALIASES["auto"] = "Yolo"` | `auto` **is** xaft's canonical spelling of Yolo; `yolo` is accepted as its alias | ✅ |
| 5 | `Safe` | alias `ask` / `guard` | `MODE_ALIASES["ask"] = "Safe"`, `["guard"] = "Safe"` | aliases `ask` and `guard` → mode `safe` | ✅ |
| 6 | `Plan` | alias `review` | `MODE_ALIASES["review"] = "Plan"` | alias `review` → mode `plan` | ✅ |
| 7 | `Replay` (internal) | — | Registered with `selectable=False`; all nine capabilities blocked; deliberately absent from the cycle and from the `/mode` listing. | crates/xaft-tui mode `replay` — `AgentModeBuilder::internal()`: not selectable (`/mode replay` is unknown), not cycled, not listed; its tool filter blocks every tool; `/replay` runs under it via `ModeManager::set_internal` and restores the prior mode | ✅ |
| 8 | — | `debug` rejected | agenthicc registers no `Debug` identity, so `/mode debug` raises `UnknownModeError`. The name is *also* reserved: a mode plugin named `debug` is dropped with "Debug is a rejected mode name", and `debug` is filtered out of workflow bindings. | no `debug` mode: `/mode debug` answers `Unknown mode 'debug'. Choose one of: auto, plan, safe.` and `ModeRegistry::register` refuses the name from any source (`RESERVED_MODE_NAMES`) | ✅ |

**Subtotal: 8 rows.** ✅ 8

### 3b. Mode rows re-measured during G7

Four rows changed, and the corrections are all cases where the row asserted
something the code does not do.

**Row 5 was wrong about `guard`.** It read "`guard` **not accepted**", 🟡.
`canonical_mode_name` has mapped `guard → safe` all along, and
`crates/xaft-tui/tests/mode_parity_integration.rs::every_agenthicc_alias_actually_switches_the_active_mode`
now drives the real `ModeManager::set` to prove it. Row 5 is ✅.

**Row 3 was 🟡 for a naming reason only.** It is the same row as the old row 4
(alias `auto`), and row 4 was already ✅ — a mode cannot be divergent about
`Yolo` and matchable by its own alias. agenthicc's `Yolo` carries no
`blocked_capabilities` and no `approval_required`; xaft's `auto` carries no
`tool_filter` and no system patch. Same permissions, same silence. The badge
differs (`⏯⏯` vs `AUTO`), which is presentation, not semantics. Row 3 is ✅.

**Row 1 is the real divergence, and it is not a one-line fix.** agenthicc's
`Safe` is *gate-and-ask*: reads run directly, writes run after the user says
yes. xaft has no mode with that shape at all. `safe` hard-blocks writes
behind a read-only allowlist and never prompts; the only mode carrying
agenthicc-Safe's semantics is the registered `ask` mode (unfiltered tools + a
`[Y/n]` instruction) — and **`ask` cannot be reached**, see below. xaft also
enforces tool approval globally and per-destructive-tool rather than from a
mode's `approval_required` set, so even a restored `ask` mode would be a
model-side instruction where agenthicc's is a runtime gate. That is why the
row is 🟡 and not ❌: the *intent* is present, the enforcement locus is not.
Closing it properly means an approval-capability gate on `RunRequest` — new
subsystem work, not a mode edit. *(That gate now exists and the row is ✅ —
see §3d.)*

**Row 7 (`Replay`) was missing entirely.** agenthicc registers a fourth
identity with `selectable=False`; it is a real mode in `registry.all(
include_internal=True)` even though it is not user-selectable, so it belongs
in a matrix scoped to agenthicc's modes. ❌. *(Implemented — see §3d.)*

### 3c. Two registered modes cannot be selected by their own name

Found during G7 and previously recorded nowhere. `ModeManager::set` resolves
the requested name through `canonical_mode_name` **before** it consults the
registry, and that alias table contains `ask → safe` and `review → plan` —
both copied verbatim from agenthicc's `MODE_ALIASES`. The registered `ask`
and `review` modes are therefore unreachable by *any* spelling:

| registered mode | what it does | what `/mode <its own name>` actually does |
|---|---|---|
| `ask` | every tool available, side effects behind a model-side `[Y/n]` prompt | activates `safe` — every tool available, side effects behind a **runtime** approval gate (since §3d) |
| `review` | structured review prompt, Plan's 15-tool allowlist | activates `plan` — same allowlist, different prompt |

`mode_parity_integration.rs::two_of_the_five_selectable_modes_are_shadowed_by_the_alias_table`
enumerates the selectable registry (the internal `replay` mode is not
selectable) and proves the set is exactly these two, so a third cannot appear
unnoticed. `debug` is no longer a mode at all (row 8, §3d).

**DECISION: keep the alias shadowing. None of the three fixes is
authorized, and that is now a recorded outcome rather than an open question.**
The operator was asked to choose between the three directions below and
**did not answer within the deadline**, so no authorization exists for any of
them and the measured behaviour stands deliberately:

- *Prefer an exact registered name over the alias* — **NOT AUTHORIZED**
  (fail-open). This would make `/mode ask` reach the unfiltered `[Y/n]` mode,
  silently moving a currently accepted input from a hard read-only sandbox to
  **every tool**, gated only by a system-patch instruction the model is free
  to ignore. A fail-open security change is never a silent one.
- *Delete the two dead modes* — **NOT AUTHORIZED** (feature removal). `ask` is
  in any case the only registered mode carrying agenthicc-`Safe`'s
  gate-and-ask shape, so deleting it would also close off row 1's only route
  to real parity.
- *Rename xaft's `safe`* (e.g. to `sandbox`) so the names `ask`/`safe` can
  carry agenthicc's meaning — **NOT AUTHORIZED** (rename of a shipped
  user-facing mode name), and on its own it would not close row 1 anyway,
  which needs a runtime approval gate rather than a better name.

**Why leaving it is the safe direction, not merely the convenient one.** The
two shadowings are not equivalent, and the difference is what makes this a
settled outcome:

| shadowed name | what you type | what you get | direction |
|---|---|---|---|
| `ask` | `/mode ask` | `safe` — every side effect behind an explicit runtime approval (§3d; before that, a 12-tool read-only allowlist) | **fail-closed**: strictly more protective than the `[Y/n]` mode that was requested, whose prompt the model is free to ignore |
| `review` | `/mode review` | `plan` — the same 15-tool allowlist, different system prompt | not a security change at all; only the prompt differs |

So keeping the alias table never grants a capability the user did not already
have, and the one shadowing that *is* security-relevant (`ask`) errs toward
restriction. `mode_parity_integration.rs::two_of_the_five_selectable_modes_are_shadowed_by_the_alias_table`
pins the exact shadowed set, and `plan_blocks_and_safe_gates_every_write_tool`
pins that the mode `/mode ask` resolves to gates every write on an explicit
approval. Measured, not
assumed: deleting the `"ask" => "safe"` arm — which is what option 1 requires —
turns **six** tests in `mode_parity_integration.rs` red (re-measured on
2026-10-23: G7's five, plus
`slash_mode_refuses_debug_and_replay_without_switching`, whose expected
`Choose one of: auto, plan, safe.` would gain `ask`), so the fail-open change
cannot land unnoticed.

**What WOULD reverse this.** Only an explicit operator authorization naming
the option, which then means editing `ModeManager::set` and the test above in
the same change. Reversing it without that authorization would be the
fail-open change described in option 1, so it is not a refactor.

What was done instead is to make the code stop *claiming* otherwise: the
`/mode` handler's comment that "`/mode debug` is rejected" and
`ModeManager::reject_if_not_cyclable`'s "mirroring agenthicc's `/mode` guard
(Debug is not an alias and is rejected)" were both false — `builtin_modes()`
contains `debug`, so the guard's `!any(...)` never fires, and
`reject_if_not_cyclable` is called from nowhere except its own unit test.
Both comments now describe the measured behaviour, and the tests pin it.

### 3d. Rows 1, 7 and 8 closed (2026-10-23 parity push)

The operator authorized implementing every agenthicc feature, which answers the
open questions this section recorded. All three remaining mode rows are ✅.

**Row 1 — `Safe` is gate-and-ask.** `safe` no longer filters tools out; its
`approval_filter` (`builtins::safe_requires_approval`) selects every tool
outside `SAFE_DIRECT_TOOLS` — agenthicc's `READ`/`SEARCH`/`GIT_READ`/`CONTROL`
capabilities by xaft tool name (`read_file`, `grep`, `git_status`, `recall`,
`handoff_to_agent`, …). Everything else — writes, git writes, shell and code
execution, the terminal tools (agenthicc tags them `EXECUTE`), `read_url`,
memory writes, and any unknown tool (agenthicc's fail-closed `UNDECLARED`) —
must be approved before **each** call. The enforcement locus is the runtime,
as agenthicc's is: `ModeManager::apply_to_run_request` sets the new
`RunRequest::mode_approval_filter`, `XaftRuntime::run_task` wraps each matching
tool in `xaft_runtime::mode_gate::ApprovalRequiredTool` (its
`requires_confirmation()` is `true`, everything else delegates), and the agtrs
executor sends it through the run's `ApprovalGate` — the path `bash_exec`
already takes. The TUI queues such approvals with
`ApprovalQueue::push_manual`, so the low-risk auto-approve policy can never
answer a Safe-mode approval: agenthicc's Safe requires *explicit* approval.
That holds after Ctrl+B too, and the filter is the one the *running* run was
submitted under: `AppState::run_approval_filters` keeps one entry per run
handed to the runtime (which runs them one at a time) and retires it on the
run's `TaskComplete` / `RuntimeError`, so a task queued behind a background run
never lends it its mode. (Fixed with it: a background run's approval that the
policy *did* answer was dropped, leaving that run waiting on its gate for good.)
The system patch is agenthicc's `## SAFE MODE` suffix. Not changed: the default
mode stays `auto` (agenthicc's default is `Safe`; that is not a matrix row, and
switching every new session to approve-every-write is a separate decision).
Pinned end to end by `crates/xaft-runtime/tests/mode_approval_gate.rs` (2: a
scripted coder's `write_file` reaches the gate and, rejected, writes nothing;
without the filter the same write runs unasked), plus `mode_gate.rs` (3), the
builtins and manager unit tests, five `state.rs` approval-routing tests
(foreground, Ctrl+B, and a task queued behind a background run), and
`mode_parity_integration.rs::plan_blocks_and_safe_gates_every_write_tool`.

**Row 7 — internal `replay` mode.** Registered with
`AgentModeBuilder::internal()`: `ModeManager::set` refuses it as an unknown
mode (agenthicc's `set_by_name` returns `None` for an internal mode), the
Shift+Tab cycle and the `/mode` listing never show it, and its tool filter
blocks every tool. `/replay` runs under it — `AppState` enters it with
`ModeManager::set_internal("replay")` around the replay and `restore`s the prior
mode afterwards, agenthicc's `_run_replay`. (xaft's replay is synchronous within
the command, so the window is the command's own execution.) Pinned by
`mode_parity_integration.rs::replay_is_registered_internal_and_unselectable`,
`…::replay_runs_under_the_internal_replay_mode_and_restores_the_prior_one`
(a probe handler observes `replay` as the active mode while `/replay` runs)
and `manager.rs::replay_is_entered_only_internally_and_restores`.

**Row 8 — `debug` is rejected.** The built-in `debug` mode is gone (its
post-hook was never applied anywhere, so selecting it behaved exactly like
`auto`), `/mode debug` answers `Unknown mode 'debug'. Choose one of: auto, plan,
safe.`, and `ModeRegistry::register` refuses the name from any source
(`RESERVED_MODE_NAMES`), as agenthicc drops a mode plugin named `debug`.

**Found while closing these: `/mode <name>` never switched.** The handler
printed `Switching to 'plan' mode` while nothing called `ModeManager::set` —
row 24 was a confirmation for a no-op, the §2c shape. `AppState` now performs
the switch (`AppState::switch_mode`, announced like Shift+Tab and emitting
`XaftModeChanged`) before the handler reports it, and the `/mode` listing
shows only the modes `/mode` accepts by name (`auto`, `plan`, `safe`) plus the
alias line. Pinned by `mode_parity_integration.rs::slash_mode_switches_the_active_mode`
and `…::slash_mode_refuses_debug_and_replay_without_switching`.

---

## 4. TUI triggers

> **agenthicc source:** `src/agenthicc/tui/triggers/slash_command.py`,
> `src/agenthicc/tui/triggers/at_mention.py`.

| # | trigger | agenthicc class | xaft handler | Status |
|---|---|---|---|---|
| 1 | `/` | `SlashCommandTrigger` — Slash palette | crates/xaft-tui `SlashCommandTriggerHandler` | ✅ |
| 2 | `@` | `AtMentionTrigger` — File/dir mention picker | crates/xaft-tui `MentionTriggerHandler` | ✅ |
| 3 | `$` | `SkillTrigger` — Skill-only picker | crates/xaft-tui `SkillTriggerHandler` | ✅ |

**Subtotal: 3 rows.** ✅ 3

> Note: agenthicc has no `#` trigger, so it is not a parity row. xaft's `#`
> history trigger IS registered — in `AppState::new` (`state.rs`), not in
> `trigger/mod.rs`, which is why an earlier reading of the module doc missed
> it. The module doc's handler tree also listed `$` twice with a malformed
> branch; that is fixed as part of G5.

---

## 5. Keybindings

> **agenthicc source:** `src/agenthicc/tui/input/capabilities.py`,
> `src/agenthicc/tui/workspace/overlays/*`, `docs/guides/tui.md`.

| # | key | agenthicc action | xaft location | Status |
|---|---|---|---|---|
| 1 | `Shift+Tab` | Cycle mode Safe → Plan → Yolo, wrapping | crates/xaft-tui `state.rs` → `ModeManager::cycle` over `CYCLE_NAMES` = `safe, plan, auto` (`auto` is Yolo) | ✅ |
| 2 | `Ctrl+C` | Cancel current intent | crates/xaft-tui input | ✅ |
| 3 | `Esc` | Pause / back out of overlay | crates/xaft-tui input | ✅ |
| 4 | `Ctrl+V` | Reveal collapsed paste placeholder | crates/xaft-tui input bar | ✅ |
| 5 | `Enter` | Submit / select in overlay | crates/xaft-tui input | ✅ |
| 6 | `Up`/`Down` | Navigate picker list | crates/xaft-tui palette | ✅ |
| 7 | `Home`/`End` | Move to start/end of visible line | crates/xaft-tui input bar | ✅ |
| 8 | `PageUp`/`PageDown` | Scroll transcript | crates/xaft-tui transcript | ✅ |
| 9 | `k`/`j` | Vim-style up/down | crates/xaft-tui slash `/vim` | ✅ |
| 10 | `y`/`n` | Confirm / deny in overlays | crates/xaft-tui overlays | ✅ |
| 11 | `s` | Suspend in terminal overlay | crates/xaft-tui `terminals::TerminalListMenu` (opened by `/ps`) — `s`/`Ctrl+X` → `TerminalManager::request_stop` on the selected terminal | ✅ |
| 12 | `?` | Toggle help overlay | crates/xaft-tui help overlay | ✅ |

**Subtotal: 12 rows.** ✅ 12

> **Row 11 — closed 2026-10-23.** agenthicc's `TerminalListOverlay` binds `s`
> (and `Ctrl+X`) to `manager.request_stop(selected)`: the "suspend" label is a
> graceful stop that escalates to `SIGKILL` after the grace period. xaft's
> `/ps` overlay (`terminals::TerminalListMenu`, drawn by the renderer through
> `MenuWidget::overlay_lines`) does the same over the session's one
> `TerminalManager`, keeps the selection pinned to a terminal id so a refresh
> can never retarget the key, and reports `Stop requested for {id}.` in the
> overlay. Pinned end to end by
> `tests/terminal_slash_commands.rs::ps_opens_the_overlay_and_s_stops_the_selected_terminal`.
> History and agenthicc's exact `_cmd_ps` / `_cmd_stop` contracts are in §2b.

---

## 6. Built-in tools

agenthicc exposes **78** built-in tools across 8
`*_AGENT_TOOLS` lists — re-derived by the 2026-10-23 re-baseline and unchanged
(`python3 scripts/agenthicc-surface.py --section 6`; `MCP_AGENT_TOOLS` is
defined in `tools/mcp_guidance.py` and reaches the aggregate through the root
`agent_tools.py`, which is why row 78 cites that file). xaft's registry holds
**73** tools — the count is the registry snapshot in
[tool-registry.txt](tool-registry.txt), not a source grep. A `const TOOL_NAME`
count under-reports by five: `mcp_connection_guide` returns its name from
`McpConnectionGuideTool::name()` and the four terminal tools return their names
as literals from `terminals/tools.rs`, so none of the five has a const to grep.
Every tool this section marks ✅ is proven resolvable in a built `ToolRegistry`
by `crates/xaft-tools/tests/tool_parity_reachability.rs`.

### 6.6 `tools/fs/agent_tools.py` (24 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 1 | `append_file` | `tools/fs/agent_tools.py` | `append_to_file` (renamed) | ✅ |
| 2 | `apply_diff` | `tools/fs/agent_tools.py` | `patch_file` (renamed) | ✅ |
| 3 | `batch_copy` | `tools/fs/agent_tools.py` | `batch_copy` | ✅ |
| 4 | `batch_delete` | `tools/fs/agent_tools.py` | `batch_delete` | ✅ |
| 5 | `batch_move` | `tools/fs/agent_tools.py` | `batch_move` | ✅ |
| 6 | `batch_read` | `tools/fs/agent_tools.py` | `batch_read` (renamed from read_many) | ✅ |
| 7 | `batch_write` | `tools/fs/agent_tools.py` | `batch_write` | ✅ |
| 8 | `checksum_file` | `tools/fs/agent_tools.py` | `checksum_file` | ✅ |
| 9 | `copy_file` | `tools/fs/agent_tools.py` | `copy_file` | ✅ |
| 10 | `delete_file` | `tools/fs/agent_tools.py` | `delete_file` | ✅ |
| 11 | `file_exists` | `tools/fs/agent_tools.py` | `file_exists` | ✅ |
| 12 | `get_file_info` | `tools/fs/agent_tools.py` | `file_stat` (renamed) | ✅ |
| 13 | `grep_file` | `tools/fs/agent_tools.py` | `grep` (renamed) | ✅ |
| 14 | `grep_files` | `tools/fs/agent_tools.py` | `search_files` (renamed) | ✅ |
| 15 | `list_directory` | `tools/fs/agent_tools.py` | `list_files` (renamed) | ✅ |
| 16 | `make_directory` | `tools/fs/agent_tools.py` | `create_directory` (renamed) | ✅ |
| 17 | `move_file` | `tools/fs/agent_tools.py` | `move_file` | ✅ |
| 18 | `patch_file` | `tools/fs/agent_tools.py` | `patch_file` | ✅ |
| 19 | `read_file` | `tools/fs/agent_tools.py` | `read_file` | ✅ |
| 20 | `read_lines` | `tools/fs/agent_tools.py` | `read_lines` | ✅ |
| 21 | `search_files` | `tools/fs/agent_tools.py` | `search_files` | ✅ |
| 22 | `touch_file` | `tools/fs/agent_tools.py` | `touch_file` | ✅ |
| 23 | `truncate_file` | `tools/fs/agent_tools.py` | `truncate_file` | ✅ |
| 24 | `write_file` | `tools/fs/agent_tools.py` | `write_file` | ✅ |

### 6.7 `tools/git/agent_tools.py` (11 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 25 | `git_add` | `tools/git/agent_tools.py` | `git_add` | ✅ |
| 26 | `git_blame` | `tools/git/agent_tools.py` | `git_blame` | ✅ |
| 27 | `git_branch` | `tools/git/agent_tools.py` | `git_branch` | ✅ |
| 28 | `git_checkout` | `tools/git/agent_tools.py` | `git_checkout_files` (renamed) | ✅ |
| 29 | `git_commit` | `tools/git/agent_tools.py` | `git_commit_staged` (renamed) | ✅ |
| 30 | `git_diff` | `tools/git/agent_tools.py` | `git_diff` | ✅ |
| 31 | `git_grep` | `tools/git/agent_tools.py` | `git_grep` | ✅ |
| 32 | `git_log` | `tools/git/agent_tools.py` | `git_log` | ✅ |
| 33 | `git_show` | `tools/git/agent_tools.py` | `git_show` | ✅ |
| 34 | `git_stash` | `tools/git/agent_tools.py` | `git_stash` | ✅ |
| 35 | `git_status` | `tools/git/agent_tools.py` | `git_status` | ✅ |

### 6.8 `tools/exec/agent_tools.py` (10 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 36 | `inspect_terminal` | `tools/exec/agent_tools.py` | `inspect_terminal` | ✅ |
| 37 | `run_bash` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 38 | `run_command` | `tools/exec/agent_tools.py` | `run_command` | ✅ |
| 39 | `run_python` | `tools/exec/agent_tools.py` | `run_python` | ✅ |
| 40 | `run_python_expr` | `tools/exec/agent_tools.py` | `run_python_expr` | ✅ |
| 41 | `run_tests` | `tools/exec/agent_tools.py` | — | ❌ |
| 42 | `shell` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 43 | `stop_terminal` | `tools/exec/agent_tools.py` | `stop_terminal` | ✅ |
| 44 | `wait_terminal` | `tools/exec/agent_tools.py` | `wait_terminal` | ✅ |
| 45 | `wait_terminal_ready` | `tools/exec/agent_tools.py` | `wait_terminal_ready` | ✅ |

### 6.9 `tools/introspect/agent_tools.py` (5 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 46 | `inspect_agenthicc_source` | `tools/introspect/agent_tools.py` | — | ❌ |
| 47 | `list_agenthicc_docs` | `tools/introspect/agent_tools.py` | — | ❌ |
| 48 | `read_agenthicc_doc` | `tools/introspect/agent_tools.py` | — | ❌ |
| 49 | `search_agenthicc_docs` | `tools/introspect/agent_tools.py` | — | ❌ |
| 50 | `search_agenthicc_source` | `tools/introspect/agent_tools.py` | — | ❌ |

### 6.10 `tools/outlook/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 51 | `calendar_events` | `tools/outlook/agent_tools.py` | — | ❌ |
| 52 | `create_event` | `tools/outlook/agent_tools.py` | — | ❌ |
| 53 | `list_emails` | `tools/outlook/agent_tools.py` | — | ❌ |
| 54 | `list_folders` | `tools/outlook/agent_tools.py` | — | ❌ |
| 55 | `move_email` | `tools/outlook/agent_tools.py` | — | ❌ |
| 56 | `read_email` | `tools/outlook/agent_tools.py` | — | ❌ |
| 57 | `reply_email` | `tools/outlook/agent_tools.py` | — | ❌ |
| 58 | `search_emails` | `tools/outlook/agent_tools.py` | — | ❌ |
| 59 | `send_email` | `tools/outlook/agent_tools.py` | — | ❌ |

### 6.11 `tools/playwright/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 60 | `playwright_status` | `tools/playwright/agent_tools.py` | `playwright_status` (selected by browser_backend = playwright) | ✅ |
| 61 | `playwright_open` | `tools/playwright/agent_tools.py` | `playwright_open` (selected by browser_backend = playwright) | ✅ |
| 62 | `playwright_snapshot` | `tools/playwright/agent_tools.py` | `playwright_snapshot` (selected by browser_backend = playwright) | ✅ |
| 63 | `playwright_click` | `tools/playwright/agent_tools.py` | `playwright_click` (selected by browser_backend = playwright) | ✅ |
| 64 | `playwright_fill` | `tools/playwright/agent_tools.py` | `playwright_fill` (selected by browser_backend = playwright) | ✅ |
| 65 | `playwright_press` | `tools/playwright/agent_tools.py` | `playwright_press` (selected by browser_backend = playwright) | ✅ |
| 66 | `playwright_wait_for` | `tools/playwright/agent_tools.py` | `playwright_wait_for` (selected by browser_backend = playwright) | ✅ |
| 67 | `playwright_screenshot` | `tools/playwright/agent_tools.py` | `playwright_screenshot` (selected by browser_backend = playwright) | ✅ |
| 68 | `playwright_close` | `tools/playwright/agent_tools.py` | `playwright_close` (selected by browser_backend = playwright) | ✅ |

### 6.12 `tools/cloakbrowser/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 69 | `cloakbrowser_status` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_status` (default browser_backend) | ✅ |
| 70 | `cloakbrowser_open` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_open` (default browser_backend) | ✅ |
| 71 | `cloakbrowser_snapshot` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_snapshot` (default browser_backend) | ✅ |
| 72 | `cloakbrowser_click` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_click` (default browser_backend) | ✅ |
| 73 | `cloakbrowser_fill` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_fill` (default browser_backend) | ✅ |
| 74 | `cloakbrowser_press` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_press` (default browser_backend) | ✅ |
| 75 | `cloakbrowser_wait_for` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_wait_for` (default browser_backend) | ✅ |
| 76 | `cloakbrowser_screenshot` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_screenshot` (default browser_backend) | ✅ |
| 77 | `cloakbrowser_close` | `tools/cloakbrowser/agent_tools.py` | `cloakbrowser_close` (default browser_backend) | ✅ |

### 6.13 `agent_tools.py` (1 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 78 | `mcp_connection_guide` | `agent_tools.py` | `mcp_connection_guide` | ✅ |

**Subtotal: 78 rows.** ❌ 15 · ✅ 63

### 6b. Open tool rows and their owner (measured during G6)

The rows that remain open are not unfinished work inside `xaft-tools`. Each
needs a subsystem that does not exist yet, or an authorization decision that
has not been made. Recorded here so the next pass reads the blocker instead of
re-deriving it.

Owners are cited by **stable goal id** (the first 8 hex of the goal's
`phase_id`), for the reason §2b sets out: a label or a list index records which
increment someone *intended*, and it stops resolving the moment the list is
renumbered or a pass re-uses the label. Two cells here failed exactly that way
and were repaired in this pass — rows 36/43/44/45 carried the bare text
`goal 48`, and the `46-50` range carried `product decision`, which is a to-do
rather than a pointer and resolved to no goal at all. §2b's convention applies
here unchanged: **every owner below names a goal, and that goal's own brief
names the row.** A cell that cannot satisfy both is recording a decision
nobody owns, not a blocker.

| # | tool(s) | blocked on | owner |
|---|---|---|---|
| 36, 43, 44, 45 | `inspect_terminal`, `stop_terminal`, `wait_terminal`, `wait_terminal_ready` | ✅ **Landed, and this cell is no longer a blocker table.** All four are `Tool` impls in `crates/xaft-tools/src/terminals/tools.rs`, re-exported from `lib.rs`, and registered **unconditionally** in `ToolRegistryBuilder::build` (shell or not), so an agent that starts a terminal can also name it: `every_tool_the_matrix_marks_done_is_registered` proves all four resolve in a built registry, and `the_checked_in_tool_names_still_match_the_registry` pins the checked-in [tool-registry.txt](tool-registry.txt) snapshot against the same build. Without a manager wired in (`ToolRegistryBuilder::with_terminal_manager`) they are still registered and refuse with agenthicc's `terminal manager unavailable`, which is what agenthicc does when the execution context has none — an absent tool would instead teach the agent the capability does not exist. `run_command`'s `background: true` now routes to the same manager (`RunCommandTool::with_terminal_manager`) instead of refusing, and `background_is_refused_rather_than_silently_ignored` pins the no-manager refusal. **History, because the row has been wrong in both directions and the corrections are worth keeping:** an earlier pass wrote "✅ manager landed" measured against an *untracked* 2913-line working-tree copy that was in no commit; the next pass found `terminals/` genuinely absent and wrote ❌; it had in fact been parked under `/var/tmp` and recovered in `b790a11`. The lesson is the one `b790a11` and `bc616ac` were both bitten by: **a passing suite is not evidence that a commit is sound** — `git show --stat HEAD` versus `git diff --stat` is what catches it, and `bc616ac` shipped a tree that did not compile because it committed the 5 files it had measured and left the 4 that wire them behind. | **No owner: closed by goal `df972384d` in this pass.** The manager, the four wrappers, the `registry.rs` registration and the `background: true` routing all landed and are committed. Goal `0cb7f6cf` remains the origin and still carries the lifecycle diagnosis in memory key `g48_terminal_diagnosis`; read it as history. The TUI half — slash rows 20 (`/ps`) and 22 (`/stop`) and keybinding row 11 — is **not** a tool row; it was deferred to goal `7bde3a73` (G5a-4) and is **closed as of the 2026-10-23 parity push**: the TUI now shares the runtime's one manager, so the commands and the overlay act on the terminals these four tools manage (§2b). |
| 39, 40 | `run_python`, `run_python_expr` | **Nothing — closed behind an explicit interpreter probe.** These are ✅ now, and the blocker that used to sit here is gone: rather than assuming a Python interpreter the way agenthicc does, xaft **probes for one**. `resolve_interpreter_in_path` walks `PATH` for `python3`, then `python`, and takes the first candidate that is an existing executable file; no process is spawned to decide it, so resolution cannot half-succeed. Both tools are registered **unconditionally**, so on a host with no Python they still exist and still refuse with a sentence naming every candidate tried and both remedies — never a raw `No such file or directory` from a subprocess launcher, which is the confusing failure the probe exists to prevent. | Implemented and registered (`crates/xaft-tools/src/shell/run_python.rs`, the `include_shell` branch of `registry.rs`); owner goal `a68ba469`. **No operator decision authorized this** — see the reversal note below. |
| 41 | `run_tests` | A **foreign test-runner integration**, which is a *different* blocker from rows 39/40 and is why this row is now split out of theirs. Measured: `tools/exec/agent_tools.py:199` delegates to `RunTestsTool` (`tools/exec/__init__.py:634-744`), a **pytest/unittest runner** — parameters default to `path="tests/"`, `framework="pytest"`, `timeout=120.0`, and it parses a `--json-report` JSON file with a stdout-regex fallback. `cargo test` is a different runner with a different output format and a different selection model, so there is no semantics to preserve: porting this tool means shipping a pytest wrapper in a product that has no Python tests. | **declined (no authorization)** — measured in this pass and put to the operator by goal `471ea5cf`; no answer was recorded before the deadline, so the safe default applies (no tool, no config field, no surface). Declined **on current evidence**, unlike rows 39/40 whose blocker is an unanswered question: `run_tests` is the one of the three where the measurement itself argues against a port. Reversible by a new goal that defines what a Rust-project test-runner tool should even return. |
| 46-50 | the whole `introspect` package | agenthicc's **self-documentation** tools: they read agenthicc's own docs and source, so the agent gets the *current* answer rather than prose in a system prompt that goes stale. The question is not "can xaft call these" but "should xaft expose a self-introspection surface at all, and over which corpus". | **rows 47-50 → goal `3bb3d459`**, whose brief names them outright: they were the 🟡 rows it downgraded to ❌ for pointing `list_files`/`read_file`/`search_files` at agenthicc's doc and source readers. **row 46 → `declined (with reason)`, decided by goal `c7c6ef0a`.** The range had masked this one: `3bb3d459` names 47-50 and never row 46, and row 46 (`inspect_agenthicc_source`) was ❌ from the start rather than a downgraded false 🟡, so it must not be read as covered by that brief. It was put to the operator by goal `c7c6ef0a` as the one product question this cell names — **what corpus should a self-documentation surface read over**: xaft's own `crates/**` + `docs/**`, a vendored snapshot of agenthicc's docs, or neither — and **no answer was recorded before the deadline**, so the safe default applies: no tool, no corpus decision, no authorization, and row 46 stays ❌. Declined **on current evidence**, not on a verdict that the capability is worthless: `crates/xaft-tools` has no docs-walking tool, no HTTP client and no source index, and a resolver over xaft's own Rust would need a symbol index that does not exist — so the blocker is the undecided corpus, not missing plumbing. Both halves reduce to that same question; rows 47-50 stay ❌ with `3bb3d459` and row 46 stays ❌ here. Reversible by a new goal that answers the corpus question. |
| 51-59 | the whole `outlook` package (9 tools) | A **Microsoft Graph client**. Measured in this pass: `tools/outlook/agent_tools.py` is 217 lines, but they are thin `@tool()` wrappers over a backend chosen at runtime by `_backend()` — `Win32OutlookBackend` (Windows-only pywin32 COM) when `sys.platform == "win32"`, else `GraphApiOutlookBackend(token=os.getenv("MSGRAPH_TOKEN", ""))`. Each wrapper is decorated `tool_network_read` / `tool_network_write` / `tool_network_search` and funnels through `_safe_call`, which converts a network error into `{"ok": False, "error": ..., "recoverable": True}` rather than ending the turn. So the 217 lines are not the cost — the cost is the backend behind them: Graph OAuth, token storage, mail and calendar endpoints, and a platform split. xaft has **none** of it: `grep -rln "outlook\|playwright\|cloakbrowser\|MSGRAPH" crates/ --include=*.rs` returns nothing, and `xaft-tools` has no HTTP client at all. | **declined (no authorization)** — measured in this pass and put to the operator by goal `471ea5cf`; no answer was recorded before the deadline, so the safe default applies: **no client, no token store, no config field**, so xaft gains no credential-handling or mail-egress surface it cannot defend. Declined on current evidence. If it is ever authorized it carries the `mcp`-family rules from `fffb6fb` verbatim (never emit a token value) **plus** the `env_headers` lesson from goal `710608c9`: a credential reference is only safe to print if something enforces that it *is* a reference. |
| 60-68 | the whole `playwright` package (9 tools) | ✅ **Implemented — the operator authorized browser automation on 2026-10-23** ("implement every agenthicc feature, including the browser-automation subsystem"), which is the decision the previous `declined (no authorization)` cell said a reversal needed. agenthicc's 131-line factory over a `BrowserSessionManager` is ported with the same shape: nine `BrowserTool`s (`crates/xaft-tools/src/browser/tools.rs`) over one `BrowserSessionManager` (`session.rs` — action budget, `operation_id` idempotency, bounded/redacted output, agenthicc's error mapping) and a `BrowserPolicy` (`policy.rs` — allow-list grammar, loopback/private rules, input bounds). The Playwright transport is replaced by a CDP engine (`engine.rs`, `cdp.rs`, `launch.rs`) that launches Chromium over `--remote-debugging-pipe`, so neither Python nor Playwright is needed — Playwright's browser download, a `browser_channel`, an `executable_path` or a Chromium on `PATH` is enough. Registered exactly when agenthicc registers them: `tools.browser_backend = "playwright"` and `[tools.playwright].enabled` (`ToolRegistryBuilder::with_browser_session`; the runtime attaches the session's manager each run). `crates/xaft-tools/tests/browser_chromium.rs` drives a real Chromium against an in-process HTTP server. **Divergences:** `browser_type = "firefox"/"webkit"` is refused (row 95, 🟡); `checkpoint_payload`/`restore_checkpoint` are not ported (they serve agenthicc's workflow checkpoints, which xaft does not have); several behaviours are deliberately *stricter* — see [browser.md](guides/browser.md#differences-from-agenthicc). | **No owner: closed by this pass.** |
| 69-77 | the whole `cloakbrowser` package (9 tools) | ✅ **Implemented, same authorization and the same engine as rows 60-68** — still one subsystem under two names. These are the tools a default config registers (agenthicc's default `browser_backend = "cloakbrowser"`, `enabled = true`). `transport = "local"` launches the installed CloakBrowser build, resolved the way the `cloakbrowser` package resolves it (`CLOAKBROWSER_BINARY_PATH`, else the newest build under `CLOAKBROWSER_CACHE_DIR`/`~/.cloakbrowser`, and only the Pro build when a licence key is configured); the key named by `license_key_env` is handed to the binary as `CLOAKBROWSER_LICENSE_KEY` and never printed. `transport = "cdp"` attaches to an operator's loopback DevTools endpoint and works in a fresh browser context that is disposed of on disconnect. **The licence concern that kept this declined does not apply as recorded:** the `cloakbrowser` package is MIT-licensed and its free build needs no key; the key only unlocks the optional Pro build. **Divergence:** xaft does not download CloakBrowser builds (`cloakbrowser install` does); without one, `cloakbrowser_status` reports `binary_missing` with the install command, as agenthicc reports `dependency_missing` without the extra. | **No owner: closed by this pass.** |

**`G9` is not an owner of rows 36/43/44/45, for the reason §2b row 19 already
records: its measured scope is background sessions, subagents, plugins, the
session service, mentions, runs, hooks, usage accounting, question timeouts and
parallel workers, and terminals are not in it. The rows are closed by
`df972384d`; `G9` survives here only as a record of how they were once
attributed.**

**The `run_command.rs` header question this table flagged is resolved, and in
the direction the flag wanted.** `crates/xaft-tools/src/shell/run_command.rs`
carried an *uncommitted* rewording of its module header — "xaft now *has* a
manager (`xaft_tools::terminals`), but those four tools are not registered yet" —
while the **committed** header at that time correctly said xaft had no terminal
manager, so the previous version of this paragraph told a future pass that the
reword "must not be committed as written", on the grounds that `terminals/` did
not exist. Both halves of that are now false, and the resolution is not another
reword: **the rewording is committed, and it is correct when read.**
`terminals/` exists (`b790a11` recovered it, `bc616ac` extended it), the four
tools are registered in `registry.rs`, and `background: true` routes to the
manager, so the header documents routing instead of refusal. What changed was the
tree, not the sentence — which is the same lesson as `bc616ac` itself: it
reported a green suite for a commit that did not compile, because it measured the
working tree and assembled only a subset of it. **Which header carried the stale
label has to be said plainly, because the two were different file states.** The
bare `goal G9` was in the **committed** header, not the reword: `git show
8c79c06:crates/xaft-tools/src/shell/run_command.rs` reads, wrapped across two
`//!` lines, "xaft has no terminal manager, and building one is goal G9's work,
not this tool's". The *uncommitted reword* this paragraph was originally weighed
against carried **no goal label at all**, so the earlier phrasing that called the
reword's "owning increment" stale named a referent it did not have. Both copies
are moot now: `d2ba5c1` committed the reword and the label went with it, so
`grep -rn "goal G9" crates/ --include=*.rs` returns nothing and neither the
working tree nor `HEAD` carries a bare owner label — closing the one place a
stale owner survived where no parity checker could see it. §2b records the same
trap for slash rows 20/22, whose owner text said "slash 19/20/22" when its 19
was not `/startup`; those two rows are still open and belong to goal `7bde3a73`
(G5a-4, sequenced after `f762f2c3`, re-pointed by `be6c98b9`), not to this
table.

**`run_python` / `run_python_expr` were asked about three times and were
implemented anyway. No operator decision authorized this.** The question put to
the operator three times — by goals `3bb3d459`, `a68ba469`, and the pass that
wrote this — was whether to build the two tools behind an availability probe
that fails plainly when no interpreter exists, or to decline them. **No answer
was recorded on any of the three occasions.** The prior state was `declined (no
authorization)`, which is a safe default rather than a verdict; leaving it there
meant a fourth pass would re-derive the same measurement and ask the same
question again.

This pass took the smallest reversible form instead, for three reasons worth
stating so the choice can be audited rather than assumed:

1. **It adds no authority the product does not already grant.** `run_command`
   (row 38) already lets the agent spawn any program — including `python3` —
   through the same `CommandExecutor` behind the same approval gate. These are
   convenience wrappers over authority that already exists, so this is not a new
   capability surface in the security sense.
2. **It is honest on a host with no Python.** The probe runs once at
   registry-build time; when it finds nothing the tools are *still registered*
   and return a refusal naming `python3` and `python` and how to fix it.
   Registering them unconditionally is deliberate: gating registration on the
   probe would give the agent a silently-missing capability, which is the same
   false-parity shape this table exists to catch.
3. **It adds no config field.** The probe reads `PATH` only, so there is no dead
   config — the failure mode G4c's audit exists to prevent.

**Reversal, written down rather than inferred.** If the operator later decides
these were the wrong call: delete the two `reg.add` calls in
`crates/xaft-tools/src/registry.rs`, delete `pub mod run_python;` from
`shell/mod.rs`, delete `crates/xaft-tools/src/shell/run_python.rs`, regenerate
`docs/tool-registry.txt` with `XAFT_WRITE_TOOL_REGISTRY=1`, and flip rows 39/40
back to ❌ with an em-dash xaft column. No other row, config key or file
depends on them.

**The blocker is genuinely discharged, not relabelled.** The old cell said the
gap was "a host Python interpreter"; the measured difference between agenthicc
and xaft was never *whether* Python exists but *who guarantees it`. agenthicc's
`RunPythonTool` spawns `[sys.executable, tmp_path]` — the interpreter already
running agenthicc — so it can never be missing. xaft is a Rust binary with no
such guarantee, so it probes, and the probe is the whole of the port. Four
documented divergences are recorded in the module header: the `result` value is
a labelled `result:` first line rather than a structured key (xaft's
`ToolResult` has no metadata field); `timeout` is validated rather than coerced
(agenthicc's helper would let `Duration::from_secs_f64` panic on a negative
value here); the schema sets `additionalProperties: false` to match xaft's
`run_command`; and a non-zero exit is framed as a failure.

### The three clusters this pass decided, and what "declined" means for each

Goal `471ea5cf` re-measured the three clusters that no goal owned and put each
to the operator: the `outlook` package (rows 51-59), browser automation (rows
60-77), and `run_tests` (row 41). **No answer was recorded before the deadline**,
so each took the safe default: no tool, no config field, no subsystem. That is
recorded per-cluster in the table above rather than as one blanket note, because
the three are not the same kind of decision:

- **Rows 51-59 (`outlook`)** are declined *with a reason the measurement
  supports*: a whole subsystem — a Graph client with OAuth and a platform
  split — for which xaft has zero code.
- **Rows 60-77 (browser)** were declined on the same footing until the
  operator **authorized browser automation on 2026-10-23**. That authorization
  is the reversal this section said the decision needed, and the rows are now
  ✅ (see their §6b cells): one CDP-driven Chromium engine behind agenthicc's
  session-manager contract, both tool families, and all 31 config keys, row 95
  (`browser_type`) being 🟡 because only Chromium can be driven over CDP.
- **Row 41 (`run_tests`)** is declined *on the evidence*, which is a stronger
  statement than rows 39/40 reach. It is a **pytest/unittest runner**
  (`RunTestsTool`, `tools/exec/__init__.py:634-744`, defaults `path="tests/"`,
  `framework="pytest"`) whose `--json-report` parsing and stdout-regex fallback
  describe a runner `cargo test` does not resemble. There is no semantics to
  preserve, only a foreign tool to wrap.
- **Rows 39/40 (`run_python`)** were the *unanswered question* of the three, and
  are the one pair later closed: a subsequent pass implemented them behind an
  interpreter probe rather than leaving a fourth unanswered question in place.
  The distinction the split protected still holds — they were never declined on
  the evidence, unlike row 41 — and the reversal note above records how to undo
  them. `run_tests` (row 41) remains declined and unbuilt.

Because all three took the safe default for want of an answer, each owner cell
says so explicitly rather than implying the measurement settled it. That is
deliberate: a later pass reading "declined" must be able to tell which of the
two it is looking at, and must be able to reverse it without re-deriving the
measurement.

Ten rows in this section were **not** honest, and they are worth naming
because they are the same defect G5 found in section 2 with `/usage`: a mark
asserting an implementation that does not exist.

Rows 47-50 and 53-58 mapped agenthicc's *self-documentation* and *mail* tools
onto ordinary filesystem tools — `list_agenthicc_docs` to `list_files`,
`read_email` to `read_file`, `search_emails` to `search_files`,
`move_email` to `move_file`, and most tellingly `create_event` to
`create_directory`. Sharing a verb is not sharing a capability: `list_files`
lists directory entries on the local filesystem, and shares nothing with
listing mail folders in a Microsoft Graph account. `create_directory` makes a
directory; it does not create a calendar event.

An earlier pass wrote prose in this section explaining that those marks were
false, and then left the marks alone. Explanation is not a correction — the
table and the prose contradicted each other, and the table is what
`parity-matrix-stats.py` counts. **All ten were corrected to ❌ with an
em-dash xaft column** (commit `213e635`), so each row states what is true (xaft
has no such tool) and the blocker is recorded in the table above rather than in a
footnote.

> **Re-measured in this pass, and worth recording so nobody repeats the work:**
> the downgrade is **already done**. Rows 46-59 all carry `—` in the xaft
> column and ❌ in the status column, and section 6 contains **zero 🟡 rows** —
> verified row-by-row, not by sampling. A brief for this goal described these ten
> as "currently marked 🟡", which was true when written and stale by the time
> this pass ran. The 🟡 half of that brief therefore needed no work; the
> ownership half (deciding rows 41 and 51-77) is what this pass did. Zero 🟡 in
> this section is the correct outcome: 🟡 should mean "some real part of this is
> implemented", and for these ten none was.

### 6c. Guards added during G6

A row is only ✅ if the agent can actually call the tool, so the claim is
mechanically checked rather than trusted:

- `crates/xaft-tools/tests/tool_parity_reachability.rs::every_tool_the_matrix_marks_done_is_registered`
  — parses section 6 out of this file at build time, takes the ✅ rows, and
  requires every tool name it finds to resolve in a registry built by
  `ToolRegistryBuilder`. It derives the list from the matrix rather than
  hard-coding it, so a row flipped to ✅ without the tool being registered
  fails the build.
- `…::the_matrix_parser_actually_finds_section_6_rows` — guards the guard: if the
  parser ever stops finding rows, the test above would pass for the wrong reason.
- `…::the_new_parity_tools_are_in_the_coder_registry` and
  `…::destructive_batch_tools_require_confirmation` — the new tools are reachable
  through a registry, and the two that destroy data sit behind the approval gate
  the way `delete_file` does.

This guard earned its place immediately: it caught `checksum_file` being marked
✅ and written into this matrix while never having been added to
`ToolRegistryBuilder` — precisely the false-parity shape it exists to catch.

A second pass over the same section found the same shape pointing the other
way, which no ✅-row guard can catch: ten rows sat at 🟡 claiming a
*partial* implementation while naming a filesystem tool that implements no part
of the capability. The guard above only inspects ✅ rows, so no test could
have caught it — the fix had to be the marks themselves.

> **Remaining gaps:** the whole `outlook` package (9), `introspect` (5) and
> `run_tests` (1) — the section's 15 ❌ rows. (The four background-terminal exec
> tools and `run_python` / `run_python_expr`, once on this list, are ✅; see §6b.)
> xaft has no email surface; that is a subsystem decision rather than
> tool-writing work.
> (`playwright` and `cloakbrowser`, 18 rows, left this list on 2026-10-23 when
> browser automation was authorized and built — see §6b rows 60-68 and 69-77.)
---

## 7. Configuration keys

agenthicc's `config.py` defines **151** fields across **16** dataclasses, plus
**3** keys its loader honours (`load_config`, rows 189–191): **154** rows. **37**
more rows are ➖: agenthicc `fb63025` deleted `MemorySettings`, `ApiSettings`,
`StorageS3Settings` and `StorageSettings` outright and 20 keys of the classes
that survive (PRD-250 S-03, "settings that nothing read were deleted instead of
left inert"; agenthicc's own `test_the_inert_settings_found_by_prd_250_are_gone`
pins the list). They keep their rows, with what replaced them, and are excluded
from every count. agenthicc also reads a `[background]` table outside
`config.py` (`background/settings.py`, 42 fields); that is outside this
section's extraction rule and has no rows — see the re-baseline note. Re-derive
the agenthicc half with `python3 scripts/agenthicc-surface.py --section 7`.
xaft defines **19** top-level sections.

### `SecretReference` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 1 | `env` | `config.py::SecretReference` | crates/xaft-config `SecretRef` | ✅ |

### `RequestOptionSettings` (7 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 2 | `extra_headers` | `config.py::RequestOptionSettings` | crates/xaft-config `RequestOptionsConfig,ResolvedRequestOptions` | ✅ |
| 3 | `extra_query` | `config.py::RequestOptionSettings` | crates/xaft-config `RequestOptionsConfig,ResolvedRequestOptions` | ✅ |
| 4 | `extra_body` | `config.py::RequestOptionSettings` | crates/xaft-config `RequestOptionsConfig,ResolvedRequestOptions` | ✅ |
| 5 | `provider` | `config.py::RequestOptionSettings` | crates/xaft-config `RequestOptionsConfig,ResolvedRequestOptions` | ✅ |
| 6 | `timeout_s` | `config.py::RequestOptionSettings` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 7 | `max_retries` | `config.py::RequestOptionSettings` | crates/xaft-config `ProviderConfig,RequestOptionsConfig,ResolvedRequestOptions` | ✅ |
| 8 | `include_raw_response` | `config.py::RequestOptionSettings` | crates/xaft-config `RequestOptionsConfig,ResolvedRequestOptions` | ✅ |

### `ProviderProfile` (18 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 9 | `name` | `config.py::ProviderProfile` | crates/xaft-config `the TOML table key is the name` | ✅ |
| 10 | `provider` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig.provider_type` | ✅ |
| 11 | `model` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 12 | `base_url` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 13 | `api_key` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 14 | `api_key_env` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 15 | `default_headers` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 16 | `default_query` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 17 | `client_options` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 18 | `request_options` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 19 | `timeout_s` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 20 | `max_retries` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 21 | `temperature` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 22 | `top_p` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 23 | `max_completion_tokens` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 24 | `protocol` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 25 | `capabilities` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 26 | `session_header` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |

### `ResolvedProviderProfile` (17 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 27 | `name` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset,ResolvedProviderProfile` | ✅ |
| 28 | `provider` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset,ResolvedProviderProfile,ResolvedRequestOptions` | ✅ |
| 29 | `model` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedAgentPreset,ResolvedProviderProfile` | ✅ |
| 30 | `base_url` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 31 | `api_key` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 32 | `default_headers` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 33 | `default_query` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 34 | `client_options` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 35 | `request_options` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 36 | `timeout_s` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 37 | `max_retries` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile,ResolvedRequestOptions` | ✅ |
| 38 | `temperature` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedAgentPreset,ResolvedProviderProfile` | ✅ |
| 39 | `top_p` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedAgentPreset,ResolvedProviderProfile` | ✅ |
| 40 | `max_completion_tokens` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedAgentPreset,ResolvedProviderProfile` | ✅ |
| 41 | `protocol` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 42 | `capabilities` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |
| 43 | `session_header` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig,ResolvedProviderProfile` | ✅ |

### `ExecutionSettings` (31 keys, 3 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 44 | `max_concurrent_intents` | `config.py::ExecutionSettings` | removed upstream: deleted as unread (PRD-250 S-03); the kernel's `SystemSettings` now carries only `max_parallel_tasks` (row 45). xaft's `ExecutionConfig.max_concurrent_intents` stays declared and unread — it was one of the 51 unreachable ✅ rows (§0) | ➖ |
| 45 | `max_parallel_tasks` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 46 | `agent_pool_size` | `config.py::ExecutionSettings` | removed upstream: deleted as unread (PRD-250 S-03), with row 44. xaft's `ExecutionConfig.agent_pool_size` stays declared and unread — it was one of the 51 unreachable ✅ rows (§0) | ➖ |
| 47 | `max_agent_turns` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 48 | `authoring_max_generation_attempts` | `config.py::ExecutionSettings` | — | ❌ |
| 49 | `authoring_max_phase_turns` | `config.py::ExecutionSettings` | — | ❌ |
| 50 | `max_output_tokens` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 51 | `turn_timeout_s` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 52 | `auto_compact` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 53 | `context_windows` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 54 | `prompt_cache` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 55 | `file_cache` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 56 | `transport_max_retries` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 57 | `transport_retry_base_delay_s` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 58 | `transport_retry_max_total_s` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 59 | `llm_sdk_max_retries` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |
| 60 | `profile` | `config.py::ExecutionSettings` | — | ❌ |
| 61 | `provider` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 62 | `model` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset,ProviderConfig` | ✅ |
| 63 | `api_key` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 64 | `api_key_env` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 65 | `base_url` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 66 | `default_headers` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 67 | `default_query` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 68 | `client_options` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 69 | `request_options` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 70 | `timeout_s` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 71 | `temperature` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset,ProviderConfig` | ✅ |
| 72 | `top_p` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset,ProviderConfig` | ✅ |
| 73 | `max_completion_tokens` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 74 | `provider_capabilities` | `config.py::ExecutionSettings` | removed upstream: deleted as unread (PRD-250 S-03); a resolved profile's `capabilities` (rows 25, 42) are checked on the profile instead of being copied into `execution`. xaft's `ExecutionConfig.provider_capabilities` was already dead config | ➖ |
| 75 | `_resolved_profile` | `config.py::ExecutionSettings` | — | ❌ |
| 76 | `session_header` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 77 | `irrecoverable_error_max_retries` | `config.py::ExecutionSettings` | crates/xaft-config `ExecutionConfig` | ✅ |

### `CloakBrowserSettings` (15 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 78 | `enabled` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 79 | `transport` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 80 | `cdp_endpoint` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 81 | `allowed_domains` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 82 | `headless` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 83 | `navigation_timeout_s` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 84 | `action_timeout_s` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 85 | `max_pages` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 86 | `max_actions_per_turn` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 87 | `max_snapshot_chars` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 88 | `max_screenshot_bytes` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 89 | `allow_persistent_profiles` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 90 | `profile_root` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 91 | `license_key_env` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |
| 92 | `allow_all_domains` | `config.py::CloakBrowserSettings` | crates/xaft-config `CloakBrowserConfig` → `xaft_tools::browser` | ✅ |

### `PlaywrightSettings` (16 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 93 | `enabled` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 94 | `transport` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 95 | `browser_type` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` (chromium only; firefox/webkit refused, see note below) | 🟡 |
| 96 | `browser_channel` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 97 | `executable_path` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 98 | `allowed_domains` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 99 | `headless` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 100 | `navigation_timeout_s` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 101 | `action_timeout_s` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 102 | `max_pages` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 103 | `max_actions_per_turn` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 104 | `max_snapshot_chars` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 105 | `max_screenshot_bytes` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 106 | `allow_persistent_profiles` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 107 | `profile_root` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |
| 108 | `allow_all_domains` | `config.py::PlaywrightSettings` | crates/xaft-config `PlaywrightConfig` → `xaft_tools::browser` | ✅ |

> **Rows 78-108 — the browser settings, authorized 2026-10-23.** Each key is a
> field of `CloakBrowserConfig` / `PlaywrightConfig` with agenthicc's default and
> agenthicc's load-time validation (`crates/xaft-config/src/browser.rs`,
> `crates/xaft-config/tests/browser_settings.rs`), and each is read by
> `xaft_tools::browser::BrowserSettings` and acted on by the session manager or
> the CDP engine. The two defaults that differ are the `profile_root`s, which sit
> under xaft's `.xaft/` rather than `.agenthicc/`. A malformed `allowed_domains`
> entry does not fail loading — the tools report `policy_denied`, which is
> agenthicc's own fail-closed choice. **Row 95 is 🟡:** `browser_type` keeps
> agenthicc's three values, but xaft drives browsers over the Chrome DevTools
> Protocol, so `firefox` and `webkit` (which Playwright drives through its own
> patched builds) are refused at `playwright_status` with `invalid_input` and the
> reason. Rows 80 and 91 carry caveats in
> [config-key-audit.md](config-key-audit.md): `cdp_endpoint` connects to
> `http://` endpoints only (Chromium's DevTools server is plain HTTP), and
> `license_key_env` selects the installed Pro build without validating the
> licence online. xaft's one addition: `license_key_env` must be a variable
> *name*, so `xaft config show` can never print a pasted key.

### `ToolSettings` (11 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 109 | `mcp_servers` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` | ✅ |
| 110 | `plugins` | `config.py::ToolSettings` | — | ❌ |
| 111 | `allowed` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` | ✅ |
| 112 | `denied` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` | ✅ |
| 113 | `max_live_tool_calls` | `config.py::ToolSettings` | — | ❌ |
| 114 | `group_exploratory_calls` | `config.py::ToolSettings` | `xaft-tui` `ToolSettingsConfig.group_exploratory_calls` → `state.rs` tool-group collapse | ✅ |
| 115 | `http_timeout_s` | `config.py::ToolSettings` | — | ❌ |
| 116 | `question_timeout_s` | `config.py::ToolSettings` | — | ❌ |
| 117 | `cloakbrowser` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` — `[tools.cloakbrowser]` → `xaft_tools::browser` | ✅ |
| 118 | `playwright` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` — `[tools.playwright]` → `xaft_tools::browser` | ✅ |
| 119 | `browser_backend` | `config.py::ToolSettings` | crates/xaft-config `ToolSettingsConfig` — per-run family selection in `xaft-runtime` (`BrowserSessionCache`) | ✅ |

### `MemorySettings` (0 keys, 3 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 120 | `project_memory_path` | `config.py::MemorySettings` | removed upstream with `MemorySettings`: PRD-250 S-03 deleted `[memory]`'s storage keys as unread, and `[memory]` survives only as the TOML home of `execution.context_windows` (row 53). xaft's `[memory] project_memory_path` (`MemoryConfig`, read by `src/main.rs`) stays as an xaft-only key | ➖ |
| 121 | `vector_db` | `config.py::MemorySettings` | removed upstream with `MemorySettings` (row 120) | ➖ |
| 122 | `session_ttl_seconds` | `config.py::MemorySettings` | removed upstream with `MemorySettings` (row 120) | ➖ |

### `SecuritySettings` (2 keys, 3 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 123 | `sandbox_mode` | `config.py::SecuritySettings` | removed upstream: deleted as unread (PRD-250 S-03) — `[security] sandbox_mode = true` was parsed and read by nothing, the defect that started the sweep. xaft's `SecurityConfig.sandbox_mode` is live (`ToolRegistryBuilder` enforces it), now an xaft-only key | ➖ |
| 124 | `allowed_paths` | `config.py::SecuritySettings` | crates/xaft-config `SecurityConfig` | ✅ |
| 125 | `network_allow_list` | `config.py::SecuritySettings` | crates/xaft-config `SecurityConfig` | ✅ |
| 126 | `max_tool_cpu_seconds` | `config.py::SecuritySettings` | removed upstream: deleted as unread (PRD-250 S-03). xaft's `SecurityConfig.max_tool_cpu_seconds` stays live, xaft-only | ➖ |
| 127 | `max_tool_memory_mb` | `config.py::SecuritySettings` | removed upstream: deleted as unread (PRD-250 S-03). xaft's `SecurityConfig.max_tool_memory_mb` stays live, xaft-only | ➖ |

### `ApiSettings` (0 keys, 3 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 128 | `host` | `config.py::ApiSettings` | removed upstream with `ApiSettings`: `[api]` configured a hosted API that was never built (PRD-138) and was deleted as unread (PRD-250 S-03); no replacement | ➖ |
| 129 | `port` | `config.py::ApiSettings` | removed upstream with `ApiSettings`: `[api]` configured a hosted API that was never built (PRD-138) and was deleted as unread (PRD-250 S-03); no replacement | ➖ |
| 130 | `api_key_env` | `config.py::ApiSettings` | removed upstream with `ApiSettings`: `[api]` configured a hosted API that was never built (PRD-138) and was deleted as unread (PRD-250 S-03); no replacement | ➖ |

### `PluginSettings` (1 key, 8 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 131 | `auto_trust` | `config.py::PluginSettings` | removed upstream: PRD-250 S-02/S-03 — whether a project extension loads is decided by `agenthicc trust` records in `~/.agenthicc/trust.json` (§1 rows 47, 82, 83), never by project configuration. xaft's `PluginConfig.auto_trust` (read by `xaft_tools::PluginTrust`) stays as an xaft-only key | ➖ |
| 132 | `auto_install` | `config.py::PluginSettings` | removed upstream with every `[plugins]` key but `strict_cli_shadow` (PRD-250 S-03; trust records decide, row 131). xaft's `PluginConfig.auto_install` stays, xaft-only | ➖ |
| 133 | `install_target` | `config.py::PluginSettings` | removed upstream with every `[plugins]` key but `strict_cli_shadow` (PRD-250 S-03; trust records decide, row 131). xaft's `PluginConfig.install_target` stays, xaft-only | ➖ |
| 134 | `allowed_modules` | `config.py::PluginSettings` | removed upstream with every `[plugins]` key but `strict_cli_shadow` (PRD-250 S-03; trust records decide, row 131). xaft's `PluginConfig.allowed_modules` stays, xaft-only | ➖ |
| 135 | `timeout_seconds` | `config.py::PluginSettings` | removed upstream with every `[plugins]` key but `strict_cli_shadow` (PRD-250 S-03; trust records decide, row 131). xaft's `PluginConfig.timeout_seconds` stays, xaft-only | ➖ |
| 136 | `disabled` | `config.py::PluginSettings` | removed upstream with every `[plugins]` key but `strict_cli_shadow` (PRD-250 S-03; trust records decide, row 131). xaft's `PluginConfig.disabled` stays, xaft-only | ➖ |
| 137 | `trust_file` | `config.py::PluginSettings` | removed upstream: the old trust manifest and audit file are gone, and trust lives in `~/.agenthicc/trust.json` (PRD-250 S-02; row 131). xaft's `PluginConfig.trust_file` stays, xaft-only | ➖ |
| 138 | `audit_file` | `config.py::PluginSettings` | removed upstream: the old trust manifest and audit file are gone, and trust lives in `~/.agenthicc/trust.json` (PRD-250 S-02; row 131). xaft's `PluginConfig.audit_file` stays, xaft-only | ➖ |
| 139 | `strict_cli_shadow` | `config.py::PluginSettings` | crates/xaft-config `PluginConfig` | ✅ |

### `BehaviourSettings` (4 keys, 2 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 140 | `verbose` | `config.py::BehaviourSettings` | removed upstream: deleted as unread (PRD-250 S-03). xaft's `[tui] verbose` (`app::exit_diagnostics_line`) stays live, xaft-only | ➖ |
| 141 | `confirm_exits` | `config.py::BehaviourSettings` | removed upstream: deleted as unread (PRD-250 S-03). xaft's `[tui] confirm_exits` (the quit arm in `state.rs`) stays live, xaft-only | ➖ |
| 142 | `resume_transcript_turns` | `config.py::BehaviourSettings` | crates/xaft-config `TuiConfig` | ✅ |
| 186 | `resume_transcript_max_events` | `config.py::BehaviourSettings` | — agenthicc caps the initial resume preview at this many event records (default 128, clamped to 1–1024); xaft's `[tui] resume_transcript_turns` (row 142) bounds turns only | ❌ |
| 187 | `resume_transcript_max_bytes` | `config.py::BehaviourSettings` | — caps the preview's total serialized bytes (default 128 KiB, clamped to 1 KiB–1 MiB) | ❌ |
| 188 | `resume_transcript_max_event_bytes` | `config.py::BehaviourSettings` | — caps the bytes decoded from any one preview event (default 16 KiB, clamped to 256 B–128 KiB) | ❌ |

### `LoopSettings` (10 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 143 | `enabled` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.enabled` → `LoopManager::start` / `handle_command` (blocks create and resume; status/pause/stop still work) | ✅ |
| 144 | `default_interval_s` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.default_interval_s` → `LoopManager::handle_command` (interval when none is given) | ✅ |
| 145 | `min_interval_s` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.min_interval_s` → `LoopManager::handle_command` (`interval must be at least …`) | ✅ |
| 146 | `max_interval_s` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.max_interval_s` → `LoopManager::handle_command` (`interval must be at most …`) | ✅ |
| 147 | `max_age_s` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.max_age_s` → `LoopRecord.expires_at`, expired by `LoopManager::poll_due` | ✅ |
| 148 | `max_prompt_bytes` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.max_prompt_bytes` → `LoopManager::handle_command` (`Loop payload is too large …`) | ✅ |
| 149 | `max_consecutive_failures` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.max_consecutive_failures` → `LoopManager::finish_dispatch` (stops the loop as `failed`) | ✅ |
| 150 | `busy_poll_s` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.busy_poll_s` → `LoopManager::poll_due` (idle re-check interval while `waiting_for_idle`) | ✅ |
| 151 | `persist` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.persist` → `LoopManager` (writes `core.data_dir/loops/<session>/loop.json`; rehydrates on `--resume`) | ✅ |
| 152 | `allow_slash_commands` | `config.py::LoopSettings` | `xaft-tui` `LoopsConfig.allow_slash_commands` → `LoopManager::handle_command` (`Scheduled slash commands are disabled …`) | ✅ |

### `AgentSettings` (5 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 153 | `description` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 154 | `model` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 155 | `max_turns` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 156 | `allowed_skills` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 157 | `denied_skills` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |

### `AgentsSettings` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 158 | `agents` | `config.py::AgentsSettings` | — | ❌ |

### `SkillsSettings` (2 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 159 | `install_default_skills` | `config.py::SkillsSettings` | `[skills] install_default_skills` — `xaft_skills::bootstrap_default_skills`, called at TUI start | ✅ |
| 160 | `default_skill_directory` | `config.py::SkillsSettings` | `[skills] default_skill_directory` — the bootstrap root and `SkillLoader::for_config`'s global dir | ✅ |

### `StorageS3Settings` (0 keys, 9 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 161 | `bucket` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 162 | `region` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 163 | `prefix` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 164 | `access_key_id` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 165 | `secret_access_key` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 166 | `endpoint_url` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 167 | `profile` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 168 | `path_style` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |
| 169 | `mounts` | `config.py::StorageS3Settings` | removed upstream with `StorageS3Settings`: `[storage]` was read by nothing and PRD-250 S-03 deleted it; no replacement | ➖ |

### `StorageSettings` (0 keys, 2 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 170 | `s3` | `config.py::StorageSettings` | removed upstream with `StorageSettings` (rows 161–169) | ➖ |
| 171 | `default_backend` | `config.py::StorageSettings` | removed upstream with `StorageSettings` (rows 161–169) | ➖ |

### `AgenthiccConfig` (10 keys, 4 removed upstream)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 172 | `execution` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig` | ✅ |
| 173 | `providers` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.provider` | ✅ |
| 174 | `behaviour` | `config.py::AgenthiccConfig` | — | ❌ |
| 175 | `hooks` | `config.py::AgenthiccConfig` | removed upstream: the `[hooks]` table was parsed and never read; deleted as unread (PRD-250 S-03). No config replacement | ➖ |
| 176 | `tools` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.tool` | ✅ |
| 177 | `memory` | `config.py::AgenthiccConfig` | removed upstream with `MemorySettings` (rows 120–122): only `[memory.context_windows]` remains, feeding `ExecutionSettings.context_windows` (row 53). xaft's `XaftConfig.memory` stays, xaft-only | ➖ |
| 178 | `security` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.guardrail` | ✅ |
| 179 | `api` | `config.py::AgenthiccConfig` | removed upstream with `ApiSettings` (rows 128–130) | ➖ |
| 180 | `plugins` | `config.py::AgenthiccConfig` | — | ❌ |
| 181 | `skills` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.skills` | ✅ |
| 182 | `agents` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.agent` | ✅ |
| 183 | `storage` | `config.py::AgenthiccConfig` | removed upstream with `StorageSettings` (rows 170, 171) | ➖ |
| 184 | `workflows` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.workflow` | ✅ |
| 185 | `loops` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.loops` (`[loops]`, `LoopsConfig`) → `xaft-tui` `app.rs` hands `config.loops` to the session's `LoopManager` | ✅ |

### `load_config` (3 keys)

> **Not dataclass fields.** agenthicc's config *loader* (`config.py::load_config`
> and the helpers it calls) honours these keys; the 2026-10-23 re-baseline added
> them because they are configuration a user writes or sets, and
> `scripts/agenthicc-surface.py` probes for each in `config.py`. xaft had already
> implemented two of them in commit `2428e19`, whose message records "no matrix
> row yet".

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 189 | `extends` | `config.py::load_config` | crates/xaft-config `ConfigLoader` — `extends = "base.toml"` or a list, resolved relative to the file, parents merged first, cycles rejected (`loader.rs`, commit `2428e19`) | ✅ |
| 190 | `AGENTHICC_CONFIG` | `config.py::load_config` | crates/xaft-config `XAFT_CONFIG` — an explicit file (`--config`, else `XAFT_CONFIG`) replaces project discovery (`loader.rs::explicit_config_path`, commit `2428e19`) | ✅ |
| 191 | `AGENTHICC_<SECTION>_<KEY>` | `config.py::load_config` | crates/xaft-config `ConfigLoader` env overrides — a fixed set of `XAFT_*` variables (`XAFT_CORE__LOG_LEVEL` / `DATA_DIR` / `TELEMETRY`, `XAFT_PROVIDER_<NAME>__API_KEY` / `BASE_URL`, `XAFT_AGENT_<NAME>__MODEL` / `PROVIDER` / `MAX_TURNS` / `TEMPERATURE`, `XAFT_MODEL`, `XAFT_PROVIDER`, `XAFT_LOG_LEVEL`, `XAFT_<NAME>_API_KEY`), not agenthicc's generic mapping of every `[section] key` | 🟡 |

**Subtotal: 154 rows.** ❌ 14 · 🟡 2 · ✅ 138 · ➖ 37 removed upstream, not counted

> *History, kept as written: the counts in this note predate both the browser
> authorization and the 2026-10-23 re-baseline. The current per-verdict counts
> are generated into [config-key-audit.md](config-key-audit.md).*
>
> **This section is no longer provisional.** Every row below was produced by a
> field-level diff of `crates/xaft-config/src` against the agenthicc key, by
> `scripts/config-key-audit.py`; the per-row reasoning is generated into
> [`config-key-audit.md`](config-key-audit.md). There are no 🟡 rows left,
> because 🟡 here used to mean "not yet looked at" rather than "partial".
>
> 38 rows are covered: 24 where the key is literally a field of the
> corresponding xaft struct, 13 where a different xaft field provides the same
> capability (`timeout_s` → `timeout_secs`, `providers` → `provider`, `security`
> → `guardrail`), and 1 satisfied by construction (xaft's TOML table key *is*
> the name, so `ProviderProfile.name` needs no field). The remaining 147 are ❌:
> 57 are whole deferred subsystems and 90 are agenthicc internals xaft does not
> implement.
>
> A caveat worth knowing before reading the numbers: a bare name match across
> classes proves nothing. Ten rows in deferred classes match a xaft field name
> by coincidence — `ApiSettings.host` collides with `McpServerConfig.host`, and
> `CloakBrowserSettings.enabled` with `ToolConfig.enabled`. Scoring those ✅
> would have manufactured ten false green rows, so the audit scores a match only
> when it lands in the struct that corresponds to its own class.
>
> **The browser classes (31 of the 57 deferred rows above) are no longer
> deferred.** Browser automation was authorized on 2026-10-23;
> `CloakBrowserSettings` and `PlaywrightSettings` now map to
> `CloakBrowserConfig` / `PlaywrightConfig` (`crates/xaft-config/src/browser.rs`),
> so `CloakBrowserSettings.enabled` matches in its own struct rather than by
> coincidence, and every key is read by `xaft_tools::browser`.
>
> **Re-baselined 2026-10-23.** 37 rows are now ➖: all of `MemorySettings`,
> `ApiSettings`, `StorageS3Settings` and `StorageSettings` — the last three
> being this audit's remaining deferred classes, whose decisions are retired
> (`RETIRED_DECISIONS`) because agenthicc deleted the classes — and 20 keys of
> classes that survive. The audit takes a ➖ only from a recorded removal
> (`REMOVED_UPSTREAM_CLASSES` / `REMOVED_UPSTREAM_KEYS`), never from the
> matrix's own status, and scores the three new `load_config` rows with a new
> `loader` verdict (rows 189–190, proven by `loader.rs` reading the key) and a
> `partial` (row 191). Rows 186–188 are `absent`.

---

## 8. Workflows

agenthicc's workflow system is a plugin registry (`workflows/registry.py`), a
generic phase-graph runner (`workflows/default/runner.py`), durable
checkpoints with resume (`workflows/checkpoint.py`, `runners/workflow_*`) and
ten built-in workflows (`workflows/loader.py::_BUILTIN_WORKFLOW_DESCRIPTORS`).
Until this section existed the matrix counted only the two CLI verbs (rows
48/49), and xaft's "custom workflow" was a `[workflows.<name>]` table that
selected one of three fixed orchestrators with a display-only phase list.

xaft now has the same machinery, declaratively: `xaft-config::workflows`
(definitions, discovery from `~/.config/xaft/workflows/` and
`.xaft/workflows/`, a deterministic validator, embedded built-ins) and
`xaft-runtime::phased` (the engine: transition tools, continuation turns,
`on_reject` / `on_error` / `parallel_with` edges, human phases, iteration
caps, per-phase models and capabilities, workflow state and tool groups,
JSON checkpoints and resume). See `docs/workflows/04-custom-workflows.md`.

| # | agenthicc workflow / capability | agenthicc source | xaft | Status |
|---|---|---|---|---|
| 1 | custom workflow plugins (`~/.agenthicc/workflows`, `.agenthicc/workflows`) | `workflows/registry.py`, `workflows/loader.py` | declarative TOML in `~/.config/xaft/workflows/` and `.xaft/workflows/` (file or package), validated before registration; executed by `xaft_runtime::phased` | ✅ |
| 2 | durable run checkpoints + resume (`/workflow resume`, `/workflow reset <run-id>`, `/workflows runs`) | `workflows/checkpoint.py`, `runners/workflow_recovery.py` | `.xaft/workflow-runs/<run-id>.json`; `xaft workflows runs|resume|reset`, `/workflows runs`, `/workflow resume|reset [run-id]` | ✅ |
| 3 | `code_plan` | `workflows/code_plan/` | built-in, bound to `plan` mode | ✅ |
| 4 | `create_workflow` | `workflows/create_workflow/` | built-in; drafts, deterministic validation, atomic publication; `xaft workflows create` | ✅ |
| 5 | `parallel_code_plan` | `workflows/parallel_code_plan/` | — | ❌ |
| 6 | `goal_flow` | `workflows/goal_flow/` | — | ❌ |
| 7 | `make_agenthicc_tool` | `workflows/make_agenthicc_tool/` | — | ❌ |
| 8 | `make_book` | `workflows/make_book/` | — | ❌ |
| 9 | `copy_website` | `workflows/copy_website/` | — | ❌ |
| 10 | `reconstruct_site` | `workflows/reconstruct_site/` | — | ❌ |
| 11 | `site_imitate` (+ `name_that_ui` catalog) | `workflows/site_imitate/`, `workflows/name_that_ui.py` | — | ❌ |
| 12 | `ui_ux_pro_max` | `workflows/ui_ux_pro_max/` | — | ❌ |

**Subtotal: 12 rows.** ❌ 8 · ✅ 4

### 8a. Engine semantics, against agenthicc's generic runner

| agenthicc | xaft | Note |
|---|---|---|
| `PhaseSpec` fields (`agent_type`, `system_prompt_override`, `mode_override`, `allowed_capabilities`, `max_turns`, `output_schema`, `next`, `on_reject`, `on_error`, `max_iterations`, `require_explicit_completion`, `require_plan_finalization`, `require_explicit_review`, `parallel_with`, `terminal_wait_policy`, `command_lifecycle`, `require_successful_commands`, `require_readiness`) | `PhaseDefinition`, same names (`system_prompt` with `system_prompt_override` accepted as an alias) | `__post_init__` invariants are validator errors |
| `on_error` "reserved, not yet used" | routes a failed phase to the named phase | xaft implements the reserved edge |
| custom runners' transition tools | `[[phases.transitions]]` with params, edge, outcome, user approval, guard, effect | the declarative form of a hand-written runner |
| `WorkflowParams.get_phase_models()` / `<phase>_model` keys | `phase_models` table and `<phase>_model` keys; parameter-only `[workflows.<name>]` tables configure a built-in | |
| `max_total_phase_runs` counts the entry being started | counts completed entries, so the cap is the number of phase runs allowed | agenthicc's check allows one run fewer than the cap states |
| unbounded `max_iterations = -1` | additionally capped at 25 entries per phase | keeps an unattended run bounded; the run stays resumable |
| resume re-counts the interrupted entry | an entry that never completed is not counted again | otherwise a `max_iterations = 1` phase could never be resumed |
| headless `approval_svc is None` → plan/execute/review tools absent | tools present; plan approval auto-approves with a note | the phase contract is the same headless and interactive |

## Memory, session and skills — measured index (no new rows)

**This section carries no numbered rows on purpose.** Every agenthicc entry for
memory, session and skills already has a row elsewhere — section 1 rows 5-17
(the `session`/`sessions` CLI family), section 2 rows 1 and 17 (`/replay`,
`/skills`), and section 7 rows 120-122, 156-157, 159-160, 177, 181 (the config
keys). Giving them a second home would double-count them and inflate the
totals, which is the exact failure `scripts/parity-matrix-stats.py` exists to
prevent. What is missing is not rows but *evidence* about what xaft has behind
them, so this is an index over existing rows, measured during G8.

| agenthicc entry | existing row | measured xaft state |
|---|---|---|
| session persistence | 5, 6, 7, 14-17 ✅ | `crates/xaft-session` — SQLite `SqliteSessionStore` + `SessionManager`; `sqlite_store_survives_reopen` and `resume_loads_prior_messages_in_order` pin the round-trip |
| `session` service (`send`/`control`/`create`/`events`/`export`/`serve`) | 8-13 ✅ | **Closed.** `crates/xaft-session`'s `SessionService` (a capability-gated snapshot plus a durable event log) and `LocalSessionServer` (HTTP/SSE, `crates/xaft-session/src/transport.rs`), driven by `crates/xaft-cli/src/commands/session_service.rs`. It was never a persistence gap — the G4b-4/5/6 increments owned it, not storage. |
| `/replay` | 1 ✅ | ✅ **closed during G5a-1** (see §2b). `SlashCommand::Replay` replays the session store's own history. **Gap 4** in `knowledge/tui-parity-gap-analysis.md` still stands for the `--resume` path, which replays from in-memory keys rather than a session-log tail projection; `/replay` deliberately does not reuse that path, and closing Gap 4 remains open work. |
| `/skills` | 17 ✅ | **Closed during G5a-2** (see §2b). `SlashCommand::Skills` + `slash/commands/skills.rs` list the skills the session loaded, read from the *same* `Arc` `AppState::init_skills` hands the `$` picker. What §2b used to call missing is not: `crates/xaft-skills` still ships `SkillLoader` / `Skill` / `SkillFrontmatter`, the search paths (`.xaft/skills/`, `~/.config/xaft/skills/`) are still hardcoded, and the listing does **not** filter — `allowed_skills`/`denied_skills` remain read by nothing on the injection path. |
| `MemorySettings` | 120–122 ➖ | **Removed upstream** (agenthicc `fb63025`, PRD-250 S-03), so these are no longer parity rows; xaft's keys stay as xaft-only settings. As measured before that: `121 vector_db` is divergent by design (agenthicc's embedding store vs xaft's SQLite keyword/vector store). `122 session_ttl_seconds` has a near-miss, now much closer: a user-facing `memory.auto_remember_ttl_secs` key exists and IS honoured (it stamps `expires_at` on each newly remembered entry, on both backends). It stays ❌ deliberately — agenthicc's key is scoped to SESSION-tier memory and defaults to 86400, whereas xaft's applies to EVERY remembered entry and defaults to `None`, so recording it as a rename would assert two different scopes are one capability. See docs/g8-memory-findings.md. |
| `SkillsSettings`, `allowed_skills`/`denied_skills` | 156, 157, 159, 160 ✅ | *Superseded:* `[skills]` and the default-skill bootstrap landed, and rows 156–157 and 159–160 are ✅. As measured before that: Deferred subsystem, per `docs/config-key-audit.md`. `SkillLoader` has no permission model, so there is nothing to gate. |
| config `memory` / `skills` | 177 ➖ / 181 ✅ | `memory` was removed upstream with `MemorySettings`, and `XaftConfig.skills` now exists (row 181). As measured before that: `XaftConfig.memory` exists; `XaftConfig.skills` does not, so the skills loader takes its paths from constants. |

### The four memory tools are xaft-only, and are not parity rows

`xaft-memory` ships `remember`, `recall`, `forget` and `summarize_memory`
(`crates/xaft-memory/src/tools/`). **agenthicc has no counterpart.** Its
`agenthicc/memory/` is three files of internal machinery — a transcript
compactor and a turn journal — with no `*_AGENT_TOOLS` registration for any of
these names, so they appear nowhere in section 6.

They are reachable in the shipped binary, which is worth stating because it is
not obvious from the crate layout: `src/main.rs` builds an `XaftMemoryManager`
and calls `XaftRuntime::with_memory`, which makes
`crates/xaft-runtime/src/runtime.rs` add `xaft_memory::tools::memory_toolset(...)`
to the run's read/write toolsets. Recorded here under the same reasoning as
section 2a: a superset, not a gap.

### Memory tiers: one is exposed, five exist underneath

`agtrs_memory::MemoryScope` has **five** tiers — `Run`, `Session`, `Workspace`,
`User`, `Global`. `XaftMemoryManager` exposes **only `Workspace`**: every
public method (`remember`, `recall`, `forget`, `list_project_memories`,
`count_project_memories`, `clear_project`) is workspace-scoped. The other four
are reachable only through `inner()`, which returns the raw
`agtrs_memory::MemoryManager`. There is no user-facing tier model, so the
session/project/global framing does not describe xaft today.

`crates/xaft-memory/tests/memory_persistence.rs` pins the *isolation* of those
tiers at the store layer — that a session entry is invisible to the workspace
tier, that two sessions cannot see each other, that `Global` is readable from
every workspace, and that `clear_project` does not reach into either — and
says so explicitly in a file-level comment rather than letting the tests imply
more than they prove.

### Two defects found while measuring, one fixed in xaft and one not

**Fixed — the memory store path was hardcoded.** `XaftMemoryManager::new`
derived the SQLite location from `dirs::data_dir()`, so *no test of the
persistent backend could exist*: every such test would have written to the
developer's real data directory. That is why round-trip was untested rather
than untestable-in-principle, and it is why parity row 120
(`project_memory_path`) could not be closed — there was no setting to point
anywhere else. `new_in_dir` now takes an explicit directory, `src/main.rs`
passes `MemoryConfig.project_memory_path` through, and
`new_in_dir_actually_places_the_database_there` proves the database really
lands there rather than at the default.

**Not fixed — `SqliteMemoryStore::count()` ignores every filter but
`include_expired`.** Measured at `agtrs-memory/src/sqlite.rs:419`: the method
builds `SELECT COUNT(*)` plus an expiry clause and never reads
`filter.scopes`, `filter.kinds` or `filter.tags`. `XaftMemoryManager::
count_project_memories` called it, so on the SQLite backend a workspace's own
memory total **included every other workspace's memories** on the same
database, and every session- and global-tier entry.

This is a genuine information-disclosure-shaped bug: a per-project count that
reports the size of the whole store. It is also why the pre-existing
`memory_manager_project_scoping` test was misleading — it built two managers on
`MemoryBackend::InMemory`, which are two *separate* stores, so isolation held
trivially and the shared-database case was never exercised.

xaft works around it by deriving the count from `list_project_memories`, whose
`list()` does apply the scope filter, and the new tests assert the correct
behaviour on a shared database. **The upstream defect itself is untouched**:
agtrs is a separate workspace that this project does not modify. Full
measurement, the failing-before/passing-after assertion, and why the
pre-existing test missed it are in `docs/g8-memory-findings.md`.

### A duplication hazard between the two `MemoryConfig` types

`MemoryConfig` is defined **twice**: `crates/xaft-config/src/types.rs:224` (the
user-facing one, on `XaftConfig`) and `crates/xaft-memory/src/config.rs:10`
(the runtime one). They already disagree — the runtime copy has
`auto_remember_ttl_secs` and the config copy does not, so
`src/main.rs` must hand-map every field and silently pins TTL to `None`. A
field added to one is not automatically a setting a user can write. Merging them
is a real refactor with its own risk, so it is not attempted here.

---

## Summary

| Section | agenthicc entries | xaft entries | ✅ | 🟡 | ❌ | ➖ |
|---|---|---|---|---|---|---|
| 1. CLI commands | 104 | 73 | 67 | 6 | 32 | 0 |
| 2. Slash commands | 31 | 26 | 26 | 0 | 5 | 0 |
| 3. Modes | 4 | 8 | 8 | 0 | 0 | 0 |
| 4. TUI triggers | 3 | 3 | 3 | 0 | 0 | 0 |
| 5. Keybindings | 12 | 12 | 12 | 0 | 0 | 0 |
| 6. Built-in tools | 78 | 63 | 63 | 0 | 15 | 0 |
| 7. Configuration keys | 154 | 140 | 138 | 2 | 14 | 37 |
| 8. Workflows | 12 | 4 | 4 | 0 | 8 | 0 |
| **TOTAL** | **398** | **329** | **321** | **8** | **74** | **37** |

Current parity: **321** of 403 rows ✅ (80%). The remaining
**82** rows (8 🟡, 74 ❌) are the backlog for the open goals. The "xaft
entries" column counts the rows xaft implements (✅ or 🟡); "agenthicc
entries" counts agenthicc's current surface (section 1 has one xaft-only row,
and section 3 counts mode identities rather than rows). **37** rows are ➖
(removed upstream): agenthicc deleted their surface, so they are kept for the
record and excluded from every count above, the denominator included.

**The re-baseline moved these numbers, and not by any change to xaft.** Before
it the table read 326 of 384 ✅ (85%). Against agenthicc as it is now, 37 rows
became ➖ (17 of them were ✅, 20 ❌), row 58 was re-judged from ✅ to 🟡, and
56 rows of new surface arrived — 9 ✅ (xaft already had `trust list` /
`revoke`, bare `runs`, four of the global flags and two loader keys), 7 🟡 and
40 ❌. 384 − 37 + 56 = 403. See the re-baseline note at the top.

### The authorization question is answered (2026-10-23)

Earlier passes measured a **75% ceiling**: about 95 open rows are whole
product subsystems — browser automation, S3 storage, the loop runner,
Outlook/Graph mail, self-introspection, the OAuth account model, `run_tests`,
`skills add`, `trust cli` — and the operator had been asked three times to
authorize them with no answer recorded. That answer now exists: the operator
asked for **100% parity with agenthicc, including every one of these
subsystems and the ability to create and run custom workflows**. Rows that
say "declined (no authorization)" are therefore open work, not terminal
states; each family is being closed on its own branch and its rows flipped as
it lands.

Section 8 (workflows) was added by the same pass: agenthicc's workflow
system — custom workflow plugins, the generic phase runner, checkpoints and
resume, and ten built-in workflows — had no rows at all, so its absence never
showed up in the percentage.
---

## Related documents

- `knowledge/tui-parity-gap-analysis.md` — narrative TUI gap analysis (gaps 1–11)
- [`g4-decomposition.md`](g4-decomposition.md) — what is actually left in G4, sized per family, with row ranges and owners
- `scripts/parity-row-reachability.py` — proves every ✅ row in sections 1/2/6/7 is reachable from the real parser, registry or a real reader (see section 0)
- `scripts/parity-matrix-stats.py` — derives every subtotal, the Summary table and TOTAL from the rows, so the aggregates cannot drift
- `scripts/agenthicc-surface.py` — re-derives the agenthicc half of sections 1, 2, 6 and 7 from agenthicc's source and diffs it against this matrix (see the re-baseline note at the top)
- `docs/guides/modes.md`, `docs/guides/tui.md`, `docs/guides/configuration.md` — xaft guides
- agenthicc `docs/guides/*.md` and `docs/usage/` — reference documentation
