# Parity Matrix — agenthicc → xaft

Tracks **user-visible surface parity** between the Python reference implementation
`agenthicc` (`~/python_projects/agenthicc`) and the Rust port `xaft`.

Scope is deliberately the *surface a user can touch*: CLI commands, slash commands,
TUI triggers and keybindings, built-in tools, configuration keys, and modes. Internal
architecture is **not** required to match — xaft is free to be idiomatic Rust.

Legend: ✅ present · 🟡 partial / divergent semantics · ❌ absent

---

## How to read this document

Each row cites the **agenthicc** source location that defines the feature and the
**xaft** crate/module that implements (or should implement) it. Status is updated as
gaps close. See `knowledge/tui-parity-gap-analysis.md` for the detailed TUI gap
narrative (gaps 1–11) that this matrix indexes.

**Source of truth.** agenthicc entries were extracted programmatically from its own
registries — `@command`/`@group` decorators in `src/agenthicc/cli/commands/*`, the
`BUILTIN_COMMANDS` list in `src/agenthicc/commands/builtins.py`, `__all__` /
`*_AGENT_TOOLS` lists in `src/agenthicc/tools/**/agent_tools.py`, the dataclasses in
`src/agenthicc/config.py`, and `src/agenthicc/modes/builtin.py`. xaft entries were
extracted from the clap enums in `crates/xaft-cli/src/args.rs`, the `SlashCommand`
enum in `crates/xaft-tui/src/slash/mod.rs`, `TOOL_NAME` consts in `crates/xaft-tools`,
the config structs in `crates/xaft-config`, and `crates/xaft-tui/src/mode/builtins.rs`.

---

## 1. CLI commands

agenthicc registers **57** CLI entries via `@command`/`@group`. xaft exposes
`run`, `config {show,init,validate}`, `session {list,show,resume,cancel}`, `completions`,
`version`.

| # | agenthicc command | agenthicc source | xaft location | Status |
|---|---|---|---|---|
| 1 | `config show` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Show` | ✅ |
| 2 | `config init` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Init` | ✅ |
| 3 | `config validate` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Validate` | ✅ |
| 4 | `config profiles` | `cli/commands/config.py` | crates/xaft-cli `ConfigSubcommand::Profiles` | ✅ |
| 5 | `session list` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::List` | ✅ |
| 6 | `session show` | `cli/commands/session_service.py` | crates/xaft-cli `SessionSubcommand::Show` | ✅ |
| 7 | `session resume` | — (xaft-only; agenthicc resumes via `jobs resume`) | crates/xaft-cli `SessionSubcommand::Resume` | ✅ |
| 8 | `session send` | `cli/commands/session_service.py` | — | ❌ |
| 9 | `session control` | `cli/commands/session_service.py` | — | ❌ |
| 10 | `session create` | `cli/commands/session_service.py` | — | ❌ |
| 11 | `session events` | `cli/commands/session_service.py` | — | ❌ |
| 12 | `session export` | `cli/commands/session_service.py` | — | ❌ |
| 13 | `session serve` | `cli/commands/session_service.py` | — | ❌ |
| 14 | `sessions list` | `cli/commands/sessions.py` | `xaft session list` | ✅ |
| 15 | `sessions show` | `cli/commands/sessions.py` | `xaft session show` | ✅ |
| 16 | `sessions export` | `cli/commands/sessions.py` | `xaft session export` (crates/xaft-cli `commands/sessions.rs`) | ✅ |
| 17 | `sessions inspect` | `cli/commands/sessions.py` | `xaft session inspect` (crates/xaft-cli `commands/sessions.rs`) | ✅ |
| 18 | `jobs list` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 19 | `jobs status` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 20 | `jobs cancel` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 21 | `jobs approve` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 22 | `jobs reject` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 23 | `jobs input` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 24 | `jobs labels` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 25 | `jobs rename` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 26 | `jobs retry` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 27 | `jobs resume` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 28 | `jobs archive` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 29 | `jobs restore` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 30 | `jobs delete` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 31 | `jobs purge` | `cli/commands/background.py` | crates/xaft-cli `jobs` (`commands/jobs.rs` + `jobs/`) | ✅ |
| 32 | `mcp list` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 33 | `mcp add` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 34 | `mcp get` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 35 | `mcp remove` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 36 | `mcp connect` | `cli/commands/mcp.py` | — | ❌ |
| 37 | `mcp disconnect` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 38 | `mcp auth` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 39 | `mcp logout` | `cli/commands/mcp.py` | crates/xaft-cli `Commands::Mcp` / `mcp.rs` | ✅ |
| 40 | `mcp refresh` | `cli/commands/mcp.py` | — | ❌ |
| 41 | `mcp doctor` | `cli/commands/mcp.py` | crates/xaft-tui slash `/doctor` | 🟡 |
| 42 | `runs list` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 43 | `runs show` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 44 | `runs cancel` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 45 | `runs resume` | `cli/commands/runs.py` | crates/xaft-cli `RunsSubcommand` (`commands/runs.rs` + `runs/`) | ✅ |
| 46 | `skills add` | `cli/commands/skills.py` | — | ❌ |
| 47 | `trust cli` | `cli/commands/trust.py` | — | ❌ |
| 48 | `workflows list` | `cli/commands/workflows.py` | — | ❌ |
| 49 | `workflows run` | `cli/commands/workflows.py` | — | ❌ |
| 50 | `worktrees list` | `cli/commands/worktrees.py` | — | ❌ |
| 51 | `worktrees show` | `cli/commands/worktrees.py` | — | ❌ |
| 52 | `worktrees integrate` | `cli/commands/worktrees.py` | — | ❌ |
| 53 | `worktrees recover` | `cli/commands/worktrees.py` | — | ❌ |
| 54 | `login` | `cli/commands/auth.py` | crates/xaft-tui slash `/login` (no CLI subcommand) | 🟡 |
| 55 | `logout` | `cli/commands/auth.py` | crates/xaft-tui slash `/logout` (no CLI subcommand) | 🟡 |
| 56 | `whoami` | `cli/commands/auth.py` | — | ❌ |
| 57 | `init` | `cli/commands/init.py` | crates/xaft-tui slash `/init` (no CLI subcommand) | 🟡 |
| 58 | `run` | `cli/parser.py` | crates/xaft-cli `Commands::Run` | ✅ |
| 59 | `agents` | `cli/commands/` (agent registry) | — (no CLI subcommand; `/agents` slash only) | 🟡 |
| 60 | `attach` | `cli/commands/` (attach to session) | — | ❌ |

