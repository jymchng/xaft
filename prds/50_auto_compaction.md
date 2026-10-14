# PRD 50 — Auto-Compaction (F19): Context-Window Overflow Handling

## Status: Draft
## Depends on: PRD 09 (runtime), PRD 10 (TUI arch), PRD 35 (/clear command)

---

## 1. Problem

Every LLM has a finite context window. Claude claude-sonnet-4-6 supports up to
200 000 tokens; even so, a long xaft session — several rounds of file-reading,
code-editing, and QA — can exhaust it. When the context is full, the LLM call
fails with an `overloaded` / `context_length_exceeded` error that surfaces as
`RuntimeError::Agent("…")`, terminating the task with no useful output.

The user's only recourse today is:
- Start a fresh session (`xaft run "new task"`) and lose all prior context.
- Manually shorten the task description (not always possible).
- There is a stub `/compact` command that returns
  `"/compact: not yet connected to runtime."` — never implemented.

Claude Code solves this by automatically summarising older messages when the
conversation approaches the context limit, and by exposing `/compact` as a
manual trigger. xaft needs the same.

---

## 2. Goals

1. **Automatic compaction**: when an agent's running context (measured by the
   most recent `input_tokens` from `ModelCallComplete`) exceeds
   `threshold_pct × context_window` (default 80 %), compact before the next
   LLM call.
2. **Manual compaction**: `/compact` triggers compaction immediately, producing
   a visible confirmation line in the TUI transcript.
3. **Lossless persistence**: the full pre-compaction message history is saved
   under a snapshot key in `ConversationStore` before any messages are dropped.
4. **Tool-result integrity**: compaction never splits a `ToolUse` /
   `ToolResult` pair.
5. **Recent-turn preservation**: the last `keep_recent_turns` complete user +
   assistant turns (including their tool interactions) are always kept verbatim.
6. **Observability**: a `ContextCompacted` signal is emitted, carrying
   `messages_removed`, `tokens_before`, and `tokens_saved` so the TUI can show
   a `[compact]` marker and cost/token panels can reflect the change.
7. **Config-driven**: all thresholds are configurable via `[compaction]` in
   `xaft.toml` and default-safe (feature is on by default).

---

## 3. Non-Goals

- Compacting messages across agent handoffs (each agent manages its own
  context independently; the planner's context is separate from the coder's).
- Semantic "smart" selection of which messages to drop (we always drop the
  oldest, by turn boundary, after the keep window).
- UI replay of a compacted session's dropped messages (the snapshot key is for
  recovery only; the TUI does not surface a "show full history" button).
- Reducing the cost of the session (compaction adds one summarisation LLM call;
  net savings come from shorter subsequent prompt tokens).
- Compacting the `prior_messages` that are loaded at `--continue` time (those
  are injected once at session start and then managed by the in-memory
  conversation).

---

## 4. Architecture

### 4.1 Overview

```text
XaftAgent::before_llm_call (in xaft-agent/src/agent.rs)
  │
  ├── read current context token count from AgentContext
  ├── compare: input_tokens / context_window > threshold_pct ?
  │    No → proceed normally
  │    Yes → call Compactor::compact(ctx, llm, conv_store, config, signals)
  │             │
  │             ├── 1. identify boundary (keep last N turns)
  │             ├── 2. save full history to snapshot key (ConversationStore)
  │             ├── 3. summarize older messages via one LLM call
  │             ├── 4. replace ctx.history with [system(summary)] + recent_turns
  │             └── 5. emit XaftContextCompacted signal
  │
  └── LLM call proceeds with compacted context
```

### 4.2 `Compactor` — `crates/xaft-runtime/src/compactor.rs` (new file)

