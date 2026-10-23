# G4 decomposition (measured)

<!-- Analysis backing the per-family increments in the goal list. Not generated. -->

G4 ("close CLI command and config-key parity gaps") is an umbrella, not a
deliverable. This note records what is actually left, how big each piece is,
and who owns it, so the umbrella is not re-derived from scratch on every pass.

Row numbers refer to [`parity-matrix.md`](parity-matrix.md). Config-key row
numbers are 1-185 in section 7; CLI row numbers are 1-60 in section 1.

> **§1c is the authoritative open-row index; this document is the analysis
> behind it.** [`parity-matrix.md` §1c](parity-matrix.md#1c-open-cli-rows-and-their-owner-measured-not-re-derived)
> lists every open CLI row with its owner goal and states that where the two
> documents disagree §1c is current. This is the reciprocal half of that
> supersession — until it was written here, §1c pointed at this document and
> this document never pointed back, so a reader arriving here first had no way
> to learn §1c exists. Nothing in the per-family work below needs re-deriving
> from §1c; re-derive the **count** with `python3
> scripts/cli-row-ownership.py --check`, never from prose.

## Baseline correction

> **Goal text is immutable, so three briefs in the goal list are stale on their
> face. Read this section before working from any of them.** Each is listed
> with what replaced it. Every number below is measured, and both checkers
> re-derive them from the rows rather than from prose.

### Stale brief 1 — G4b (goal-list index 7)

> **G4b's original brief is SUPERSEDED and cannot be edited.** Goal text is
> immutable once written, so G4b (goal-list index 7) still reads "185
> config-key rows (128 🟡 + 57 ❌)" and "28 ❌ CLI rows". **Do not work from
> that text.** It is reproduced here only so the next pass can see what was
> replaced, and by what.

G4b's original brief counted **213 open rows** (28 ❌ CLI + 185 config keys =
128 🟡 + 57 ❌). That number is obsolete:

* The 128 🟡 were provisional placeholders, not measured gaps. The field-level
  diff in [`config-key-audit.md`](config-key-audit.md) (commit `ada5968`)
  resolved them: the config section is now **104 ✅ / 0 🟡 / 81 ❌**. On the CLI
  side, of the command families named in G4b's original brief, `config profiles`
  (row 4), `runs list|show|cancel|resume` (42-45), `sessions export|inspect`
  (16, 17) and all seven config-only `mcp` commands (32-35, 37-39) have landed,
  which is why the brief's 213 open rows is now **102**.
* The brief's "43 keys already match by exact name" was inflated to 24 by
  cross-class coincidences. See the audit for the four that mattered.

**Actually open: 21 CLI rows and 81 config rows** — 102 open rows in total.

The config figure has moved repeatedly since `ada5968`, which is worth writing
down because it is exactly the drift this section exists to prevent. Section 7's
✅ count, re-derived per commit rather than quoted from prose:

| Commit | What landed | Section 7 |
|---|---|---|
| `ada5968` | the measured field-level diff itself | 38 ✅ / 147 ❌ |
| `6f759d5` | row 120 `MemorySettings.project_memory_path`, G8 | 39 ✅ / 146 ❌ |
| `de5acc2` | reachability audit — **downgrades** rows 109 and 180 (see below) | **37 ✅ / 148 ❌** |
| `8db744c` | row 142 `resume_transcript_turns`, Gap 4 | 38 ✅ / 147 ❌ |
| `6d4cfa7` | per-agent `description` (row 153) | 41 ✅ / 144 ❌ |
| `4b0559d` | provider profile-level model and sampling defaults | 47 ✅ / 138 ❌ |
| `b4a2034` | the `[execution]` section | 63 ✅ / 122 ❌ |
| `2914b30` | request options, protocol, capabilities, `[tools]` MCP servers | 87 ✅ / 98 ❌ |
| `d001f86` | `[security]` (rows 123-127); the `PluginSettings` keys | 103 ✅ / 82 ❌ |
| `00f8621` | `tui.verbose` (row 140) | **104 ✅ / 81 ❌** |