**Subtotal: 60 rows.** ❌ 18 · 🟡 5 · ✅ 37

---

## 2. Slash commands

> **agenthicc source:** `src/agenthicc/commands/builtins.py` (`BUILTIN_COMMANDS`).

agenthicc registers **26** built-in slash commands (`BUILTIN_COMMANDS`) plus
**3** aliases. xaft's `SlashCommand` enum has **24** variants, several of which have no
agenthicc counterpart (they are Claude-Code-style extras: `/diff`, `/commit`, `/pr`,
`/rewind`, `/permissions`, `/theme`, `/vim`, `/emacs`, `/doctor`, `/memory`, `/agents`).

| # | agenthicc command | aliases | xaft location | Status |
|---|---|---|---|---|
| 1 | `/replay` | — | — | ❌ |
| 2 | `/cancel` | `/interrupt` | crates/xaft-tui `SlashCommand::Cancel` | ✅ |
| 3 | `/loop` | — | — | ❌ |
| 4 | `/loops` | — | — | ❌ |
| 5 | `/clear` | — | crates/xaft-tui `SlashCommand::Clear` | ✅ |
| 6 | `/commands` | — | crates/xaft-tui `SlashCommand::Commands` | ✅ |
| 7 | `/tools` | — | crates/xaft-tui `SlashCommand::Tools` | ✅ |
| 8 | `/workflows` | — | — | ❌ |
| 9 | `/config` | — | crates/xaft-tui `SlashCommand::Config` | ✅ |
| 10 | `/expand` | — | crates/xaft-tui `SlashCommand::Expand` | ✅ |
| 11 | `/help` | — | crates/xaft-tui `SlashCommand::Help` | ✅ |
| 12 | `/history` | — | crates/xaft-tui `SlashCommand::History` | ✅ |
| 13 | `/init` | — | crates/xaft-tui `SlashCommand::Init` | ✅ |
| 14 | `/mcp` | — | crates/xaft-tui `SlashCommand::Mcp` | ✅ |
| 15 | `/model` | — | crates/xaft-tui `SlashCommand::Model` | ✅ |
| 16 | `/models` | — | crates/xaft-tui `SlashCommand::Models` | ✅ |
| 17 | `/skills` | — | — | ❌ |
| 18 | `/status` | — | crates/xaft-tui `SlashCommand::Status` | ✅ |
| 19 | `/startup` | — | — | ❌ |
| 20 | `/ps` | `/processes` | — | ❌ |
| 21 | `/workers` | — | crates/xaft-tui `SlashCommand::Agents` | 🟡 |
| 22 | `/stop` | `/stop-terminal` | — | ❌ |
| 23 | `/usage` | — | crates/xaft-tui `SlashCommand::Cost` | ✅ |
| 24 | `/mode` | — | crates/xaft-tui `SlashCommand::Mode` | ✅ |
| 25 | `/workflow` | — | — | ❌ |
| 26 | `/compact` | — | crates/xaft-tui `SlashCommand::Compact` | ✅ |

**Subtotal: 26 rows.** ❌ 9 · 🟡 1 · ✅ 16

### 2b. Open slash rows and their owner (measured during G5)

