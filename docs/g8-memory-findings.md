
## Notes

### `SqliteMemoryStore::count()` ignores every filter except `include_expired`

MEASURED DURING G8 IMPLEMENTATION. `agtrs-memory/src/sqlite.rs:419` builds
`SELECT COUNT(*) FROM memory_entries WHERE 1=1` plus an expiry clause and never
reads `filter.scopes`, `filter.kinds` or `filter.tags` (nor the date filters).
`list()` in the same file does honour them, so the two disagree on the same
filter.

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

SCOPE: fix `count()` in agtrs-memory to apply the same filter predicates
`list()` does. That is a SEPARATE WORKSPACE (see the agtrs authorization goals)
and must not be edited from xaft without explicit authorization; the xaft-side
workaround is already merged and is correct either way, so this is an upstream
bug fix, not a blocker. Check the `in_memory.rs` store for the same defect while
there, and add a test to agtrs-memory that counts with a scope filter against
BOTH backends so the two stores cannot diverge again.

TEST: `count` with a `MemoryFilter` restricting to one scope returns only that
scope's rows on the sqlite AND in-memory stores; a regression test asserts
sqlite and in-memory `count` agree for the same filter and fixture; xaft's
`cargo test -p xaft-memory` stays green (it must pass both before and after the
upstream fix, since xaft no longer depends on the broken path).