```rust
/// Compacts an agent's in-memory message history to free context-window space.
pub struct Compactor {
    /// Maximum fraction of the context window before compaction triggers.
    pub threshold_pct: u8,            // 0–100, default 80
    /// Number of complete user+assistant turns to preserve verbatim.
    pub keep_recent_turns: usize,     // default 4
    /// Maximum tokens the LLM may produce for the summary call.
    pub summary_max_tokens: usize,    // default 1024
}

impl Compactor {
    pub fn from_config(cfg: &CompactionConfig) -> Self { … }

    /// Run compaction on `messages`, using `llm` to produce the summary.
    ///
    /// Returns the compacted `Vec<Message>` and a `CompactionResult` with stats.
    pub async fn compact(
        &self,
        messages: Vec<Message>,
        agent_name: &str,
        llm: Arc<dyn LlmProvider>,
        conv_store: Option<Arc<dyn ConversationStore>>,
        conv_key: &str,
        signals: Arc<SignalBus>,
    ) -> Result<(Vec<Message>, CompactionResult), RuntimeError> { … }
}

pub struct CompactionResult {
    pub messages_before: usize,
    pub messages_after:  usize,
    pub tokens_before:   usize,      // estimated from last ModelCallComplete
    pub chars_saved:     usize,      // characters removed
    pub summary_len:     usize,      // characters in the generated summary
    pub triggered_by:    CompactionTrigger,
}

pub enum CompactionTrigger { Auto, Manual }
```

### 4.3 Turn-boundary identification

A "turn" is the atomic unit preserved or dropped. A complete turn consists of:

```
[User message]
  (optional) [Assistant: ToolUse blocks]
  (optional) [Tool: ToolResult blocks, one per tool call]
[Assistant: final text response (no ToolUse)]
```

The algorithm:

```rust
fn find_boundary(messages: &[Message], keep_recent_turns: usize) -> usize {
    // Walk backwards from the end, counting complete turns.
    let mut turns_kept = 0usize;
    let mut boundary = messages.len();  // index from which to KEEP

    let mut i = messages.len();
    while i > 0 {
        i -= 1;
        // A turn starts with a User message.
        if messages[i].role == Role::User {
            turns_kept += 1;
            if turns_kept >= keep_recent_turns {
                boundary = i;
                break;
            }
        }
    }
    boundary
}
```

**Invariant**: the boundary always lands on a `Role::User` message. Messages
from `0..boundary` (exclusive) are candidates for summarisation. Messages from
`boundary..` are kept verbatim. If `turns_kept < keep_recent_turns` (the
history is shorter than the keep window), no compaction is performed even if
the threshold is exceeded.

### 4.4 Snapshot persistence

Before any messages are dropped, the current full history is saved to the
`ConversationStore` under a snapshot key that includes a timestamp:

```rust
let snapshot_key = format!(
    "{}::snapshot::{}",
    conv_key,
    chrono::Utc::now().timestamp_millis()
);
if let Some(ref store) = conv_store {
    let _ = store.save(&snapshot_key, &messages).await;
}
```

Snapshots are not automatically pruned; a future `/cleanup-snapshots` command
or periodic session maintenance can delete them. The normal resume flow
(`--continue`) loads from the primary `conv_key`, never a snapshot key.

### 4.5 Summarisation call

Older messages (indices `0..boundary`) are formatted into a prompt and sent to
the LLM via a lightweight `one_shot_completion` call (no tool use, no agent
lifecycle):

```
SYSTEM:
  You are summarizing an AI coding session for context compression.
  Produce a dense, structured summary covering:
  - The user's original task and goals
  - Key decisions made and their rationale
  - Files read, edited, or created (paths + brief description of changes)
  - Test outcomes and QA results
  - Any unresolved issues or open questions
  Write in third-person past tense. Be specific; include file paths and function
  names. Maximum {{ summary_max_tokens }} tokens.

USER:
  <conversation>
  {{ formatted_older_messages }}
  </conversation>
  Summarize the above conversation.
```

The resulting summary is wrapped in a `Message::system()`:

```rust
Message::system(format!(
    "[CONTEXT SUMMARY — {} messages compacted]\n\n{}",
    messages_compacted_count,
    summary_text
))
```

This is prepended to the kept recent turns, forming the new history:

```
[system: CONTEXT SUMMARY …]        ← generated
[user: turn N-3]                   ← kept verbatim
  [assistant + tools: turn N-3]
[user: turn N-2]
  [assistant + tools: turn N-2]
[user: turn N-1]
  [assistant + tools: turn N-1]    ← newest complete turn
```

### 4.6 Integration with `XaftAgent` (`crates/xaft-agent/src/agent.rs`)