Ten of the 26 rows are still open. Each is blocked on a subsystem that either
does not exist in xaft or is owned by another goal. Recorded here so the next
pass reads the blocker instead of re-deriving it.

| # | command | blocked on | owner |
|---|---|---|---|
| 1 | `/replay` | Replay machinery over the session log: agenthicc resolves `find_latest_session_for_cwd()` and pushes a `pending_replay`; xaft has `replay_history_lines*` for `--resume` but no on-demand replay command. | new increment |
| 3 | `/loop` | `LoopManager`. Also gated on the 10 `LoopSettings` keys, which are one of the seven **deferred config classes** awaiting an authorized/declined decision. | deferred-class decision, then new increment |
| 4 | `/loops` | Same `LoopManager` as row 3. | as row 3 |
| 8 | `/workflows` | Workflow registry. agenthicc builds it from `.agenthicc/` project + `~/.agenthicc/` user definitions; xaft has a fixed `WorkflowMode` enum and no plugin-defined workflows. | G10 |
| 17 | `/skills` | Skill discovery. agenthicc reads `SkillDef` records with per-agent permission sets; xaft has a `xaft-skills` crate and a `$` picker but no listing/permission surface. | G8 |
| 19 | `/startup` | Startup phase coordinator with per-phase `elapsed_s`/`state` reports. xaft records no startup phases. | G9 |
| 20 | `/ps` (`/processes`) | Background **terminal** manager. xaft has background *jobs* (`xaft jobs`), which is a different concept; `grep` for `terminal_manager` across `crates/` returns 0 hits. | G9 |
| 21 | `/workers` | `ManifestStore` / `ParallelManifest`. Needs the parallel-orchestration layer, which does not exist yet. | G4b-7, then G9 |
| 22 | `/stop` (`/stop-terminal`) | Same background terminal manager as row 20. | G9 |
| 25 | `/workflow` | Workflow switching/resume. In agenthicc this is intercepted in `TUISession.route()` before dispatch, so it needs the session-local workflow state G10 builds. | G10 |

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
*semantics* that are still divergent are section 3 rows 1, 7 and 8.

Keybinding row 11 (`s` suspend in the terminal overlay) is blocked on the same
missing background-terminal overlay as rows 20 and 22.

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
| 1 | `Safe` | — | Reads/search/git-read run directly; every `WRITE`, `GIT_WRITE`, `EXECUTE`, `NETWORK` and `UNDECLARED` tool requires **explicit user approval** (`RuntimeMode.approval_required`, enforced by the runtime capability gate). | crates/xaft-tui mode `safe` — a 12-tool read-only allowlist that **blocks** writes and never prompts | 🟡 |
| 2 | `Plan` | — | Read-only planning; `blocked_capabilities` **hard-blocks** the five restricted capabilities. | crates/xaft-tui mode `plan` — 15-tool read-only allowlist + a `MUST NOT` system patch | ✅ |
| 3 | `Yolo` | — | Full automatic; every tool allowed, no prompt, no patch. | crates/xaft-tui mode `auto` — no `tool_filter`, no system patch | ✅ |
| 4 | `Yolo` | alias `auto` | `MODE_ALIASES["auto"] = "Yolo"` | `auto` **is** xaft's canonical spelling of Yolo; `yolo` is accepted as its alias | ✅ |
| 5 | `Safe` | alias `ask` / `guard` | `MODE_ALIASES["ask"] = "Safe"`, `["guard"] = "Safe"` | aliases `ask` and `guard` → mode `safe` | ✅ |
| 6 | `Plan` | alias `review` | `MODE_ALIASES["review"] = "Plan"` | alias `review` → mode `plan` | ✅ |
| 7 | `Replay` (internal) | — | Registered with `selectable=False`; all nine capabilities blocked; deliberately absent from the cycle and from the `/mode` listing. | — | ❌ |
| 8 | — | `debug` rejected | agenthicc registers no `Debug` identity, so `/mode debug` raises `UnknownModeError`. The name is *also* reserved: a mode plugin named `debug` is dropped with "Debug is a rejected mode name", and `debug` is filtered out of workflow bindings. | crates/xaft-tui mode `debug` **accepted** — full capabilities plus a per-response token/cost footer | ❌ |

**Subtotal: 8 rows.** ❌ 2 · 🟡 1 · ✅ 5

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
subsystem work, not a mode edit.

**Row 7 (`Replay`) was missing entirely.** agenthicc registers a fourth
identity with `selectable=False`; it is a real mode in `registry.all(
include_internal=True)` even though it is not user-selectable, so it belongs
in a matrix scoped to agenthicc's modes. ❌.

### 3c. Two registered modes cannot be selected by their own name

Found during G7 and previously recorded nowhere. `ModeManager::set` resolves
the requested name through `canonical_mode_name` **before** it consults the
registry, and that alias table contains `ask → safe` and `review → plan` —
both copied verbatim from agenthicc's `MODE_ALIASES`. The registered `ask`
and `review` modes are therefore unreachable by *any* spelling:

