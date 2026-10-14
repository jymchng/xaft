# PRD 36 — `/compact` Command: Context Compaction

## Status: Draft
## Depends on: PRD 32a–32e, PRD 26 (conversation persistence)

---

## 1. Problem

`/compact` returns `"/compact: not yet connected to runtime."` and does
nothing. The intended behaviour — summarising the current conversation to
free up context-window space — requires emitting a signal that the running
`HandoffOrchestrator` can act on, or directly calling a compaction routine
on the `ConversationStore`.

---

## 2. Goals

1. `/compact` triggers a compaction of the active conversation: the
   assistant produces a rolling summary of messages older than the last N
   turns, then replaces those messages with the summary.
2. The user sees progress: a `"Compacting context…"` spinner line, then
   `"Context compacted — N messages → 1 summary (saved ~K tokens)."` on
   completion.
3. If no session is active the command prints `"No active session."`.
4. `/ctx` alias behaves identically.

---

## 3. Non-Goals

- Automatic compaction triggered by token thresholds (that belongs in the
  orchestrator/runtime layer).
- Compaction of tool-call chains (only user/assistant text messages for now).
- Streaming the summary back into the visible transcript.

---

## 4. Technical Design

### 4.1 Signal: `XaftCompactRequested`

Add to `crates/xaft-agent/src/signals.rs`:

```rust
#[derive(Clone, Debug)]
pub struct XaftCompactRequested {
    pub session_id: Uuid,
}
```

`CompactHandler` emits this via `CommandContext::signals`:

```rust
fn execute(&self, ctx: CommandContext) -> CommandResult {
    let Some(session_id) = ctx.session_id else {
        return CommandResult::Error("No active session.".into());
    };
    ctx.signals.emit(XaftCompactRequested { session_id });
    CommandResult::Lines(vec!["Compacting context…".into()])
}
```

### 4.2 Orchestrator subscription

`HandoffOrchestrator` subscribes to `XaftCompactRequested`. On receipt it:

1. Loads the current conversation via `ConversationStore::list_messages`.
2. Sends a summarisation request to the active agent's provider (a single
   no-tool, low-temperature call with a fixed system prompt:
   `"Summarise the following conversation in under 200 words…"`).
3. Replaces messages older than the last 6 turns with a single `System`
   message prefixed `[Context summary — auto-compacted at <timestamp>]`.
4. Emits `XaftCompactComplete { session_id, messages_removed, tokens_saved }`.

### 4.3 TUI bridge

`EventBridge` maps `XaftCompactComplete` → `TuiEvent::CompactComplete`.
`AppState` handles `TuiEvent::CompactComplete` by pushing a `CommitLine`
with the completion summary.

### 4.4 `CommandResult::AsyncStarted`

For the spinner use the existing `Lines` variant and let the `CompactComplete`
signal push the resolution line. No new variant needed.

---

## 5. Implementation Steps

1. Define `XaftCompactRequested` and `XaftCompactComplete` in
   `xaft-agent/src/signals.rs`.
2. Subscribe in orchestrator; implement compaction logic in
   `crates/xaft-runtime/src/orchestrator.rs`.
3. Bridge `XaftCompactComplete` in `crates/xaft-tui/src/bridge.rs`.
4. Handle `TuiEvent::CompactComplete` in `AppState`.
5. Update `CompactHandler::execute` to emit the signal.
6. Tests: `compact_handler_emits_signal`, `orchestrator_compacts_messages`,
   `compact_no_session_returns_error`.

---

## 6. Tests

```
test compact_handler_emits_xaft_compact_requested
test compact_handler_no_session_returns_error
test orchestrator_compaction_replaces_old_messages
test orchestrator_compaction_preserves_last_6_turns
test compact_complete_signal_reaches_tui
```