`XaftAgent` already has a `before_llm_call` hook. Auto-compaction is inserted
there:

```rust
async fn before_llm_call(&self, ctx: &mut AgentContext) -> Result<(), AgtrsError> {
    // … existing hook logic …

    if let Some(ref compactor) = self.compactor {
        let last_input = ctx
            .last_token_usage()
            .map(|u| u.input_tokens)
            .unwrap_or(0);
        let window = self.llm.context_window();
        if window > 0 && last_input * 100 / window >= compactor.threshold_pct as usize {
            info!(
                agent = %ctx.agent_name,
                input_tokens = last_input,
                context_window = window,
                "xaft: context threshold exceeded, compacting"
            );
            let (compacted, stats) = compactor.compact(
                ctx.history().to_vec(),
                &ctx.agent_name,
                Arc::clone(&self.llm),
                self.conv_store.clone(),
                &self.conv_key,
                Arc::clone(&self.signals),
            ).await?;
            ctx.replace_history(compacted);
            // Signal emitted inside compact().
        }
    }
    Ok(())
}
```

`XaftAgent` gains a `compactor: Option<Arc<Compactor>>` field, set in
`XaftAgent::build_for_run()` based on `config.compaction`.

### 4.7 Manual `/compact` trigger (`crates/xaft-tui/src/slash/commands/compact.rs`)

The existing stub is replaced with a real async handler. Because the running
agent's context is in a different tokio task, `/compact` uses a **signal-based
trigger** rather than directly mutating the context:

1. The TUI emits `XaftCompactRequested { agent_name: None }` on the signal bus.
2. `XaftAgent::before_llm_call` checks for this signal and, if present, runs
   the compactor regardless of the token threshold.
3. On compaction, the agent emits `XaftContextCompacted` as normal.
4. The TUI listens for `XaftContextCompacted` to display the confirmation.

For the case where **no agent is currently running** (the user types `/compact`
while idle), the handler falls back to operating directly on the stored
conversation:

```rust
// Async CompactHandler::execute_async(ctx: CommandContext) → CommandResult:
if let (Some(store), Some(session_id)) = (&ctx.conversation_store, &ctx.session_id) {
    let conv_key = format!("{}::workflow::planner", session_id);
    let messages = store.load(&conv_key).await?;
    if messages.is_empty() {
        return CommandResult::Lines(vec!["Nothing to compact.".into()]);
    }
    let compactor = Compactor::from_config(&ctx.config.compaction);
    let llm = build_llm_from_config(&ctx.config)?;
    let (compacted, stats) = compactor.compact(messages, "planner", llm, Some(store.clone()), &conv_key, ctx.signals).await?;
    store.save(&conv_key, &compacted).await?;
    return CommandResult::Lines(vec![
        format!("  ✓ Compacted: {} → {} messages  ({} chars saved)",
            stats.messages_before, stats.messages_after, stats.chars_saved),
    ]);
}
CommandResult::Error("No active session to compact.".into())
```

### 4.8 `XaftContextCompacted` signal → TUI `[compact]` line

The signal bus bridges the runtime and TUI. When `XaftContextCompacted` fires:

```rust
TuiEvent::ContextCompacted { messages_removed, tokens_saved } => {
    self.mutations.push(RenderMutation::CommitLine(StyledLine::new(
        format!(
            "  [compact] {} message(s) summarised  ·  ~{} tokens freed",
            messages_removed,
            tokens_saved
        ),
        LineKind::System,
    )));
}
```

The `EventBridge` adds a subscription for `XaftContextCompacted` in
`attach_xaft_signals()`.

---

## 5. New signal: `XaftContextCompacted`

```rust
/// Emitted when automatic or manual context compaction completes.
#[derive(Debug, Clone)]
pub struct XaftContextCompacted {
    /// Agent whose context was compacted.
    pub agent_name: String,
    /// How many messages were replaced by the summary.
    pub messages_removed: usize,
    /// Total character length of the removed messages.
    pub chars_removed: usize,
    /// Character length of the generated summary.
    pub summary_chars: usize,
    /// Estimated tokens freed (chars_removed / 4, rough heuristic).
    pub tokens_saved_estimate: u64,
    /// Whether this was triggered automatically or by the user.
    pub triggered_by: CompactionTrigger,
}

/// New signal: manual compact requested from TUI slash command.
#[derive(Debug, Clone)]
pub struct XaftCompactRequested {
    /// When `None`, all agents check and compact if needed.
    pub agent_name: Option<String>,
}
```