| registered mode | what it does | what `/mode <its own name>` actually does |
|---|---|---|
| `ask` | every tool available, side effects behind a `[Y/n]` prompt | activates `safe` — a read-only allowlist |
| `review` | structured review prompt, Plan's 15-tool allowlist | activates `plan` — same allowlist, different prompt |

`mode_parity_integration.rs::three_of_the_six_registered_modes_cannot_be_activated_by_their_own_name`
enumerates the registry and proves the set is exactly these two, so a third
cannot appear unnoticed. `debug` is reachable by name but is outside the
cycle (row 8).

**No fix was applied, deliberately.** The two directions available both
require authorization that has not been given:

- *Prefer an exact registered name over the alias* would make `/mode ask`
  reach the unfiltered `[Y/n]` mode. That silently moves a currently
  accepted input from a hard read-only sandbox to **every tool**, gated only
  by a system-patch instruction the model is free to ignore. A fail-open
  security change is not a silent one to make.
- *Delete the two dead modes* is a feature removal.
- *Rename xaft's `safe`* (e.g. to `sandbox`) so the name `ask`/`safe` can
  carry agenthicc's meaning is a rename of a shipped user-facing name.

What was done instead is to make the code stop *claiming* otherwise: the
`/mode` handler's comment that "`/mode debug` is rejected" and
`ModeManager::reject_if_not_cyclable`'s "mirroring agenthicc's `/mode` guard
(Debug is not an alias and is rejected)" were both false — `builtin_modes()`
contains `debug`, so the guard's `!any(...)` never fires, and
`reject_if_not_cyclable` is called from nowhere except its own unit test.
Both comments now describe the measured behaviour, and the tests pin it.

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
| 11 | `s` | Suspend in terminal overlay | — | ❌ |
| 12 | `?` | Toggle help overlay | crates/xaft-tui help overlay | ✅ |

**Subtotal: 12 rows.** ❌ 1 · ✅ 11

---

## 6. Built-in tools

agenthicc exposes **78** built-in tools across 8
packages. xaft implements **48** (`TOOL_NAME` consts in
`crates/xaft-tools`), and every tool this section marks ✅ is proven
resolvable in a built `ToolRegistry` by
`crates/xaft-tools/tests/tool_parity_reachability.rs`.

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
| 36 | `inspect_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 37 | `run_bash` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 38 | `run_command` | `tools/exec/agent_tools.py` | `run_command` | ✅ |
| 39 | `run_python` | `tools/exec/agent_tools.py` | — | ❌ |
| 40 | `run_python_expr` | `tools/exec/agent_tools.py` | — | ❌ |
| 41 | `run_tests` | `tools/exec/agent_tools.py` | — | ❌ |
| 42 | `shell` | `tools/exec/agent_tools.py` | `bash_exec` (renamed) | ✅ |
| 43 | `stop_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 44 | `wait_terminal` | `tools/exec/agent_tools.py` | — | ❌ |
| 45 | `wait_terminal_ready` | `tools/exec/agent_tools.py` | — | ❌ |

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
| 60 | `playwright_status` | `tools/playwright/agent_tools.py` | — | ❌ |
| 61 | `playwright_open` | `tools/playwright/agent_tools.py` | — | ❌ |
| 62 | `playwright_snapshot` | `tools/playwright/agent_tools.py` | — | ❌ |
| 63 | `playwright_click` | `tools/playwright/agent_tools.py` | — | ❌ |
| 64 | `playwright_fill` | `tools/playwright/agent_tools.py` | — | ❌ |
| 65 | `playwright_press` | `tools/playwright/agent_tools.py` | — | ❌ |
| 66 | `playwright_wait_for` | `tools/playwright/agent_tools.py` | — | ❌ |
| 67 | `playwright_screenshot` | `tools/playwright/agent_tools.py` | — | ❌ |
| 68 | `playwright_close` | `tools/playwright/agent_tools.py` | — | ❌ |

### 6.12 `tools/cloakbrowser/agent_tools.py` (9 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 69 | `cloakbrowser_status` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 70 | `cloakbrowser_open` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 71 | `cloakbrowser_snapshot` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 72 | `cloakbrowser_click` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 73 | `cloakbrowser_fill` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 74 | `cloakbrowser_press` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 75 | `cloakbrowser_wait_for` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 76 | `cloakbrowser_screenshot` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |
| 77 | `cloakbrowser_close` | `tools/cloakbrowser/agent_tools.py` | — | ❌ |

### 6.13 `agent_tools.py` (1 tools)

| # | agenthicc tool | agenthicc source | xaft tool | Status |
|---|---|---|---|---|
| 78 | `mcp_connection_guide` | `agent_tools.py` | `mcp_connection_guide` | ✅ |

