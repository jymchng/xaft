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
see the goal appended for the upstream fix.