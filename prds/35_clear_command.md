# PRD 35 — `/clear` Command: Wipe Visible Transcript

## Status: Draft
## Depends on: PRD 32a–32e (slash command system)

---

## 1. Problem

`/clear` currently returns the string `"Session display cleared."` but does
not actually remove any lines from the transcript. The rendered output stays
unchanged; only a new line is appended. The command has no effect.

The user expects `/clear` to wipe the visible transcript (analogous to
`clear` in a shell), leaving a clean slate — though the underlying
conversation history for the LLM context should remain intact by default
(separate from the display layer).

---

## 2. Goals

1. `/clear` erases all lines in the visible transcript region immediately.
2. A single dimmed footer line `"Transcript cleared."` is printed afterwards
   so the user knows the command ran.
3. The LLM conversation history in `ConversationStore` is **not** touched —
   only the rendered surface is cleared.
4. `/cls` (alias) behaves identically.

---

## 3. Non-Goals

- Clearing conversation history or context window (that is `/compact`).
- Confirmation prompt before clearing.
- Undo / restore of cleared content.

---

## 4. Technical Design

### 4.1 New `RenderMutation` variant

Add `ClearTranscript` to the `RenderMutation` enum in
`crates/xaft-tui/src/transcript.rs`:

```rust
pub enum RenderMutation {
    // existing …
    ClearTranscript,
}
```

`IncrementalRenderer::apply` handles `ClearTranscript` by:
1. Writing `\x1b[2J\x1b[H` (erase display, cursor home) via `crossterm`.
2. Resetting its internal `line_count` / scroll-offset to 0.
3. Immediately redrawing the input prompt so the input bar is visible again.

### 4.2 `CommandResult` routing in `AppState`

`CommandResult::Handled` already exists for commands that produce side
effects rather than output lines. `ClearHandler` should return
`CommandResult::Handled` with a new variant or piggyback on a sentinel
`Lines` result. The cleanest path:

Add `CommandResult::ClearTranscript` alongside the dimmed line:

```rust
pub enum CommandResult {
    // existing …
    ClearTranscript { footer: String },
}
```

`AppState::handle_slash_parse_result` matches on `ClearTranscript`:
```rust
CommandResult::ClearTranscript { footer } => {
    self.mutations.push(RenderMutation::ClearTranscript);
    self.mutations.push(RenderMutation::CommitLine(
        StyledLine::new(footer, LineKind::SystemMessage),
    ));
}
```

### 4.3 `ClearHandler` update

```rust
impl SlashHandler for ClearHandler {
    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::ClearTranscript {
            footer: "Transcript cleared.".into(),
        }
    }
}
```

---

## 5. Implementation Steps

1. Add `ClearTranscript` variant to `RenderMutation`.
2. Add `ClearTranscript { footer: String }` variant to `CommandResult`.
3. Implement `ClearTranscript` handling in `IncrementalRenderer::apply`.
4. Match `ClearTranscript` in `AppState::handle_slash_parse_result`.
5. Update `ClearHandler::execute`.
6. Write unit test: `clear_command_emits_clear_transcript_mutation`.

---

## 6. Tests

```
test clear_command_returns_clear_transcript_result
test clear_command_does_not_affect_conversation_history
test cls_alias_clears_transcript
test renderer_apply_clear_transcript_resets_line_count
```