**Subtotal: 78 rows.** ❌ 39 · 🟡 0 · ✅ 39

### 6b. Open tool rows and their owner (measured during G6)

The rows that remain open are not unfinished work inside `xaft-tools`. Each
needs a subsystem that does not exist yet, or an authorization decision that
has not been made. Recorded here so the next pass reads the blocker instead of
re-deriving it.

| # | tool(s) | blocked on | owner |
|---|---|---|---|
| 36, 43, 44, 45 | `inspect_terminal`, `stop_terminal`, `wait_terminal`, `wait_terminal_ready` | A **background terminal manager**. These four operate on a long-lived process xaft owns and can address by handle; xaft has background *jobs* (`xaft jobs`) and a one-shot `bash_exec`, but no terminal registry. `grep -rn "terminal_manager" crates/` returns 0 hits. | G9 |
| 39, 40, 41 | `run_python`, `run_python_expr`, `run_tests` | A **host Python interpreter**. `run_python*` evaluates a Python snippet, and `run_tests` is literally a pytest/unittest runner (`agenthicc/tools/exec/agent_tools.py:199`, defaults `path="tests/"`, `framework="pytest"`) — neither maps onto a Rust product's `cargo test`. Shipping them means shipping a tool that fails on any host without Python unless it also probes for one and says so plainly. That is an availability/product decision, not tool-writing work. | product decision, then a new increment |
| 46-50 | the whole `introspect` package | agenthicc's **self-documentation** tools: they read agenthicc's own docs and source. The question is not "can xaft call these" but "should xaft expose a self-introspection surface at all, and over which corpus". | product decision |
| 51-59 | the whole `outlook` package (9 tools) | A Microsoft Graph client: OAuth, token storage, mail and calendar endpoints. xaft has no Graph client and no account concept. | new auth subsystem, authorization-gated |
| 60-68 | the whole `playwright` package (9 tools) | Browser automation, gated on the `PlaywrightSettings` (16 config keys) authorized/declined decision, which is still open. | deferred-config decision |
| 69-77 | the whole `cloakbrowser` package (9 tools) | As above: `CloakBrowserSettings` (15 config keys), same open decision. | deferred-config decision |

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
`parity-matrix-stats.py` counts. **All ten are now ❌ with an em-dash xaft
column**, so each row states what is true (xaft has no such tool) and the
blocker is recorded in the table above rather than in a footnote. That leaves
zero 🟡 rows in this section, which is the correct outcome: 🟡 should mean
"some real part of this is implemented", and for these ten none was.

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

> **Remaining gaps:** the whole `outlook` package (9), `playwright` (9),
> `cloakbrowser` (9), `introspect` (5), the four background-terminal exec tools
> and the three `run_*` tools. xaft has no browser automation and no email
> surface; both are subsystem decisions rather than tool-writing work.
---

## 7. Configuration keys

agenthicc's `config.py` defines **185** fields across **20** classes, rendered
into `xaft.toml` by `config_template.py`. xaft defines **13** top-level sections.

### `SecretReference` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 1 | `env` | `config.py::SecretReference` | — | ❌ |

### `RequestOptionSettings` (7 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 2 | `extra_headers` | `config.py::RequestOptionSettings` | — | ❌ |
| 3 | `extra_query` | `config.py::RequestOptionSettings` | — | ❌ |
| 4 | `extra_body` | `config.py::RequestOptionSettings` | — | ❌ |
| 5 | `provider` | `config.py::RequestOptionSettings` | — | ❌ |
| 6 | `timeout_s` | `config.py::RequestOptionSettings` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 7 | `max_retries` | `config.py::RequestOptionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 8 | `include_raw_response` | `config.py::RequestOptionSettings` | — | ❌ |

### `ProviderProfile` (18 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 9 | `name` | `config.py::ProviderProfile` | crates/xaft-config `the TOML table key is the name` | ✅ |
| 10 | `provider` | `config.py::ProviderProfile` | — | ❌ |
| 11 | `model` | `config.py::ProviderProfile` | — | ❌ |
| 12 | `base_url` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 13 | `api_key` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 14 | `api_key_env` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 15 | `default_headers` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 16 | `default_query` | `config.py::ProviderProfile` | — | ❌ |
| 17 | `client_options` | `config.py::ProviderProfile` | — | ❌ |
| 18 | `request_options` | `config.py::ProviderProfile` | — | ❌ |
| 19 | `timeout_s` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 20 | `max_retries` | `config.py::ProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 21 | `temperature` | `config.py::ProviderProfile` | — | ❌ |
| 22 | `top_p` | `config.py::ProviderProfile` | — | ❌ |
| 23 | `max_completion_tokens` | `config.py::ProviderProfile` | — | ❌ |
| 24 | `protocol` | `config.py::ProviderProfile` | — | ❌ |
| 25 | `capabilities` | `config.py::ProviderProfile` | — | ❌ |
| 26 | `session_header` | `config.py::ProviderProfile` | — | ❌ |