---

## 6. `XaftConfig` knob: `CompactionConfig`

### `crates/xaft-config/src/types.rs`

```rust
/// Context-compaction settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Enable automatic context compaction.
    pub enabled: bool,
    /// Trigger compaction when `input_tokens / context_window ≥ threshold_pct / 100`.
    /// Range: 1–99.  Default: 80.
    pub threshold_pct: u8,
    /// Number of complete user + assistant turns to keep verbatim after compaction.
    /// Default: 4.
    pub keep_recent_turns: usize,
    /// Maximum tokens for the summary LLM call.  Default: 1024.
    pub summary_max_tokens: usize,
    /// Agent preset to use for the summarisation call.
    /// Defaults to the `fast` model tier if configured, else `default`.
    pub summary_agent_preset: Option<String>,
}
```

`XaftConfig` gains `pub compaction: CompactionConfig`.

### `crates/xaft-config/src/defaults.rs`

```rust
impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_pct: 80,
            keep_recent_turns: 4,
            summary_max_tokens: 1024,
            summary_agent_preset: None,
        }
    }
}
```

### TOML

```toml
[compaction]
enabled            = true
threshold_pct      = 80
keep_recent_turns  = 4
summary_max_tokens = 1024
# summary_agent_preset = "fast"  # uses the fast tier by default
```

---

## 7. Files

| File | Change |
|------|--------|
| `crates/xaft-runtime/src/compactor.rs` | **New** — `Compactor`, `CompactionResult`, `CompactionTrigger`, turn-boundary finder, snapshot persistence, summarisation call |
| `crates/xaft-runtime/src/lib.rs` | Export `compactor` module |
| `crates/xaft-agent/src/agent.rs` | Add `compactor: Option<Arc<Compactor>>` field; check-and-compact in `before_llm_call` |
| `crates/xaft-agent/src/signals.rs` | Add `XaftContextCompacted`, `XaftCompactRequested` |
| `crates/xaft-agent/src/lib.rs` | Re-export new signals |
| `crates/xaft-config/src/types.rs` | Add `CompactionConfig`; add `compaction: CompactionConfig` to `XaftConfig` |
| `crates/xaft-config/src/defaults.rs` | `impl Default for CompactionConfig` |
| `crates/xaft-tui/src/slash/commands/compact.rs` | Replace stub with real async handler |
| `crates/xaft-tui/src/bridge.rs` | Add `ContextCompacted { messages_removed, tokens_saved }` to `TuiEvent`; subscribe to `XaftContextCompacted` |
| `crates/xaft-tui/src/state.rs` | Handle `TuiEvent::ContextCompacted` → commit `[compact]` line |
| `crates/xaft-runtime/src/orchestrator.rs` | Wire `compactor` into `XaftAgent` builders via `AgentBuilder::with_compactor()` |
| `crates/xaft-runtime/tests/compactor_integration.rs` | **New** — integration tests |

---

## 8. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | When a session's running `input_tokens` exceeds 80 % of `context_window`, the next LLM call is preceded by a compaction; the call succeeds (no `context_length_exceeded` error). |
| AC2 | After auto-compaction, `[compact] N message(s) summarised · ~T tokens freed` appears in the TUI transcript. |
| AC3 | Exactly `keep_recent_turns` complete user+assistant turns (including all their tool calls) appear verbatim after the summary block. |
| AC4 | The compacted history begins with `[CONTEXT SUMMARY — N messages compacted]` as a `Role::System` message. |
| AC5 | A `ConversationStore` snapshot key `{conv_key}::snapshot::{timestamp}` is written before compaction; it contains the full pre-compaction message list. |
| AC6 | `ToolUse` and its corresponding `ToolResult` messages are never separated by the turn boundary. |
| AC7 | `/compact` with an active session triggers compaction and prints the confirmation line. |
| AC8 | `/compact` with no active session (idle TUI) operates on the stored conversation and reports the result. |
| AC9 | `XaftContextCompacted` signal is emitted with correct `messages_removed` and `tokens_saved_estimate`. |
| AC10 | `compaction.enabled = false` in config disables automatic compaction; `/compact` still works as a manual override. |
| AC11 | When history is shorter than `keep_recent_turns` complete turns, compaction is skipped even above threshold. |
| AC12 | `compaction.summary_agent_preset` selects which model tier performs the summarisation call; defaults to the fast tier when one is configured. |

