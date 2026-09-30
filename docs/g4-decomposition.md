# G4 decomposition (measured)

<!-- Analysis backing the per-family increments in the goal list. Not generated. -->

G4 ("close CLI command and config-key parity gaps") is an umbrella, not a
deliverable. This note records what is actually left, how big each piece is,
and who owns it, so the umbrella is not re-derived from scratch on every pass.

Row numbers refer to [`parity-matrix.md`](parity-matrix.md). Config-key row
numbers are 1-185 in section 7; CLI row numbers are 1-60 in section 1.

## Baseline correction

> **G4b's original brief is SUPERSEDED and cannot be edited.** Goal text is
> immutable once written, so G4b (goal-list index 7) still reads "185
> config-key rows (128 🟡 + 57 ❌)" and "28 ❌ CLI rows". **Do not work from
> that text.** It is reproduced here only so the next pass can see what was
> replaced, and by what. Every number below is measured, and both checkers
> re-derive them from the rows.

G4b's original brief counted **213 open rows** (28 ❌ CLI + 185 config keys =
128 🟡 + 57 ❌). That number is obsolete:

* The 128 🟡 were provisional placeholders, not measured gaps. The field-level
  diff in [`config-key-audit.md`](config-key-audit.md) (commit `ada5968`)
  resolved them: the config section is now **39 ✅ / 0 🟡 / 146 ❌**. On the CLI
  side, of the command families named in G4b's original brief, `config profiles`
  (row 4), `runs list|show|cancel|resume` (42-45), `sessions export|inspect`
  (16, 17) and all seven config-only `mcp` commands (32-35, 37-39) have landed,
  which is why the brief's 213 open rows is now 170.
* The brief's "43 keys already match by exact name" was inflated to 24 by
  cross-class coincidences. See the audit for the four that mattered.

**Actually open: 23 CLI rows and 146 config rows.**

The config figure moved once more since `ada5968`, which is worth noting
because it is exactly the drift this section exists to prevent: G4c recorded
**38 ✅ / 147 ❌**, and row 120 (`MemorySettings.project_memory_path`) has since
flipped ❌→✅ in G8 (`6f759d5`), making it **39 ✅ / 146 ❌**. A brief that says
"90 non-deferred ❌" is therefore one row out of date; the correct figure is 89.
Re-derive with `python3 scripts/parity-matrix-stats.py --check` rather than
trusting any number written here, including this one.

## Part 1 — CLI: 23 open rows, 1 is a small increment

Of the 23 open rows, exactly **one** family is a self-contained increment with
no subsystem behind it. The other 22 are blocked on capabilities xaft does not
have. Sizes below are measured line counts of the agenthicc source each row
actually needs, not of the thin CLI wrapper.

| Family | Rows | Row range | Measured agenthicc size | xaft's gap |
|---|---|---|---|---|
| `init` | 1 | 57 | `project_bootstrap.py` 565; CLI wrapper 34 | none — xaft already has `default_config_template()` (`crates/xaft-cli/src/commands/config.rs:327`) |
| `mcp` live | 3 | 36, 40, 41 | `mcp.py` 255; `_live_cli_manager` at :151 | no `McpSessionManager`, no `NetworkGuard` (only a doc note at `args.rs:1210`) |
| `session` service core | 3 | 8, 11, 12 | `session_service/` **2314** (`service.py` 951, `store.py` 453, `models.py` 363, `clients.py` 239, `transport.py` 250) | no service: xaft's `SessionCommand` (`args.rs:362`) is a clap enum, not agenthicc's command envelope; no capability gates, no durable event log |
| `session` command envelope | 2 | 9, 10 | same package | same |
| `session serve` | 1 | 13 | same package (`transport.py` 250, HTTP/SSE) | no attachment transport |
| `worktrees` | 4 | 50-53 | `worktrees/` **1566** (`ManifestStore`, `ParallelCoordinator`, `WorktreeManager`) | xaft uses git worktrees only for per-run branch isolation (`runtime.rs:833`); no `ParallelManifest`/`ParallelCoordinator` at all |
| `auth` | 3 | 54, 55, 56 | `auth.py` 328 — OAuth PKCE browser flow, keyring | no token store, no keyring crate, no agenthicc.ai account concept |
| `skills add` | 1 | 46 | `skills.py` wrapper over `install_skills(...)` | needs the remote skill installer + transport |
| `trust cli` | 1 | 47 | `trust.py` 53 | blocked on the plugin loader, which does not exist |
| `workflows list/run` | 2 | 48, 49 | `workflows.py` — registry of plugin-defined workflow classes | xaft has a fixed `WorkflowMode` enum |
| `agents` | 1 | 59 | `background.py:225` — opens the background sessions manager TUI | no background-sessions manager |
| `attach` | 1 | 60 | `runs.py:87` — attaches the TUI to a background session | same |