### `ResolvedProviderProfile` (17 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 27 | `name` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset` | ✅ |
| 28 | `provider` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset` | ✅ |
| 29 | `model` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset` | ✅ |
| 30 | `base_url` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 31 | `api_key` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 32 | `default_headers` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 33 | `default_query` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 34 | `client_options` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 35 | `request_options` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 36 | `timeout_s` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 37 | `max_retries` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ProviderConfig` | ✅ |
| 38 | `temperature` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset` | ✅ |
| 39 | `top_p` | `config.py::ResolvedProviderProfile` | crates/xaft-config `ResolvedAgentPreset` | ✅ |
| 40 | `max_completion_tokens` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 41 | `protocol` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 42 | `capabilities` | `config.py::ResolvedProviderProfile` | — | ❌ |
| 43 | `session_header` | `config.py::ResolvedProviderProfile` | — | ❌ |

### `ExecutionSettings` (34 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 44 | `max_concurrent_intents` | `config.py::ExecutionSettings` | — | ❌ |
| 45 | `max_parallel_tasks` | `config.py::ExecutionSettings` | — | ❌ |
| 46 | `agent_pool_size` | `config.py::ExecutionSettings` | — | ❌ |
| 47 | `max_agent_turns` | `config.py::ExecutionSettings` | — | ❌ |
| 48 | `authoring_max_generation_attempts` | `config.py::ExecutionSettings` | — | ❌ |
| 49 | `authoring_max_phase_turns` | `config.py::ExecutionSettings` | — | ❌ |
| 50 | `max_output_tokens` | `config.py::ExecutionSettings` | — | ❌ |
| 51 | `turn_timeout_s` | `config.py::ExecutionSettings` | — | ❌ |
| 52 | `auto_compact` | `config.py::ExecutionSettings` | — | ❌ |
| 53 | `context_windows` | `config.py::ExecutionSettings` | — | ❌ |
| 54 | `prompt_cache` | `config.py::ExecutionSettings` | — | ❌ |
| 55 | `file_cache` | `config.py::ExecutionSettings` | — | ❌ |
| 56 | `transport_max_retries` | `config.py::ExecutionSettings` | — | ❌ |
| 57 | `transport_retry_base_delay_s` | `config.py::ExecutionSettings` | — | ❌ |
| 58 | `transport_retry_max_total_s` | `config.py::ExecutionSettings` | — | ❌ |
| 59 | `llm_sdk_max_retries` | `config.py::ExecutionSettings` | — | ❌ |
| 60 | `profile` | `config.py::ExecutionSettings` | — | ❌ |
| 61 | `provider` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 62 | `model` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 63 | `api_key` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 64 | `api_key_env` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 65 | `base_url` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig` | ✅ |
| 66 | `default_headers` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig.headers` | ✅ |
| 67 | `default_query` | `config.py::ExecutionSettings` | — | ❌ |
| 68 | `client_options` | `config.py::ExecutionSettings` | — | ❌ |
| 69 | `request_options` | `config.py::ExecutionSettings` | — | ❌ |
| 70 | `timeout_s` | `config.py::ExecutionSettings` | crates/xaft-config `ProviderConfig.timeout_secs` | ✅ |
| 71 | `temperature` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 72 | `top_p` | `config.py::ExecutionSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 73 | `max_completion_tokens` | `config.py::ExecutionSettings` | — | ❌ |
| 74 | `provider_capabilities` | `config.py::ExecutionSettings` | — | ❌ |
| 75 | `_resolved_profile` | `config.py::ExecutionSettings` | — | ❌ |
| 76 | `session_header` | `config.py::ExecutionSettings` | — | ❌ |
| 77 | `irrecoverable_error_max_retries` | `config.py::ExecutionSettings` | — | ❌ |

### `CloakBrowserSettings` (15 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 78 | `enabled` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 79 | `transport` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 80 | `cdp_endpoint` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 81 | `allowed_domains` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 82 | `headless` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 83 | `navigation_timeout_s` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 84 | `action_timeout_s` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 85 | `max_pages` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 86 | `max_actions_per_turn` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 87 | `max_snapshot_chars` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 88 | `max_screenshot_bytes` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 89 | `allow_persistent_profiles` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 90 | `profile_root` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 91 | `license_key_env` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |
| 92 | `allow_all_domains` | `config.py::CloakBrowserSettings` | — (deferred subsystem) | ❌ |

### `PlaywrightSettings` (16 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 93 | `enabled` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 94 | `transport` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 95 | `browser_type` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 96 | `browser_channel` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 97 | `executable_path` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 98 | `allowed_domains` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 99 | `headless` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 100 | `navigation_timeout_s` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 101 | `action_timeout_s` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 102 | `max_pages` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 103 | `max_actions_per_turn` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 104 | `max_snapshot_chars` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 105 | `max_screenshot_bytes` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 106 | `allow_persistent_profiles` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 107 | `profile_root` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |
| 108 | `allow_all_domains` | `config.py::PlaywrightSettings` | — (deferred subsystem) | ❌ |

### `ToolSettings` (11 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 109 | `mcp_servers` | `config.py::ToolSettings` | crates/xaft-config `XaftConfig.mcp` | ✅ |
| 110 | `plugins` | `config.py::ToolSettings` | — | ❌ |
| 111 | `allowed` | `config.py::ToolSettings` | — | ❌ |
| 112 | `denied` | `config.py::ToolSettings` | — | ❌ |
| 113 | `max_live_tool_calls` | `config.py::ToolSettings` | — | ❌ |
| 114 | `group_exploratory_calls` | `config.py::ToolSettings` | — | ❌ |
| 115 | `http_timeout_s` | `config.py::ToolSettings` | — | ❌ |
| 116 | `question_timeout_s` | `config.py::ToolSettings` | — | ❌ |
| 117 | `cloakbrowser` | `config.py::ToolSettings` | — | ❌ |
| 118 | `playwright` | `config.py::ToolSettings` | — | ❌ |
| 119 | `browser_backend` | `config.py::ToolSettings` | — | ❌ |

### `MemorySettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 120 | `project_memory_path` | `config.py::MemorySettings` | crates/xaft-config `MemoryConfig.project_memory_path` (read by `src/main.rs` → `XaftMemoryManager::new_in_dir`) | ✅ |
| 121 | `vector_db` | `config.py::MemorySettings` | — | ❌ |
| 122 | `session_ttl_seconds` | `config.py::MemorySettings` | — | ❌ |