---

## 9. Tests

### `crates/xaft-runtime/tests/compactor_integration.rs` (new)

```rust
#[tokio::test]
async fn compactor_summarises_older_turns() {
    // Build a 12-message history (4 turns × user + assistant + tool_use + tool_result).
    // Keep 2 turns, compact the other 2.
    // Assert: result has 1 system summary + 8 kept messages.
    // Assert: system message starts with "[CONTEXT SUMMARY".
}

#[tokio::test]
async fn compactor_preserves_recent_n_turns() {
    // keep_recent_turns = 3, history has 6 turns.
    // Assert: the last 3 user messages and their assistant responses are identical
    // to the input (verbatim comparison).
}

#[tokio::test]
async fn compactor_persists_full_history() {
    // Run compact() with a mock ConversationStore.
    // Assert: store.save() was called ONCE with a snapshot key containing "::snapshot::".
    // Assert: snapshot key messages == original full history.
}

#[tokio::test]
async fn compactor_emits_signal_with_token_savings() {
    // Subscribe to XaftContextCompacted before calling compact().
    // Assert: signal received with messages_removed == expected_count.
    // Assert: tokens_saved_estimate > 0.
}

#[tokio::test]
async fn compactor_handles_empty_history_gracefully() {
    // compact(vec![], …) must return Ok with messages_before == 0.
    // Must NOT call the LLM or the conv_store.
}

#[tokio::test]
async fn compactor_respects_threshold_config() {
    // Compactor { threshold_pct: 90 }.
    // input_tokens = 800, context_window = 1000 → 80 % < 90 % → no compaction.
    // Same setup with threshold_pct = 79 → 80 % > 79 % → compaction triggered.
}

#[tokio::test]
async fn compactor_never_splits_tool_use_result_pair() {
    // Build history: [user][assistant+tool_use][tool_result][assistant_final]
    // Place the boundary such that it would naively land between tool_use and tool_result.
    // Assert: boundary is moved to before the user message that initiated the tool call.
}

#[tokio::test]
async fn compactor_skips_when_history_shorter_than_keep_window() {
    // keep_recent_turns = 4, history has 2 complete turns.
    // Even if threshold is exceeded: compact() returns original messages unchanged.
    // Assert: no LLM call made.
}
```

### Unit tests in `crates/xaft-runtime/src/compactor.rs`

```rust
#[test]
fn turn_boundary_on_four_turns_keep_two() {
    // [user0, asst0, user1, asst1, user2, asst2, user3, asst3]
    // keep_recent_turns = 2 → boundary = index of user2
    let msgs = vec![
        Message::user("u0"), Message::assistant("a0"),
        Message::user("u1"), Message::assistant("a1"),
        Message::user("u2"), Message::assistant("a2"),
        Message::user("u3"), Message::assistant("a3"),
    ];
    let boundary = find_boundary(&msgs, 2);
    assert_eq!(boundary, 4, "should keep last 2 turns (indices 4..8)");
}

#[test]
fn turn_boundary_not_split_mid_tool() {
    // [user, asst+tool_use, tool_result, asst_final]
    // These 4 are ONE turn; boundary must not land in the middle.
    let msgs = vec![
        Message::user("earlier task"),
        Message::assistant("start work"),
        // Tool turn:
        Message::user("u1"),
        Message::assistant("calling tool"),
        Message::tool_result("tid-1", "output"),
        Message::assistant("done with tool"),
    ];
    // keep 1 turn: the tool turn starting at index 2
    let boundary = find_boundary(&msgs, 1);
    assert_eq!(boundary, 2, "boundary must be at start of the full tool turn");
}

#[test]
fn compact_result_message_starts_with_context_summary() {
    let summary = "[CONTEXT SUMMARY — 4 messages compacted]\n\nDid stuff.";
    let msg = Message::system(summary);
    assert!(msg.text().starts_with("[CONTEXT SUMMARY"));
}
```

