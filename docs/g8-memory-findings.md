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

1. **Authorization — asked SIX times, never answered.** agtrs is a SEPARATE
   WORKSPACE. The question has now been put to the operator in six separate
   passes and has never received an answer:

   | goal | id | question asked |
   |---|---|---|
   | 2 (G0b) | `9079d4d7` | edit agtrs in place / commit / wait? |
   | 18 | `7056fa3f` | apply the prepared injectable-rs port, then commit or leave uncommitted? |
   | 40 | `4c9d3ab8` | repoint the three pre-rename paths |
   | 77 | `87b5970a` | authorize the port, patch uncommitted, or retire agtrs from the critical path? |
   | 83 | `0419379c` | three questions, see below |
   | **115 (this one)** | `a0f0158f34d54fe39219e54d4ac0861b` | implement `expires_after` in both backends, or retire it as an API no store honours? |

   Goal 83 asked specifically: **(a)** authorize editing agtrs in place, and
   if so commit or leave uncommitted; **(b)** `count()`-only or `count()` +
   `list()`; **(c)** authorize the injectable-rs port as well, since without it
   the goal's own TEST clause cannot run (reason 2 below). The request
   (`ce8ff0ac125b4040a91a2f82edcad879`) **timed out unanswered after 300s**,
   with `repeated_timeout: false` — i.e. the operator has never seen an answer
   to this question, not that they declined it. **Nothing in agtrs was
   modified; `git status` there is clean at `8a26310`** and `HEAD` is
   `8a26310 feat: dynamic tool call, parallel tool call and meta`.

   This is a refusal to proceed on a **timeout**, not a decision. It is
   recorded here so the next pass does not read the open question as an
   implicit yes: nothing may be written to agtrs without an explicit answer.

   Goal 115 asked the same family of question and got the same non-answer. The
   field it is about is narrower than goal 83's six columns — see the third
   `MemoryFilter` note below — but the authorization question is identical, so
   it is logged here instead of as a separate reason. Request
   `a0f0158f34d54fe39219e54d4ac0861b` **timed out unanswered**, with
   `repeated_timeout: false`. The two options put to the operator were
   **implement `expires_after` in both backends** or **retire it as an API that
   no store honours**; with no answer, neither was done, and `git status` in
   agtrs is still clean at `8a26310`.