Rows 46, 47, 48, 49, 59 and 60 already have a named owner in the goal list
(G8 skills, G9 plugins/background sessions, G10 workflows), so they do **not**
get a second goal. That leaves 17 rows needing new increments.

### Why `worktrees` is not next

`worktrees list` and `worktrees show` read a `ParallelManifest` store that
nothing in xaft writes. A read-only view of an always-empty store is not a
user-visible improvement, and `integrate`/`recover` additionally need
`WorktreeManager` to reconcile real git worktrees. The whole family is blocked
on the parallel-worker coordinator, not on the CLI.

## Part 2 — Config keys: 146 ❌, bucketed

The 146 uncovered config rows split into two very different groups.

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

### 89 non-deferred ❌ (bucketed; sums to 89)

| Bucket | Rows | Row range | Owner |
|---|---|---|---|
| `SecuritySettings` | 5 | 123-127 | G7 (security/approval surface) |
| `ToolSettings` | 10 | 110-119 | G6 (tool surface) |
| `PluginSettings` | 9 | 131-139 | G9 (plugins) |
| `MemorySettings` | 2 | 121-122 | G8 (memory) |
| `resume_transcript_turns` | 1 | 142 | G8 (`--resume` transcript replay) |
| section tables with no xaft section | 7 | 172, 174, 175, 179, 181, 183, 185 | the subsystem each one names (`execution`, `behaviour`, `hooks`, `api`, `skills`, `storage`, `loops`) |
| **cheap, genuinely user-facing** | **3** | **140, 141, 153** | **new increment** |
| agenthicc-internal / provider-execution detail | 52 | 1-5, 8, 10-11, 16-18, 21-26, 33-35, 40-60, 67-69, 73-77, 156-158 | **declined** |

The 52-row tail is `prompt_cache`, `file_cache`, `context_windows`,
`transport_retry_*`, `_resolved_profile`, `client_options`/`request_options`,
`max_completion_tokens`, `protocol`, `capabilities`, `session_header` and
similar — fields agenthicc uses internally that xaft has no equivalent
machinery for. They are declined rather than deferred: there is no subsystem
that would ever read them, so there is nothing to unblock.

The 3 cheap rows are real gaps with no subsystem behind them:
`AgentSettings.description` (row 153 — xaft's `AgentPreset` has 14 fields and
none of them is a description), and `BehaviourSettings.verbose` (140) and
`confirm_exits` (141) — `CoreConfig` has 5 fields and `TuiConfig` has 16,
neither carries either toggle (`grep -rn "confirm_exits" crates/` → 0 hits).

## Increment order (user impact, cheapest-first within impact)

1. `init` — 1 row. Every new user hits it; xaft has no top-level `init`.
2. `mcp` live — 3 rows. Servers are configurable but not connectable.
3. cheap config keys — 3 rows. Trivial, and closes the last user-facing
   config gaps with no subsystem dependency.
4. `session` service core — 3 rows. Largest structural gap (2314 lines).
5. `session` command envelope — 2 rows. Depends on 4.
6. `session serve` — 1 row. Depends on 4 (HTTP/SSE transport).
7. `worktrees` — 4 rows. Blocked on the parallel-worker coordinator.
8. `auth` — 3 rows. Needs explicit authorization; zero value without an
   agenthicc.ai account concept.

## Related documents

* [`parity-matrix.md`](parity-matrix.md) — the inventory itself.
* [`config-key-audit.md`](config-key-audit.md) — generated per-row config diff.
