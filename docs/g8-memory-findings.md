## Notes

### `SqliteMemoryStore` ignores most `MemoryFilter` predicates — in `count()` AND `list()`

MEASURED DURING G8 IMPLEMENTATION. `agtrs-memory/src/sqlite.rs:419` builds
`SELECT COUNT(*) FROM memory_entries WHERE 1=1` plus an expiry clause and never
reads `filter.scopes`, `filter.kinds` or `filter.tags` (nor the date filters).

CONSEQUENCE IN XAFT, now worked around: `XaftMemoryManager::
count_project_memories` (crates/xaft-memory/src/manager.rs) called
`inner.count(&filter)`, so on the SQLite backend a workspace's own memory
total included every other workspace's memories sharing that database, plus all
session- and global-tier entries. `two_workspaces_sharing_one_database_stay_isolated_across_restarts`
in crates/xaft-memory/tests/memory_persistence.rs fails against the old code
(2 != 1) and passes against the fix (derive the count from `list()`, which
applies the scope filter).

WHY THE PRE-EXISTING TEST DID NOT CATCH IT: `memory_manager_project_scoping` in
crates/xaft-memory/tests/memory_e2e.rs builds its two managers with
`MemoryBackend::InMemory`, which are two SEPARATE stores, so isolation held
trivially and the shared-database case was never exercised. The SQLite backend
is the default (`MemoryConfig::default().backend == "sqlite"`).

#### CORRECTION — `list()` is broken too, and this note was wrong about it

An earlier version of this note stated that "`list()` in the same file does
honour them, so the two disagree on the same filter." **That is false**, and
believing it would have produced a `count()`-only fix that left the two stores
still diverging. Re-measured directly from the source:

| | `include_expired` | `scopes` (len 1) | `scopes` (len > 1) | `kinds` | `tags` | `source_agent` | `created_after`/`_before` |
|---|---|---|---|---|---|---|---|
| `sqlite.rs list()`   | yes | yes | **NO predicate at all** | no | no | no | no |
| `sqlite.rs count()`  | yes | **no** | no | no | no | no | no |
| `in_memory.rs` (via `filter_matches`) | yes | yes | yes | yes | yes | yes | yes |

Three consequences the original note missed:

1. **`in_memory.rs` is CORRECT** — its `count()` delegates to `filter_matches`
   (`in_memory.rs:240`), which honours all of the above. The defect is
   **sqlite-only**; the divergence is one-directional.
2. **`list()` ignores `filter.tags` entirely.** This is a live xaft-facing
   defect, not a hypothetical one: `XaftMemoryManager::list_project_memories`
   forwards a caller-supplied tag filter, and `crates/xaft-memory/src/tools/
   summarize.rs:97` calls it with user-supplied tags. On the DEFAULT backend,
   asking for memories tagged `arch` returned every memory in the workspace.
   Fixed on the xaft side (see below) rather than waiting on upstream.
3. **A multi-scope filter is silently unfiltered.** `sqlite.rs list()` guards
   its scope clause with `if scopes.len() == 1`, so a two-or-more-scope filter
   adds NO predicate and returns the whole table. A caller asking for
   "this scope OR that scope" got everything — the opposite of the requested
   filter, with no error.

#### Xaft-side fix merged here

`XaftMemoryManager::list_project_memories` now applies the tag predicate
**twice on purpose**: once handed to agtrs, and again locally over the
returned entries via `entry.metadata.has_any_tag(&tags)`. This is idempotent,
so it stays correct if agtrs is ever fixed. Its doc comment records that
`count_project_memories` is correct only because it passes no tag filter.

Guard test: `tag_filtering_works_on_the_sqlite_backend` and
`an_empty_tag_filter_matches_nothing_on_either_backend` in
crates/xaft-memory/tests/memory_persistence.rs, both against `MemoryBackend::Sqlite`.
They were **proven to fail without the fix** (an `["arch"]` filter returned
`left: 3, right: 2`), while the 10 pre-existing tests stayed green — the same
in-memory-only blind spot that hid the `count()` bug, one level up. The empty
filter case is pinned because `filter_matches` defines `Some(vec![])` as
matching nothing, and the backends must not disagree about that.