### `SecuritySettings` (5 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 123 | `sandbox_mode` | `config.py::SecuritySettings` | — | ❌ |
| 124 | `allowed_paths` | `config.py::SecuritySettings` | — | ❌ |
| 125 | `network_allow_list` | `config.py::SecuritySettings` | — | ❌ |
| 126 | `max_tool_cpu_seconds` | `config.py::SecuritySettings` | — | ❌ |
| 127 | `max_tool_memory_mb` | `config.py::SecuritySettings` | — | ❌ |

### `ApiSettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 128 | `host` | `config.py::ApiSettings` | — (deferred subsystem) | ❌ |
| 129 | `port` | `config.py::ApiSettings` | — (deferred subsystem) | ❌ |
| 130 | `api_key_env` | `config.py::ApiSettings` | — (deferred subsystem) | ❌ |

### `PluginSettings` (9 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 131 | `auto_trust` | `config.py::PluginSettings` | — | ❌ |
| 132 | `auto_install` | `config.py::PluginSettings` | — | ❌ |
| 133 | `install_target` | `config.py::PluginSettings` | — | ❌ |
| 134 | `allowed_modules` | `config.py::PluginSettings` | — | ❌ |
| 135 | `timeout_seconds` | `config.py::PluginSettings` | — | ❌ |
| 136 | `disabled` | `config.py::PluginSettings` | — | ❌ |
| 137 | `trust_file` | `config.py::PluginSettings` | — | ❌ |
| 138 | `audit_file` | `config.py::PluginSettings` | — | ❌ |
| 139 | `strict_cli_shadow` | `config.py::PluginSettings` | — | ❌ |

### `BehaviourSettings` (3 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 140 | `verbose` | `config.py::BehaviourSettings` | — | ❌ |
| 141 | `confirm_exits` | `config.py::BehaviourSettings` | — | ❌ |
| 142 | `resume_transcript_turns` | `config.py::BehaviourSettings` | — | ❌ |

### `LoopSettings` (10 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 143 | `enabled` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 144 | `default_interval_s` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 145 | `min_interval_s` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 146 | `max_interval_s` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 147 | `max_age_s` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 148 | `max_prompt_bytes` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 149 | `max_consecutive_failures` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 150 | `busy_poll_s` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 151 | `persist` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |
| 152 | `allow_slash_commands` | `config.py::LoopSettings` | — (deferred subsystem) | ❌ |

### `AgentSettings` (5 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 153 | `description` | `config.py::AgentSettings` | — | ❌ |
| 154 | `model` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 155 | `max_turns` | `config.py::AgentSettings` | crates/xaft-config `AgentPreset` | ✅ |
| 156 | `allowed_skills` | `config.py::AgentSettings` | — | ❌ |
| 157 | `denied_skills` | `config.py::AgentSettings` | — | ❌ |

### `AgentsSettings` (1 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 158 | `agents` | `config.py::AgentsSettings` | — | ❌ |

### `SkillsSettings` (2 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 159 | `install_default_skills` | `config.py::SkillsSettings` | — (deferred subsystem) | ❌ |
| 160 | `default_skill_directory` | `config.py::SkillsSettings` | — (deferred subsystem) | ❌ |

