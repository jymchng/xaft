# PRD 38 — `/resume` + `/rewind` Commands: Session Navigation

## Status: Draft
## Depends on: PRD 32a–32e, PRD 26 (conversation persistence), PRD 24 (memory store)

---

## 1. Problem

Both `/resume` and `/rewind` return `"not yet connected to runtime."`. They
share the same dependency: access to the `SessionStore` and
`ConversationStore` via `CommandContext`. Neither is wired up.

- `/resume [session-id]` — reload a past session (full conversation history
  and working dir) into the current TUI context.
- `/rewind [N]` — roll back the active conversation to the message at index N,
  discarding everything after it.

---

## 2. Goals

### `/resume`

1. With no arg: print a table of the 10 most recent sessions (id prefix,
   created, task excerpt, message count, last model).
2. With a session-id prefix: restore that session — load its messages into
   the conversation store, switch `AppState::session_id`, and print a
   confirmation.
3. The visible transcript is cleared and re-populated with the restored
   history (most recent 20 messages shown; earlier lines appear in
   terminal scrollback).

### `/rewind`

1. With no arg: print the current conversation message list with indices.
2. `/rewind N`: delete messages from index N+1 onwards from the
   `ConversationStore` for the active session; print `"Rewound to message N."`.
3. The TUI transcript is NOT replayed (only future interactions use the
   shorter history) — this avoids duplicating rendered output.

---

## 3. Non-Goals

- Undo of a rewind.
- Resuming a session from a remote/different machine.
- Merging two sessions.

---

## 4. Technical Design

### 4.1 `CommandContext` additions

```rust
pub conversation_store: Option<Arc<dyn ConversationStore>>,
pub session_store:      Option<Arc<dyn SessionStore>>,
```

These are already present as `Option` fields (per the summary). The handlers
just need to use them.

### 4.2 `ResumeHandler`

```rust
fn is_async(&self) -> bool { true }

async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let session_store = ctx.session_store.ok_or_else(|| …)?;
    if ctx.args.trim().is_empty() {
        // list mode
        let sessions = session_store.list_recent(10).await?;
        let mut table = CliTable::new(["ID", "CREATED", "TASK", "MSGS", "MODEL"]);
        for s in sessions {
            table.push_row([&s.id[..8], &s.created_fmt, &s.task_excerpt, …]);
        }
        return CommandResult::StyledLines(table.render(ctx.terminal_cols));
    }
    // restore mode
    let prefix = ctx.args.trim();
    let session = session_store.find_by_prefix(prefix).await?;
    // Emit XaftSessionRestoreRequested — runtime handles hot-swap
    ctx.signals.emit(XaftSessionRestoreRequested { session_id: session.id });
    CommandResult::Lines(vec![format!("Restoring session {}…", &session.id[..8])])
}
```

### 4.3 Signal: `XaftSessionRestoreRequested`

The runtime subscribes, loads conversation messages, updates
`AppState::session_id`, and pushes a `ClearTranscript` + `CommitLine`
sequence for the restored messages.

### 4.4 `RewindHandler`

```rust
fn is_async(&self) -> bool { true }

async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let conv_store = ctx.conversation_store.ok_or_else(|| …)?;
    let session_id = ctx.session_id.ok_or_else(|| …)?;
    
    if ctx.args.trim().is_empty() {
        // list mode: print indexed messages
        let msgs = conv_store.list_messages(session_id).await?;
        let lines = msgs.iter().enumerate()
            .map(|(i, m)| format!("[{i:>3}] {role}: {excerpt}", …))
            .collect();
        return CommandResult::Lines(lines);
    }
    
    let n: usize = ctx.args.trim().parse()
        .map_err(|_| CommandResult::Error(format!("Invalid index: {}", ctx.args)))?;
    
    conv_store.truncate_after(session_id, n).await?;
    CommandResult::Lines(vec![format!("Rewound to message {n}.")])
}
```

### 4.5 `ConversationStore` addition

Add method to the trait:
```rust
async fn truncate_after(&self, session_id: Uuid, index: usize) -> Result<()>;
```

Implement for `SqliteConversationStore` with a DELETE WHERE rowid > N
query and for `InMemoryConversationStore` with a `truncate` call.

---

## 5. Implementation Steps

1. Add `XaftSessionRestoreRequested` signal; subscribe in runtime.
2. Implement `ResumeHandler::execute_async` (list + restore paths).
3. Add `ConversationStore::truncate_after`; implement for SQLite + in-memory.
4. Implement `RewindHandler::execute_async` (list + truncate paths).
5. Wire `conversation_store` and `session_store` into `CommandContext` in
   `AppState::build_command_context`.
6. Tests (see §6).

---

## 6. Tests

```
test resume_handler_no_arg_lists_recent_sessions
test resume_handler_prefix_emits_restore_signal
test resume_handler_unknown_prefix_returns_error
test rewind_handler_no_arg_lists_messages
test rewind_handler_n_truncates_conversation
test rewind_handler_invalid_index_returns_error
test conversation_store_truncate_after_removes_tail
```