WHY THE TWO EXISTING TAG TESTS NEVER CAUGHT THIS: both
`list_project_memories_with_tags` (src/manager.rs) and
`multiple_tags_filter_correctly` (tests/memory_e2e.rs) build their managers
with `MemoryBackend::InMemory` — see `make_memory_manager()` at
memory_e2e.rs:13 — where `filter_matches` makes them pass.

#### Upstream fix: NOT DONE — authorization not given, and not currently testable

SCOPE, corrected from the original note: fixing `count()` alone is
insufficient. A real fix must bring `sqlite.rs` `list()` and `count()` to parity
with `in_memory.rs::filter_matches` across all six columns above, including the
`scopes.len() == 1` guard that silently drops wider filters.

THREE REASONS IT WAS NOT DONE HERE:

1. **Authorization.** agtrs is a SEPARATE WORKSPACE (see the agtrs
   authorization goals 2, 18 and 77). The operator was asked directly during
   this pass — whether to edit agtrs in place, whether to commit, and whether
   to fix `count()` only or `count()` + `list()` — and **did not answer before
   the deadline**. Nothing in agtrs was modified; `git status` there is clean
   at `8a26310`.
2. **agtrs does not build.** `cargo metadata` in /root/rust_projects/agtrs
   exits 101: root `Cargo.toml:37-39` still declare `injectable`,
   `injectable-runtime` and `injectable-graph` against the pre-rename
   `../injectable/injectable*` paths. So the upstream TEST clause — "a
   regression test asserts sqlite and in-memory `count` agree for the same
   filter and fixture" — **is not runnable** until the injectable-rs port lands
   (goal 18). This was the fourth time this question was put to the operator.
3. **Verifying against a sandbox clone would be a fake fix.** The
   sandbox-build.sh clone is discarded on teardown, so a change made only
   there would leave the real repo broken while making the suite look green.

The xaft-side workaround is merged and correct either way, so this remains an
upstream bug fix and not a blocker for xaft. Tracked for the authorized pass;
see the goal appended for the upstream fix.### The duplicate `MemoryConfig`, and the memory TTL (goal 47)

MEASURED, and one premise in the original goal briefing was WRONG. It cites
`crates/xaft-config/src/types.rs:224` as the user-facing `MemoryConfig`, but
line 224 is `CoreConfig`. The real declaration is at **types.rs:312** — and it
is one of 40 config structs that `scripts/config-key-audit.py` scans, so the
mis-cited line number would have sent the next pass to the wrong struct.

THE TWO DECLARATIONS, as measured:

| | `xaft_config::MemoryConfig` (types.rs:312) | `xaft_memory::MemoryConfig` (config.rs:10) |
|---|---|---|
| `enabled` | `bool` | `bool` |
| `backend` | **`String`** | **`MemoryBackend`** enum |
| `auto_remember` / `auto_summarize` / `project_scope_default` | `bool` | `bool` |
| `max_entries` | `Option<usize>` | `Option<usize>` |
| `max_search_results` | `usize` | `usize` |
| `auto_remember_ttl_secs` | **absent** | `Option<u64>` |
| `project_memory_path` | `Option<String>` | **absent** |

So the drift runs in BOTH directions, not only the one the briefing named: the
runtime type had a field the config type lacked, and the config type had a
field the runtime type lacked.

#### `auto_remember_ttl_secs` was not merely unreachable — it was DEAD

`grep -rn auto_remember_ttl_secs` over `crates/xaft-memory/src` returns **zero**
hits outside its own declaration. Nothing read it. `src/main.rs` hardcoded the
runtime value to `None`. So the field was unreachable from `xaft.toml` AND
inert in the runtime: a knob that looked implemented and did nothing, which
G4c's rule ("a config field nothing reads is dead config and is worse than a
missing one") exists to catch. Adding the user-facing key alone would have
made this WORSE — a second dead field, now one a user could type.