### `StorageS3Settings` (9 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 161 | `bucket` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 162 | `region` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 163 | `prefix` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 164 | `access_key_id` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 165 | `secret_access_key` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 166 | `endpoint_url` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 167 | `profile` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 168 | `path_style` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |
| 169 | `mounts` | `config.py::StorageS3Settings` | — (deferred subsystem) | ❌ |

### `StorageSettings` (2 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 170 | `s3` | `config.py::StorageSettings` | — (deferred subsystem) | ❌ |
| 171 | `default_backend` | `config.py::StorageSettings` | — (deferred subsystem) | ❌ |

### `AgenthiccConfig` (14 keys)

| # | agenthicc key | agenthicc source | xaft section | Status |
|---|---|---|---|---|
| 172 | `execution` | `config.py::AgenthiccConfig` | — | ❌ |
| 173 | `providers` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.provider` | ✅ |
| 174 | `behaviour` | `config.py::AgenthiccConfig` | — | ❌ |
| 175 | `hooks` | `config.py::AgenthiccConfig` | — | ❌ |
| 176 | `tools` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.tool` | ✅ |
| 177 | `memory` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig` | ✅ |
| 178 | `security` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.guardrail` | ✅ |
| 179 | `api` | `config.py::AgenthiccConfig` | — | ❌ |
| 180 | `plugins` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig` | ✅ |
| 181 | `skills` | `config.py::AgenthiccConfig` | — | ❌ |
| 182 | `agents` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.agent` | ✅ |
| 183 | `storage` | `config.py::AgenthiccConfig` | — | ❌ |
| 184 | `workflows` | `config.py::AgenthiccConfig` | crates/xaft-config `XaftConfig.workflow` | ✅ |
| 185 | `loops` | `config.py::AgenthiccConfig` | — | ❌ |

**Subtotal: 185 rows.** ❌ 146 · 🟡 0 · ✅ 39

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

---

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
| `session` service (`send`/`control`/`create`/`events`/`export`/`serve`) | 8-13 ❌ | Not a persistence gap — a capability-gated service with a durable event log. Owned by the G4b-4/5/6 increments, not by storage. |
| `/replay` | 1 ❌ | `replay_history_lines*` exist for `--resume`, but `knowledge/tui-parity-gap-analysis.md` **Gap 4** records that xaft replays from in-memory keys rather than a session-log tail projection. That gap is still open and is *not* fixed by the persistence work below. |
| `/skills` | 17 ❌ | `crates/xaft-skills` ships `SkillLoader` / `Skill` / `SkillFrontmatter` and reads `.xaft/skills/` and `~/.config/xaft/skills/`, but there is no listing surface and the paths are hardcoded — see 2b. |
| `MemorySettings` | 120 ✅, 121-122 ❌ | `121 vector_db` is divergent by design (agenthicc's embedding store vs xaft's SQLite keyword/vector store). `122 session_ttl_seconds` has a near-miss: `xaft_memory::MemoryConfig.auto_remember_ttl_secs` exists but **no user-facing key sets it** — `src/main.rs` hardcodes it to `None`. |
| `SkillsSettings`, `allowed_skills`/`denied_skills` | 156, 157, 159, 160 ❌ | Deferred subsystem, per `docs/config-key-audit.md`. `SkillLoader` has no permission model, so there is nothing to gate. |
| config `memory` / `skills` | 177 ✅ / 181 ❌ | `XaftConfig.memory` exists; `XaftConfig.skills` does not, so the skills loader takes its paths from constants. |

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

| Section | agenthicc entries | xaft entries | ✅ | 🟡 | ❌ |
|---|---|---|---|---|---|
| 1. CLI commands | 59 | 19 | 37 | 5 | 18 |
| 2. Slash commands | 26 | 24 | 16 | 1 | 9 |
| 3. Modes | 4 | 6 | 5 | 1 | 2 |
| 4. TUI triggers | 3 | 3 | 3 | 0 | 0 |
| 5. Keybindings | 12 | 12 | 11 | 0 | 1 |
| 6. Built-in tools | 78 | 48 | 39 | 0 | 39 |
| 7. Configuration keys | 185 | 13 | 39 | 0 | 146 |
| **TOTAL** | **367** | **125** | **150** | **7** | **215** |

Current parity: **150** of 372 rows ✅ (40%). The remaining
**222** rows are the backlog for goals G4–G10.

---

## Related documents

- `knowledge/tui-parity-gap-analysis.md` — narrative TUI gap analysis (gaps 1–11)
- [`g4-decomposition.md`](g4-decomposition.md) — what is actually left in G4, sized per family, with row ranges and owners
- `docs/guides/modes.md`, `docs/guides/tui.md`, `docs/guides/configuration.md` — xaft guides
- agenthicc `docs/guides/*.md` and `docs/usage/` — reference documentation