One row of that table runs *backwards*, and it is the reason the chronology is
recorded rather than summarised. `de5acc2` ("Audit every ✅ parity row for real
reachability") downgraded config rows 109 (`mcp_servers`) and 180 (`plugins`)
from ✅ to ❌ with the reason "`crates/xaft-config` declares the field but NO
other crate reads it (dead config)" — 39 ✅ → 37 ✅ and 146 ❌ → 148 ❌ — and
this document was never told. `512549b` later edited the very same "Actually
open" line for the CLI count and carried the stale config number straight
through, which is exactly why the CLI half of that sentence was repaired and the
config half was not. The downgrade itself was **correct** — a field nothing
reads is not a working key, which is the rule the G4c audit exists to enforce —
so the fix is to document it, not to undo it. Row 109 has since been re-marked
✅ by `2914b30`, which gave `ToolSettingsConfig.mcp_servers` a real reader, so
the downgrade applies to **one** row today: 180, bucketed below.

The non-deferred remainder is therefore **24**, not the 89 or 90 an earlier pass
would have written. Re-derive with `python3 scripts/parity-matrix-stats.py
--check` and `python3 scripts/config-key-audit.py --check` rather than trusting
any number written here, including this one — this paragraph has now been wrong
once for each of the reasons above.

The CLI figure is **21**, and it has moved four times, in three directions:

* this document's original count of **23** was superseded when rows 48 and 49
  (`workflows list`/`run`) landed — **21**;
* §1b then added rows 20, 27 and 29 (`jobs cancel`/`resume`/`restore`) as
  real-but-broken ✅ — **24**. They were counted as open because the table said
  ✅ and the binary said ❌;
* those three have now been **genuinely fixed** — the handler now calls
  `JobStore::cancel` and `JobStore::resume`, and `Job::pre_delete_status` makes
  the restore target survive between CLI invocations. See
  [`parity-matrix.md`](parity-matrix.md) §1b;
  the tests that asserted the bug were inverted, not supplemented — back to
  **21**.

`python3 scripts/cli-row-ownership.py --check` is the authority and currently
reports **open CLI rows: 21 · owned: 21**. It cross-checks §1c against section
1's own rows and fails loudly if either side drifts.

Note precisely what that script does and does not cover: it parses
`docs/parity-matrix.md` and **never opens this file**. So the §1c index is
policed mechanically, while the counts quoted in *this* document are policed
by hand. Run the script before trusting any number written here, and re-derive
the table's rows against it rather than adding to the prose.

> **Note the shape of the middle step.** Counting a false ✅ as "open" is a
> reasonable conservative move, and it is what made the drift visible — but it
> is not a status. Rows 20/27/29 were never ❌ in the sense of "missing
> subsystem"; they had working code and an unreachable handler. They are now
> closed, not reclassified, which is why the count returns to the same 21 it
> held before §1b existed.

### Stale brief 2 — the per-family ownership table (goal-list index 20)

> Goal-list index 20 ("Record the measured per-family ownership decisions for
> the remaining G4 CLI parity rows") **cites two figures that are no longer
> true**: "32 remaining G4 CLI parity rows" (measured: **21**) and "rows 8-13, 16-17,
> 36-40, 60" as G4's remaining surface (rows 16 and 17 — `sessions export` /
> `inspect` — have since landed as ✅). **Do not re-derive a baseline from that
> text.**
>
> Its deliverable is also **already done, twice**: the per-family table with
> sizes and owners is the *Part 1* table below, and
> [`parity-matrix.md` §1c](parity-matrix.md#1c-open-cli-rows-and-their-owner-measured-not-re-derived)
> is the authoritative open-row **index** built on it. §1c explicitly states
> that this document is the analysis and §1c is the index, and that where the
> two disagree §1c is current. There is no remaining unique work here — a
> pointer was wanted and §1c already carries it.

### Stale brief 3 — one unsatisfiable TEST clause (goal-list index 25)

> Goal-list index 25 (`42a7d516`, "Re-baseline G4b…") carries a TEST clause
> demanding that **"no goal in the list cites the pre-audit 128 🟡 figure"**.
> **That clause is VOID, not failing.** It cannot be satisfied as written: the
> figure is quoted verbatim in the immutable text of G4b (index 7) and of
> stale brief 2 (index 20) above, and goal text cannot be edited after
> creation. A test that demands the impossible should be recorded as void so a
> later pass does not read it as an outstanding failure — that is what this
> paragraph is for. The goal's *substantive* half (re-baseline G4b, decide the
> 57 deferred rows) was delivered in commit `1464a13`; only this clause is
> void, not the goal.

### Stale brief 4 — G4b-2 waiting on a field G7 was supposed to add

> Goal-list G4b-2 (`bac45243`) says to "land this after G7 and read the field
> G7 adds". **That half of the clause is VOID.** G7 (`2d0066f8`, commit
> 26f2dce) touched only `crates/xaft-tui` and added no `network_allow_list`;
> `GuardrailConfig` (`crates/xaft-config/src/types.rs:688`) still carries only
> its original six fields. Do not wait for a field that does not exist, and do
> not add it alone — G4b-2 builds the allow-list **and** a consuming
> `NetworkGuard` in the same increment, empty list denying by default. The
> correction is goal `1234fc86`; the per-row decisions for 123-127 are in
> [Part 2](#securitysettings-rows-123-127--decided-and-the-g7-owner-was-never-real).

## Part 1 — CLI: 21 open rows, 1 is a small increment

> **Superseded for twelve rows.** Once the operator authorized every agenthicc
> subsystem, the `parity/cli-family` branch closed rows 36, 40, 41 (`mcp`
> live), 46 (`skills add`), 47 (`trust`, under agenthicc HEAD's
> `list`/`add`/`revoke` shape), 50–53 (`worktrees`), 57 (`init`), 59
> (`agents`) and 60 (`attach`) — with the subsystems behind them, not as
> wrappers. The measurements below are kept as the record of why they were not
> small increments; [parity-matrix §1e](parity-matrix.md) records how each was
> closed, and §1c is the live open set.

Of the 21 open rows, exactly **one** family is a self-contained increment with
no subsystem behind it; the other **20** are blocked on capabilities xaft does
not have. Sizes below are measured line counts of the agenthicc source each row
actually needs, not of the thin CLI wrapper.

**The table below is wider than the open set, on purpose, so do not read it as
the count.** It lists **23 distinct rows**, and two of them — `workflows list` /
`run` (48, 49) — landed as ✅ after the table was written and are kept as the
measured record of a closed family. Every one of the 21 open rows appears
exactly once, except that `auth` (54, 55, 56) appears twice: the first row is
the original "no token store" reasoning and the second records the
re-measurement that moved the family from deferred to **declined** (see
[Why `auth` is declined](#why-auth-is-declined-not-deferred)). The live figure
is §1c's, and `cli-row-ownership.py --check` derives it from the rows.

| Family | Rows | Row range | Measured agenthicc size | xaft's gap |
|---|---|---|---|---|
| `init` | 1 | 57 | `project_bootstrap.py` 565; CLI wrapper 34 | none — xaft already has `default_config_template()` (`crates/xaft-cli/src/commands/config.rs:327`) |
| `mcp` live | 3 | 36, 40, 41 | `mcp.py` 255; `_live_cli_manager` at :151 | no `McpSessionManager`, no `NetworkGuard` (only a doc note in the `McpSubcommand` doc comment, `crates/xaft-cli/src/args.rs:1453`) |
| `session` service core | 3 | 8, 11, 12 | `session_service/` **2314** (`service.py` 951, `store.py` 453, `models.py` 363, `clients.py` 239, `transport.py` 250) | ✅ landed — `crates/xaft-session/src/service.rs` (`SessionService`, capability gates, durable event log; `SessionCommand` is a real envelope now, `crates/xaft-cli/src/commands/session_service.rs` drives it) |
| `session` command envelope | 2 | 9, 10 | same package | ✅ landed with the core above (`SessionSubcommand::{Send,Control}`, `session control` takes an open `kind` string) |
| `session serve` | 1 | 13 | same package (`transport.py` 250, HTTP/SSE) | ✅ landed — `crates/xaft-session/src/transport.rs` (`LocalSessionServer`: HTTP/1.1 hand-rolled on `tokio::net::TcpListener`, zero new crates) |
| `worktrees` | 4 | 50-53 | `worktrees/` **1566** (`ManifestStore`, `ParallelCoordinator`, `WorktreeManager`) | xaft uses git worktrees only for per-run branch isolation (`runtime.rs:833`); no `ParallelManifest`/`ParallelCoordinator` at all |
| `auth` | 3 | 54, 55, 56 | `auth.py` 328 — OAuth PKCE browser flow, keyring | no token store, no keyring crate, no agenthicc.ai account concept |
| `skills add` — **DECLINED** | 1 | 46 | `skills.py` 41; `skills/installer.py` **567** — local dir, GitHub raw URL, git clone, plus a 9-directory skill discovery walk | **declined (no authorization)** — remote fetch plus a trust boundary; see below |
| `trust cli` | 1 | 47 | `trust.py` 53 | blocked on the plugin loader, which does not exist |
| `workflows list/run` | 2 | 48, 49 | `workflows.py` — registry of plugin-defined workflow classes | xaft has a fixed `WorkflowMode` enum |
| `agents` | 1 | 59 | `background.py:225` — opens the background sessions manager TUI | no background-sessions manager |
| `attach` | 1 | 60 | `runs.py:87` — attaches the TUI to a background session | same |
| **`auth` — DECLINED** | 3 | 54, 55, 56 | `auth.py` 328 — OAuth PKCE browser flow against `agenthicc.ai` | **no account model and no auth server to authenticate against** (see below) |

### Why `auth` is declined, not deferred

Re-measured during G4b-8, and the original reason in this table ("no token
store, no keyring crate") was **the wrong blocker**. The cost is lower than
first recorded:

- `reqwest` with TLS (`hyper-tls`, `native-tls`, `openssl`, `rustls`) is
  **already in `Cargo.lock` and already compiled**, reached through
  `agtrs-openai` / `agtrs-anthropic`, which `xaft-runtime` depends on
  directly. The token exchange would add no crate.
- `tokio` is already a dependency with `features = ["full"]`, so a loopback
  HTTP/1.1 callback listener is `tokio::net::TcpListener` — no `axum`, no
  `hyper` server, no new crate. (This also corrects the premise of the
  `session serve` measurement, which found no HTTP *server* in the graph;
  that is still true, but it is not the same as having no HTTP at all.)
- Only **`keyring`** would be genuinely new, and it is absent from the lock.

The actual blocker is that **there is nothing to log into.** `auth.py`
hardcodes `AGENTHICC_AUTH_URL = "https://agenthicc.ai/oauth/authorize"`,
`AGENTHICC_TOKEN_URL`, and `AGENTHICC_REVOKE_URL`. In xaft,
`grep -rniE 'agenthicc\.ai|xaft\.(ai|dev|com)/' crates/*/src` returns
**0 hits**, and the only literal URLs in xaft's own crates are
`https://api.anthropic.com` and `https://api.openai.com/v1` (the
`http://localhost:11434/v1` that also appears is an `xaft config
add-provider` example for a local model server, not an account service).
Loosening the pattern to add a bare `account` yields 4 hits, all
incidental — two test names ending `_accounts_for_soft_wrap`, one doc
comment about column widths "accounts for", and one `xaft.commit_policy`
whose `xaft.com` matches the host pattern. xaft has no hosted service, no
account entity, and no user identity of any kind. `TokenBundle` carries `plan`
(`free|pro|enterprise`), `email` and `user_id` — fields with no xaft
counterpart to populate.

**Row 56 is not the 🟡 the goal text assumed.** It was described as a 🟡
"because xaft can already print local identity". Measured: the row is ❌, and
xaft has no local identity either — `xaft version` prints only
`xaft <version>`, and there is no user, device or account concept anywhere.
agenthicc's `whoami` prints `{email}  plan={plan}  token_expires={exp}`,
derived from the token bundle. A local `whoami` printing version, config path
and configured providers would be a **different document wearing the same
name** — exactly the false-parity shape that `parity-row-reachability.py`
exists to catch.

**Outcome: `declined (no authorization)`.** The operator was asked to choose
between authorizing the subsystem, a `whoami`-only increment, and declining;
**no answer was recorded within the deadline**, so the decision is recorded as
*unauthorized* rather than as a judgement that the feature is unwanted. This
mirrors the `declined (no authorization)` state of the seven config classes
in Part 2.

**No code was written, deliberately.** A `login` that prints
`Logged in as …` without holding a token is strictly worse than an absent
command, because it looks implemented. The existing `/login` and `/logout`
slash handlers (`crates/xaft-tui/src/slash/commands/login.rs`) are left
untouched: they already print `"/login: not yet connected to runtime."`, which
is an honest stub claiming no success, and changing them would alter
user-visible section-2 surface this goal does not own.

**No parity row status changes.** 54 and 55 remain 🟡 — the slash stubs
genuinely exist and are reachable, and their matrix cells already say "no CLI
subcommand". 56 remains ❌.

**Cheap to overturn.** Authorizing this later means standing up an account
model and an auth server first; only then is the PKCE client worth building.
The reverse is not true — there is nothing to unblock by writing the client
first.


Rows 47, 59 and 60 have a named owner in the goal list (G9 plugins and
background sessions), so they do **not** get a second goal. **Row 46 never had
one**: it used to be listed here under "G8 skills", but G8 was scoped to skill
*loading* plus AGENTS.md auto-loading and is [verified], so that pointer
dangled and the row was invisible — which is why row 46 was given its own goal
and why this document's owner list and `parity-matrix.md` §1c could disagree
without anything noticing. Rows 48 and 49 have since landed as ✅. Rows 46, 54,
55 and 56 are **declined** (see above) and so are not waiting on an increment at
all. For the authoritative open-row count and its owners, see
`parity-matrix.md` §1c — this document's per-family table is the analysis
behind it, not the index.

### Why `skills add` is declined, not deferred

> **Superseded:** authorized and implemented (`xaft_skills::installer`,
> `xaft skills add`); see the row-46 closure note in parity-matrix §1c.

Re-measured while recording row 46's owner. The earlier cost note in this
table ("needs the remote skill installer + transport") was **directionally
right and specifically wrong**, in a way that would have led an implementer to
budget the wrong thing.

`cli/commands/skills.py` really is trivial — **41 lines**, of which the body is
one `install_skills(...)` call and one print per result. The work is behind it:
`agenthicc/skills/installer.py` is **567 lines** with four source kinds.

| Source kind | agenthicc entry point |
|---|---|
| local file or directory | `_local_source:89`, `_install_content:369`, `_copy_skill_tree:406` |
| GitHub raw URL | `_github_raw_url:289`, `_remote_source:325` |
| **git clone** of a repository | `_GitRepositorySource:49`, `_repository_source:138`, `_clone_repository:150` |
| discovery walk over a cloned repo | `_discover_repository_skills:229` against `_KNOWN_SKILL_CONTAINERS` — **9** container dirs (`.agents/skills`, `.claude/skills`, `.cline/skills`, `.codex/skills`, `.github/skills`, `.opencode/skills`, `.openhands/skills`, `.roo/skills`, `.windsurf/skills`) at `_MAX_DISCOVERY_DEPTH = 5` |

**There is no archive format, so the goal text's "archive extraction" cost is
wrong.** agenthicc installs a *directory tree* or a *single markdown file*;
`grep -n "^name = \"zip\"\|^name = \"tar\"\|^name = \"flate2\"" Cargo.lock`
returns nothing, and no such crate is needed to match the behaviour.

What the remaining cost actually is:

- **HTTP is cheap but not free.** `reqwest` 0.12 and `rustls` are already in
  `Cargo.lock` and already compiled, via `agtrs-openai`, `agtrs-anthropic`,
  `agtrs-gemini`, `agtrs-ollama` and `agtrs-mcp`. However
  `grep -rn reqwest crates/*/Cargo.toml` returns **0 hits**, and
  `[workspace.dependencies]` declares no `reqwest` either — so a fetch is a new
  *direct* dependency declaration on `xaft-skills`. No new download; still a
  dependency decision, the class `session serve` (row 13) needed and answered
  without one — but that answer does not transfer: a skills fetch must speak
  TLS to a remote host, and no one should hand-roll TLS.
- **Git is the expensive half.** Neither xaft nor `agtrs-git` carries `git2` or
  `gix`, and `agtrs-git/Cargo.toml` lists neither a git nor an HTTP dependency.
  The clone path needs a new dependency or a shell-out to the system `git`.
- **The install target is not configurable.** `SkillLoader::for_working_dir`
  (`crates/xaft-skills/src/loader.rs:61-64`) hardcodes `{cwd}/.xaft/skills`
  and `~/.config/xaft/skills`; there is no config field for the search path
  (`default_skill_directory`, config row 160, is ❌). An installer writing
  anywhere else would install successfully and be invisible.

**The decisive objection is the trust boundary.** `load_all` feeds what it
reads from those two directories into the agent's system prompt on the next
session, so "install a skill" is code-execution-equivalent, not file copying.
agenthicc ships `trust cli` (row 47) as a **separate** command for exactly this
reason — a project-local skill/plugin loader is a trust boundary. xaft has no
such loader: `grep -rn "\.agenthicc/" crates/ --include=*.rs` returns **0
hits**. There is therefore no version of row 46 that is only transport; the
fetch path and the trust decision arrive together or not at all, and row 47 is
owned by G9 (plugins) and has not been built.

**Outcome: `declined (no authorization)`.** The operator was asked three
questions — implement vs decline, which source kinds to accept (local only /
+HTTP / +git clone), and whether installation should require an explicit trust
confirmation — and **no answer was recorded within the deadline**. So this is
recorded as *unauthorized*, not as a judgement that remote skills are
undesirable. Reopening it needs a yes, not another re-derivation.

**No code was written.** No `skills` subcommand, no `installer` module, no
dependency, no config field. A `skills add` that printed
`Installed skill foo (project) at …` without having fetched anything would be
strictly worse than an absent command, because it looks implemented.

**Row 46 stays ❌ and no parity row status changes.** The knock-on effect is
config row 159 (`install_default_skills`, default `True`): it needs an installer
xaft does not have, and it is now blocked on a *recorded* decision rather than
an open question. Row 160 (`default_skill_directory`) is untouched by this —
it is a loader-path field and remains live under the split recorded in
`docs/config-key-audit.md`.

### Why `worktrees` is not next

> **Superseded:** the layer is finished and wired (`WorktreeManager`,
> `ParallelCoordinator`, `xaft worktrees list|show|integrate|recover`); the
> agent-facing producer of orchestrations (agenthicc's `spawn_worker_agents`)
> is still unported, as parity-matrix §1e records.

`worktrees list` and `worktrees show` read a `ParallelManifest` store that
nothing in xaft writes. A read-only view of an always-empty store is not a
user-visible improvement, and `integrate`/`recover` additionally need
`WorktreeManager` to reconcile real git worktrees. The whole family is blocked
on the parallel-worker coordinator, not on the CLI.

## Part 2 — Config keys: 81 ❌, bucketed

The 81 uncovered config rows split into two very different groups.

### 57 deferred (whole subsystems; decided, closed to implementation)

Seven agenthicc classes with no xaft counterpart. Each now carries an
explicit **decision** in
[`config-key-audit.md`](config-key-audit.md#deliberately-deferred-classes) —
`declined (no authorization)` — which is a recorded outcome rather than an open
question: **no goal will implement these 57 keys, and no xaft config field will
be added for them, because no xaft code would read it.** Browser/CDP
automation (31), the loop runner (10), S3 storage (9), the hosted API client
(3), skills config (2) and remote storage backends (2). The rows stay ❌.

Two caveats a later pass must not lose. First, all seven decisions are
`declined` because **no operator authorization was recorded**, not because the
measurement recommends it — and for `LoopSettings` and `SkillsSettings` the
measurement points the other way. Second, the decision is about implementation
scope and is cheap to overturn: authorizing a class means building the
subsystem first, and only then adding the keys it will read.

### 24 non-deferred ❌ (bucketed; sums to 24)

Derived from the rows rather than by eye: `python3 scripts/config-key-audit.py
--check` reports `not covered = 81`, of which **57** are the seven deferred
subsystems above, leaving **24**. Those 24 split into **11 dead** rows (xaft
declares the field and nothing outside `crates/xaft-config` reads it) and **13
absent** rows (no xaft field at all).

| Bucket | Rows | Row range | Owner |
|---|---|---|---|
| **dead** — declared in `xaft-config`, read by no other crate | 10 | 48, 49, 60, 74, 110, 113, 114, 115, 116, 141 | goal `51b81db0` — wire each to a real consumer, then flip |
| **dead** — `PluginConfig` has no reader outside `xaft-config` | 1 | 180 | G9 (plugins) |
| **absent** — `MemorySettings` keys with a matching xaft struct | 2 | 121, 122 | goal `51b81db0` |
| **absent** — browser-subsystem pointers in `ToolSettings` | 3 | 117, 118, 119 | **declined** — the 31 deferred browser keys' decision covers these |
| section tables with no xaft section | 6 | 174, 175, 179, 181, 183, 185 | the subsystem each one names (`behaviour`, `hooks`, `api`, `skills`, `storage`, `loops`) |
| **declined** — agenthicc-internal / no xaft section | 2 | 75, 158 | **declined** |
| **sum** | **24** | | |

The **11 dead rows** are the ones that matter, because they violate the G4c rule
this section exists to enforce: xaft *declares* the field, so it looks
implemented, while nothing outside `crates/xaft-config` ever reads it. Ten are
the "declared but never read" class commit `444530d` taught
`scripts/config-key-audit.py` to detect — `authoring_max_generation_attempts`,
`authoring_max_phase_turns`, `profile`, `provider_capabilities`,
`ToolSettingsConfig.plugins`, `max_live_tool_calls`, `group_exploratory_calls`,
`http_timeout_s`, `question_timeout_s` and `confirm_exits` — each accepted by
`validate()` and then ignored. Row 180 (`plugins`) is dead for a different
reason: `PluginConfig`'s five fields have no reader outside `xaft-config` either,
which is precisely the finding `de5acc2` downgraded it on. **Goal `51b81db0`
owns the ten**; each gets a real consumer wired to it *before* its row flips,
never the other way round.

The **13 absent rows** have no xaft field to be dead. Two are `MemorySettings`
keys whose xaft struct exists (`vector_db`, `session_ttl_seconds`, owned by
`51b81db0`); three are the `ToolSettings` pointers into the browser/CDP
subsystem (`cloakbrowser`, `playwright`, `browser_backend`), which is declined
along with its 31 deferred keys; six are `AgenthiccConfig` section tables each
naming a subsystem xaft has no section for; and two (`_resolved_profile`,
`agents`) are agenthicc-internal or otherwise have no xaft equivalent. They are
declined rather than deferred: there is no subsystem that would ever read them,
so there is nothing to unblock.

**The three "cheap, genuinely user-facing" rows are gone from this table**, and
that is a closure rather than a reclassification: `AgentSettings.description`
(row 153) landed in `6d4cfa7`, `BehaviourSettings.verbose` (140) in `00f8621`,
and only `confirm_exits` (141) survives — as a *dead* row, because `TuiConfig`
declares it and no exit path consults it.

### `SecuritySettings` (rows 123-127) — decided, and the `G7` owner was never real

> **Status correction — this section was written while the rows were still ❌.**
> Commit `d001f86` later added the `[security]` section to `crates/xaft-config`
> and **all five rows are now ✅**: `python3 scripts/config-key-audit.py --check`
> reports `SecuritySettings  5 rows / 5 covered / 0 collision / 0 absent / 0
> deferred`, and `scripts/parity-row-reachability.py` proves all five reachable
> rather than merely declared. Nothing below is wrong as a *decision record* —
> the decline is what the operator was asked for and did not answer — but every
> sentence below that says the rows "stay ❌" describes a state that no longer
> holds. Read those as history, and read [§7 rows
> 123-127](parity-matrix.md) for the current status. The `G4b-2` correction at
> the bottom is still live and unaffected.

The owner cell above used to read `G7 (security/approval surface)`. **That
assignment dangled and never had substance.** G7's brief named "approval gates
and sandboxing" and `crates/xaft-config` among its targets, but G7
(`2d0066f8`, commit 26f2dce) touched **only** `crates/xaft-tui`. Rows 123-127
came out of it still ❌ — they were closed later by `d001f86`, not by G7 — and
the pointer was left naming a goal that is [verified] and closed. A later pass reading this table would look for the keys
in a finished goal and find nothing — so the pointer is replaced by a decision.

Measured, per key, from agenthicc's own source and xaft's own types:

| Row | Key | agenthicc default | What reads it in agenthicc | What xaft has today | Decision |
|---|---|---|---|---|---|
| 123 | `sandbox_mode` | `True` | gates the tool sandbox | nothing: xaft scopes the workspace at the store layer, not via a mode flag | **declined (no authorization)** — no consumer, so a field would be dead config |
| 124 | `allowed_paths` | `["/workspace"]` | path allow-list for that sandbox | the workspace root is already the boundary every fs tool is given | **declined (no authorization)** — overlaps existing enforcement |
| 125 | `network_allow_list` | `[]` | `NetworkGuard(network_allow_list or [])` at `tools/sandbox.py:137`; read again at `cli/commands/mcp.py:167` and `runners/tui_session.py:1124-1125` | **nothing** — `grep -rn "network_allow\|NetworkGuard" crates/ --include=*.rs` returns one hit, a doc note in the `McpSubcommand` doc comment (`crates/xaft-cli/src/args.rs:1453`) recording the absence | **declined (no authorization)** as a bare key; see the G4b-2 correction below |
| 126 | `max_tool_cpu_seconds` | `30` | per-tool CPU cap | no supervision point: tools run in-process | **declined (no authorization)** — a cap nothing enforces reads as enforced |
| 127 | `max_tool_memory_mb` | `512` | per-tool RSS cap | same as above | **declined (no authorization)** — same reason as 126 |

xaft's `GuardrailConfig` (`crates/xaft-config/src/types.rs:688`) has exactly
six fields — `file_destruction`, `secret_leakage`, `cost_limit`,
`command_approval`, `cost_limit_config`, `secret_leakage_config` — and none of
the five keys above.

**The operator was asked** whether to authorize each key as its own subsystem
goal or decline it, and **no answer was recorded within the deadline**, so the
safe default applies: all five are `declined (no authorization)`. That is the
same state as the seven deferred classes above — a recorded outcome, not a
judgement that the features are unwanted, and not a statement that no xaft code
will ever read them. **Nothing was added *by this goal***: no config field, no
guard, no code. The rows left this exercise at ❌, and `python3
scripts/config-key-audit.py --check` reported `SecuritySettings  5 rows / 0
covered / 0 collision / 5 absent / 0 deferred` — the reading this section was
written from. **`d001f86` has since added the `[security]` section and all five
rows are ✅**; see the status correction above.

**The `G4b-2` cross-reference is void, and corrected here.** G4b-2's brief
offers a choice — add the allow-list in that increment, "or land this after G7
and read the field G7 was supposed to add". No such field exists, so that half
of the instruction is void and a pass following it would stall — or worse, add
the field alone as "the config work", which is precisely the dead config this
document exists to prevent. The corrected instruction: **G4b-2
builds the allow-list AND a consuming `NetworkGuard` in the same increment,
with an empty list denying by default**, and it adds its own
`[guardrail] network_allow_list` field to `GuardrailConfig` rather than expecting
one to exist. It must never ship a bare key, and never a guard that allows
every host when the list is empty. The correction is recorded as its own goal
(`1234fc86`) so it is visible from the goal list and not only from here.

Declining row 125 while G4b-2 may later add it is consistent rather than
contradictory: the decline is about the **standalone config key**, and G4b-2's
guard would arrive with its own authorization.

**What would reverse any of these.** An explicit operator authorization, plus
the enforcement mechanism the key needs — a supervision point for 126/127, a
sandbox for 123/124, a guard for 125. Adding the field before the mechanism is
what makes a key look implemented when it is not, so the mechanism comes first
in every case.

## Increment order (user impact, cheapest-first within impact)

1. `init` — 1 row. Every new user hits it; xaft has no top-level `init`.
2. `mcp` live — 3 rows. Servers are configurable but not connectable.
3. cheap config keys — 3 rows. Trivial, and closes the last user-facing
   config gaps with no subsystem dependency.
4. ~~`session` service core — 3 rows. Largest structural gap (2314 lines).~~
   **Landed** (`crates/xaft-session/src/service.rs`).
5. ~~`session` command envelope — 2 rows. Depends on 4.~~ **Landed**
   (`SessionSubcommand::{Send,Control}`).
6. ~~`session serve` — 1 row. Depends on 4 (HTTP/SSE transport).~~ **Landed**
   (`crates/xaft-session/src/transport.rs`, `SessionSubcommand::Serve`);
   rows 8-13 are ✅.
7. `worktrees` — 4 rows. Blocked on the parallel-worker coordinator.
8. `auth` — 3 rows. Needs explicit authorization; zero value without an
   agenthicc.ai account concept.

## Related documents

* [`parity-matrix.md`](parity-matrix.md) — the inventory itself.
* [`config-key-audit.md`](config-key-audit.md) — generated per-row config diff.
