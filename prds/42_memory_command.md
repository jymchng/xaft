# PRD 42 — `/memory` Command: Memory Store Browser

## Status: Draft
## Depends on: PRD 32a–32e, PRD 24 (memory store), PRD 25 (xaft memory)

---

## 1. Problem

`/memory` returns `"No memory store connected."`. The memory store (PRDs
24/25) holds facts, user preferences, and project context that agents use
across sessions. Users have no way to inspect, search, or delete memories
from within the TUI.

---

## 2. Goals

1. `/memory` — list all memory entries (id prefix, type, one-line body).
2. `/memory <query>` — full-text search across memory entry bodies; show
   ranked results.
3. `/memory show <id>` — print the full body of one entry.
4. `/memory rm <id>` — delete a memory entry (with confirmation).
5. `/memory add <text>` — manually create a `user` type memory entry.

### Example: `/memory`

```
ID        TYPE       BODY
────────────────────────────────────────────────────────────────
a1b2c3    user       User prefers concise responses without trailing…
d4e5f6    project    Merge freeze begins 2026-03-05 for mobile release
g7h8i9    feedback   Never mock database in tests — past incident where…
```

---

## 3. Non-Goals

- Editing an existing memory entry in-place (delete + re-add).
- Exporting memories to a file.
- Memory types beyond `user`, `project`, `feedback`, `reference`.

---

## 4. Technical Design

### 4.1 `MemoryStore` trait in `CommandContext`

Add a `MemoryStore` abstraction to `xaft-tui/src/slash/mod.rs` (or import
from `xaft-session`):

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn list(&self) -> Result<Vec<MemoryEntry>>;
    async fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;
    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>>;
    async fn delete(&self, id: &str) -> Result<bool>;
    async fn create(&self, kind: MemoryKind, body: String) -> Result<MemoryEntry>;
}
```

`CommandContext` gains:
```rust
pub memory_store: Option<Arc<dyn MemoryStore>>,
```

### 4.2 Sub-command dispatch in `MemoryHandler`

Parse `ctx.args`:
- empty → `list`
- `show <id>` → `show`
- `rm <id>` → `rm` (with `requires_confirmation()`)
- `add <text>` → `add`
- anything else → `search`

### 4.3 List rendering

`CliTable::new(["ID", "TYPE", "BODY"])`. ID shown as 6-char prefix. Body
truncated to fit `terminal_cols - 6 - 10`.

### 4.4 Search

Delegate to `MemoryStore::search`. Results sorted by relevance (BM25
or LIKE match count). Display same table format as list.

### 4.5 Rm confirmation

Use `requires_confirmation()` pattern from PRD 39. Before deletion, show
the full entry body so the user can verify.

---

## 5. Implementation Steps

1. Define `MemoryEntry`, `MemoryKind`, `MemoryStore` trait in
   `xaft-session/src/memory.rs`.
2. Implement `SqliteMemoryStore` (CREATE TABLE memories with id, kind,
   body, created_at; FTS5 virtual table for search).
3. Implement `InMemoryMemoryStore` for tests.
4. Add `memory_store` field to `CommandContext`.
5. Wire `Arc<dyn MemoryStore>` through `TuiApp` → `AppState` →
   `build_command_context`.
6. Implement `MemoryHandler` with sub-command dispatch.
7. Tests (see §6).

---

## 6. Tests

```
test memory_handler_list_empty_store
test memory_handler_list_shows_entries
test memory_handler_search_filters_by_keyword
test memory_handler_show_known_id
test memory_handler_show_unknown_id_returns_error
test memory_handler_rm_deletes_entry
test memory_handler_rm_requires_confirmation
test memory_handler_add_creates_entry
test sqlite_memory_store_fts_search
```