---

## 10. Edge cases

| Case | Behaviour |
|------|-----------|
| `context_window()` returns 0 (provider doesn't advertise it) | Automatic compaction is disabled; threshold check always returns false; `/compact` manual trigger still works |
| History has only system messages | No `Role::User` messages → no turns to keep → compaction skipped |
| Summarisation LLM call fails | Log warning; return original messages unchanged; emit no signal; task continues normally |
| `keep_recent_turns = 0` | Treated as `keep_recent_turns = 1` (must keep at least one complete turn) |
| Compaction completes but produces a summary longer than the dropped messages | Proceed anyway; log a warning; note that some tasks with very long tool outputs may not benefit from compaction |
| Two compactions in the same session | Second compaction snapshots the already-compacted history (snapshot chain); this is correct |
| `/compact` during background pipeline (PRD 33d) | Compacts the foreground pipeline's stored conversation; background pipeline is unaffected |
| Disabled (`compaction.enabled = false`) | `Compactor::from_config` builds with `enabled = false`; `before_llm_call` skips the threshold check; `/compact` still honours the manual request |

---

## 11. `Compactor` API — full interface

```rust
/// Public API (all pub for integration tests and slash command handler).
impl Compactor {
    /// Construct from config.
    pub fn from_config(cfg: &CompactionConfig) -> Self;

    /// Disabled compactor — all calls to `should_compact` return false.
    pub fn disabled() -> Self;

    /// Check if compaction should trigger.
    ///
    /// Returns true if `input_tokens * 100 / context_window >= threshold_pct`
    /// AND `context_window > 0`.
    pub fn should_compact(&self, input_tokens: usize, context_window: usize) -> bool;

    /// Run compaction synchronously (for testing without async).
    ///
    /// `summarize_fn` is a closure that produces a summary string; inject
    /// a mock in tests to avoid real LLM calls.
    pub fn compact_with_summarizer<F>(
        &self,
        messages: Vec<Message>,
        summarize: F,
    ) -> Result<(Vec<Message>, CompactionResult), RuntimeError>
    where
        F: FnOnce(&[Message]) -> String;

    /// Full async compaction with real LLM.
    pub async fn compact(
        &self,
        messages: Vec<Message>,
        agent_name: &str,
        llm: Arc<dyn LlmProvider>,
        conv_store: Option<Arc<dyn ConversationStore>>,
        conv_key: &str,
        signals: Arc<SignalBus>,
    ) -> Result<(Vec<Message>, CompactionResult), RuntimeError>;
}

/// Find the index at which to split the message list.
/// Index `i` means: keep `messages[i..]`, summarise `messages[..i]`.
/// Always lands on a `Role::User` message boundary.
pub fn find_boundary(messages: &[Message], keep_recent_turns: usize) -> usize;

/// Format older messages into a human-readable block for the summariser.
pub fn format_for_summary(messages: &[Message]) -> String;
```

---

## 12. Interaction matrix

| Scenario | Behaviour |
|----------|-----------|
| Auto-compact while coder is mid-tool-chain | Compaction deferred until `before_llm_call` is reached; the current tool calls finish normally first |
| `/compact` during agent run (background pipeline via PRD 33d) | Emits `XaftCompactRequested`; on next `before_llm_call`, agent self-compacts |
| `/clear` (PRD 35) | Clears the TUI transcript; compaction state is not reset; subsequent calls still check the token threshold |
| `--continue` after a compaction | The stored conversation (primary key, not snapshot) is loaded — which is the compacted version. The agent continues with the compacted history. ✓ |
| `--continue` to recover a snapshot | Not directly supported via CLI; advanced users can inspect `{conv_key}::snapshot::*` keys via the session store |
| Summary model call fails mid-task | Original history returned unchanged; agent continues; a `WARN` log is emitted; no signal fires |
| `keep_recent_turns` changed between sessions | The new value takes effect on the next compaction; prior snapshots are unaffected |