2. **agtrs does not build.** `cargo metadata` in /root/rust_projects/agtrs
   exits 101 — re-measured this pass:

   ```
   error: failed to load manifest for workspace member
     `/root/rust_projects/agtrs/agtrs`
   referenced by workspace at `/root/rust_projects/agtrs/Cargo.toml`
   Caused by: failed to load manifest for dependency `agtrs-graph`
   ```

   Root `Cargo.toml:37-39` still declare `injectable`, `injectable-runtime`
   and `injectable-graph` against the pre-rename `../injectable/injectable*`
   paths. So the upstream TEST clause — "a regression test asserts sqlite and
   in-memory agree for the same filter and fixture", and `cargo test -p
   agtrs-memory` exits 0 — **is not runnable** until the injectable-rs port
   lands (goal 18), which is itself unauthorized.
3. **Verifying against a sandbox clone would be a fake fix.** The
   `sandbox-build.sh` clone is discarded on teardown, so a change made only
   there would leave the real repo broken while making the suite look green.
   This is why the port must not be smuggled through the sandbox to get a
   green run.

#### A THIRD `MemoryFilter` — xaft's own copy never reaches a compiler (goal 115)

The note above says `expires_after` is honoured by **neither** backend. Re-read
this pass, a stronger statement is also true and is worth writing down: in agtrs
the field is *accepted, carried, and silently ignored*; in xaft it does not even
reach a compiler.

xaft's root crate declares its **own** `MemoryFilter` — `src/query.rs:12`, with
`expires_after` at `:32` and a builder agtrs lacks, `with_expires_after`, at
`:83`. That file is **orphaned**, not merely unused:

- No `mod query` declaration exists anywhere in the tree. `src/lib.rs` is a stub
  that says as much in its own first lines — "The actual logic lives in
  xaft-cli and xaft-runtime" — and `src/main.rs` declares only
  `mod bootstrap_tests` (`:132`). Nothing outside `target/` mentions the file.
- Rust dep-info agrees once the path is matched absolutely: xaft's own
  `src/query.rs` and `src/lifecycle.rs` occur **0** times in
  `target/debug/xaft.d` and `target/debug/libxaft.d`, while `src/main.rs` and
  `src/lib.rs` occur once each. A naive `grep -c 'src/query.rs'` returns 1 and
  is **misleading** — the hit is the agtrs clone's
  `.xaft-agtrs-sandbox/agtrs/agtrs-memory/src/query.rs`. The same trap makes
  `src/lifecycle.rs` look compiled, from agtrs's and `injectable-rs-runtime`'s
  own `lifecycle.rs`.
- Decisive probe, re-run at this HEAD: appending a deliberate syntax error to
  `src/query.rs` leaves `cargo check --bin xaft` at **exit 0**, because the file
  is in no target's fingerprint. The file was restored byte-identically
  (`sha256 03817a921c1781a7594b6285bed18637bc3f0b8ea446b90857146c72a50097fd`),
  and the check finished in 0.44s without rebuilding.

The same unwired class covers `crates/xaft-cli/src/worktrees/` (`error.rs`,
`model.rs`, `store.rs`): no `mod worktrees` declaration, and no compiled target
reads it.

Two consequences for the next pass. **First**, the "retire it as dead API"
option in the sixth ask splits in two: retiring xaft's copy needs no upstream
authorization at all, while retiring (or implementing) agtrs's copy still needs
an answer. **Second**, `tarpaulin-report.json` still describes `src/query.rs` as
coverable=11/covered=0, which now reads as a coverage gap; it is not one. That
report was produced at commit `3506e04`, not at `HEAD`, and its per-file shape
uses `traces`, not `line_coverage` — its totals are `covered=14180`,
`coverable=16507` (85.90%). Re-run coverage before trusting that file.

#### The measurements, re-verified this pass (unchanged)

Line numbers re-read from the real repo so the next pass cites current
anchors, not the ones quoted above from an older reading:

| | location | what it actually honours |
|---|---|---|
| `sqlite.rs list()` | `376`, SQL built at `377`, expiry at `379`, scope guard at `389` | `include_expired`; a scope clause **only when `scopes.len() == 1`** |
| `sqlite.rs count()` | `419`, SQL built at `420`, expiry at `422` | `include_expired` **only** |
| `in_memory.rs` | `filter_matches` at `344`, used by `list()` (`197`) and `count()` (`232`) | all six predicate columns |

`MemoryFilter` (`agtrs-memory/src/query.rs:12`) has eight fields:
`scopes`, `kinds`, `tags`, `source_agent`, `created_after`, `created_before`,
`expires_after`, `include_expired`. Note `expires_after` is honoured by
**neither** backend, so it is deliberately **not** one of the six columns a
parity fix must close — bringing sqlite to parity means matching
`filter_matches`, not exceeding it.

Re-measured at agtrs `HEAD 8a26310` (`git status` clean) this pass:
`expires_after` occurs **exactly once in the entire agtrs tree**, at that very
declaration (`agtrs-memory/src/query.rs:26`). There is no `with_expires_after`
builder — the only builders are `with_scope`, `with_scopes`, `with_kind`,
`with_tag`, `with_source_agent` — so the field can be set only through a
struct literal, and every store that receives it drops it. Also re-read:
`filter_matches` at `in_memory.rs:344`, called from `:156`, `:207`, `:240`;
`sqlite.rs::list()` at `:376` and `count()` at `:419` (expiry checks at
`:379` and `:422`; the `scopes.len() == 1` guard at `:389`).

#### What the authorized pass must do (decided now, so it is not re-litigated)

1. Fix **`list()` and `count()` together**. The `count()`-only variant is
   already ruled out by the table above: it would leave the two stores
   diverging on six fields, and `list()` is the more dangerous half because a
   2+-scope filter returns the *whole table*.
2. A multi-scope filter must become `scope_kind`/`scope_id` pairs OR-ed
   together (the union of those scopes), never an omitted predicate.
3. `Some(vec![])` for `tags` and `kinds` must match **nothing** on both
   backends — `filter_matches` defines an empty vector as matching nothing, and
   the xaft-side tests already pin that for `tags`.
4. Add an agtrs-memory regression test that runs the same fixture and filter
   through both backends per predicate column, so they cannot diverge again.
5. Then `cd /root/rust_projects/agtrs && cargo test -p agtrs-memory` must exit
   0 — which requires goal 18's port to have landed first.

The xaft-side workaround is merged and correct either way, so this is an
**upstream hygiene fix, not a blocker for xaft**: `cargo test -p xaft-memory`
is green without it, because d392b1b re-applies the tag predicate locally and
`count_project_memories` derives its count from a filtered `list()`. Both are
idempotent, so they stay correct after an upstream fix. No parity row's status
changes with this work.

### The duplicate `MemoryConfig`, and the memory TTL (goal 47)

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

#### COMMIT SCOPE — `07be681` also carries goal `7ee108d0`'s section-3c prose

`git show 07be681 --stat` lists `docs/parity-matrix.md` (+72/-…, 9 files
total), but this commit is the goal 47 memory-TTL work. The matrix hunk is
**not** goal 47's deliverable: it is goal `7ee108d0`'s section-3c rewrite —
the alias-shadowing decision table, the three `NOT AUTHORIZED` bullets, the
`ask`/`review` fail-closed analysis, and the
`plan_and_safe_never_permit_a_write_tool` reference. It was swept in because
the section-3c edits were sitting uncommitted in the working tree when this
commit was staged.

Nothing about goal 47 is wrong because of it. Goal 47's own TEST clause —
no parity row status changes — held: no `| N |` row cell differs in that
diff, and the numbered row count is 390 before and after. The defect is
attribution, not content.

**Read it this way.** A `git log --stat` reader who reaches `07be681` looking
for the mode-alias decision will not find it under the commit that made the
decision. The section-3c decision belongs to goal `7ee108d0`; `07be681` only
carried its prose. `git log -S two_of_the_six_registered_modes_are_shadowed_by_the_alias_table
-- docs/parity-matrix.md` names `07be681` as the matrix side, and `0d45a7c`
("rename the shadowing test to match the section-3c decision") as the test
side — the two halves of one decision landed in two unrelated commits, in
reverse order, with the test rename second.

This was a transient inconsistency and is now closed: as of `0d45a7c` the
committed `docs/parity-matrix.md` §3c and the committed test file name the
same test (`two_of_the_six_registered_modes_are_shadowed_by_the_alias_table`),
so the committed tree no longer documents a test that does not exist in the
committed tree. Recorded here only so the attribution is not re-derived — and
so the practice is visible: stage by path, never `git add -A`, because the
working tree carries other goals' uncommitted work.