The fix wires both halves: `xaft_config::MemoryConfig` gains the key, and
`XaftMemoryManager::remember_with_agent` stamps
`MemoryEntry::with_expiry(Utc::now() + ttl)`. No agtrs change was needed —
expiry is stored as `expires_at` on the entry and is already honoured by both
stores (`sqlite.rs` gates on `expires_at IS NULL OR expires_at > now`;
`in_memory.rs` via `is_expired()`), so the TTL works identically on the default
SQLite backend and on the in-memory one. That parity is asserted on both, since
a TTL honouring only one backend would be the same cross-backend divergence
goal 46 was about.

#### DECISION 1 — both types are KEPT; a full merge is too invasive

The goal offered "merge into one type" or "keep a runtime type with a guard".
**Kept**, because merging `backend` into a single type is not a mechanical
collapse:

- `xaft_config::MemoryConfig.backend` is a `String` because the TUI config
  menu (crates/xaft-tui/src/menu/config_menu.rs) reads and writes
  `config.memory.backend` as a `ConfigValue::Str`. Making the config type
  carry the enum changes that menu's field type and its setter.
- `project_memory_path` has NO runtime counterpart — it is consumed by
  `src/main.rs` to locate the SQLite store, never by `XaftMemoryManager`. A
  merged type would carry a field the runtime must ignore, reintroducing the
  same asymmetry in the other direction.

So the merge was declined as a behaviour change, and the goal's stated
fallback was taken instead: a **structural guard** that makes the divergence
impossible to introduce silently.

#### DECISION 2 — the guard reads the source, it is not a hand-kept list

`crates/xaft-memory/tests/config_parity.rs` parses the field list out of BOTH
struct definitions and asserts, in both directions, that a field is either
present on the other side or covered by `RUNTIME_ONLY_FIELDS` with a written
justification. A hand-maintained list would have reproduced the original bug —
two lists that drift. The allow-list has exactly ONE entry (`backend`, with
its type-difference reason), and a test fails if it ever names a field the
runtime struct does not have.

It also guards `src/main.rs`, which is the one place the two types are joined
and which has NO tests of its own (the binary crate is 0/70 covered). Two
tests parse that mapping: one asserts no runtime field is fed a hardcoded
literal, and one pins `auto_remember_ttl_secs` specifically. Both were proven
to fail by reverting the fix — and the general one was proven against a field
OTHER than the TTL, to show it is a structural check and not a string match on
the one field this goal happened to touch.

Three bugs were caught in the guard itself while writing it, all worth
recording because each would have produced a test that could never fail:

1. The allow-list test initially asserted an exempt field must be ABSENT from
   the config struct. `backend` is present on both sides — only its TYPE
   differs — so the assertion failed on the very entry that motivated the list.
2. The hardcode check initially required `config.memory.backend` on the same
   line as `backend:`, but `main.rs` converts it a few lines above into a
   local. It now searches the whole memory bootstrap and additionally proves
   each exempt field is traceable to the user config there.
3. `MemoryConfig` is NOT re-exported from `xaft-config`'s crate root; the path
   is `xaft_config::types::MemoryConfig`.

#### DECISION 3 — the TTL default is `None`, NOT agenthicc's 86400

agenthicc's `MemorySettings.session_ttl_seconds` defaults to **86400**. xaft's
key defaults to `None` (never expire), deliberately. agenthicc applies its TTL
to session-tier memory; this key applies to EVERY newly remembered entry, so
defaulting to 86400 would begin silently deleting long-lived project facts on
day one for every existing user. Making that change is a product decision, not
a parity fix, so it was not made unilaterally. `no_ttl_configured_means_entries_never_expire`
pins the default so an accidental expiry-everything regression fails loudly.

This question was put to the operator and did not get an answer before the
deadline, as did the merge-vs-guard question and whether row 122 should flip.
The safe option was taken in each case.

#### Row 122 stays ❌ — the near-miss is now CLOSER, not closed

`session_ttl_seconds` is still ❌, and that is honest. agenthicc's key names
**session**-scoped TTL; xaft's names **auto-remember** TTL, which is a
different scope, and the defaults differ (86400 vs `None`). Recording it as a
rename in `scripts/config-key-audit.py` would assert the two are the same
capability, which is a parity judgement the operator did not get to make. The
matrix's existing near-miss note now overstates the problem, so it is corrected
below rather than left to claim the key does not exist.