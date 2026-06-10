//! Context-window compaction for long-running agent sessions.
//!
//! When a conversation's token count approaches the model's context limit,
//! `Compactor` summarises older messages and replaces them with a compact
//! `[CONTEXT SUMMARY]` block, preserving the last N complete turns verbatim.

use agtrs_runtime::transport::{Message, Role};

use crate::error::RuntimeError;

// ── Public types ──────────────────────────────────────────────────────────────

/// Whether compaction was triggered automatically or manually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// Compaction was triggered automatically when the token threshold was reached.
    Auto,
    /// Compaction was triggered manually via the `/compact` slash command.
    Manual,
}

/// Statistics from a single compaction run.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Number of messages in the history before compaction.
    pub messages_before: usize,
    /// Number of messages in the history after compaction (includes the summary message).
    pub messages_after: usize,
    /// Total character length of the removed messages.
    pub chars_removed: usize,
    /// Character length of the generated summary.
    pub summary_chars: usize,
    /// Rough token estimate: chars / 4.
    pub tokens_saved_estimate: u64,
    /// Whether this compaction was triggered automatically or manually.
    pub triggered_by: CompactionTrigger,
}

impl CompactionResult {
    /// How many original messages were replaced by the summary block.
    ///
    /// Returns `0` when no compaction occurred (boundary was at the start).
    pub fn messages_removed(&self) -> usize {
        // When no compaction occurs, messages_before == messages_after.
        if self.messages_before == self.messages_after {
            return 0;
        }
        // After compaction: messages_after = 1 (summary) + kept_recent
        // messages_removed = messages_before - kept_recent = messages_before - (messages_after - 1)
        self.messages_before
            .saturating_sub(self.messages_after.saturating_sub(1))
    }
}

// ── Compactor ─────────────────────────────────────────────────────────────────

/// Compacts an agent's in-memory message history to free context-window space.
#[derive(Debug, Clone)]
pub struct Compactor {
    /// Token-usage threshold (0–100) at which compaction triggers. Default: 80.
    pub threshold_pct: u8,
    /// Number of complete user+assistant turns to keep verbatim. Default: 4.
    pub keep_recent_turns: usize,
    /// Maximum tokens for the summary LLM call. Default: 1024.
    pub summary_max_tokens: usize,
    /// Whether compaction is enabled. When `false`, `should_compact` always returns `false`.
    pub enabled: bool,
}

impl Compactor {
    /// Construct from a `CompactionConfig`.
    pub fn new(
        enabled: bool,
        threshold_pct: u8,
        keep_recent_turns: usize,
        summary_max_tokens: usize,
    ) -> Self {
        Self {
            enabled,
            threshold_pct,
            keep_recent_turns: keep_recent_turns.max(1),
            summary_max_tokens,
        }
    }

    /// Always-disabled compactor for use when the feature is turned off.
    pub fn disabled() -> Self {
        Self::new(false, 80, 4, 1024)
    }

    /// Returns `true` if compaction should trigger at these token counts.
    ///
    /// Always returns `false` when `context_window == 0` or `!self.enabled`.
    pub fn should_compact(&self, input_tokens: usize, context_window: usize) -> bool {
        self.enabled
            && context_window > 0
            && input_tokens.saturating_mul(100) / context_window >= self.threshold_pct as usize
    }

    /// Synchronous compaction with an injectable summariser.
    ///
    /// Used in tests to avoid real LLM calls.  `summarize` receives the
    /// older messages and must return a summary string.
    pub fn compact_with_summarizer<F>(
        &self,
        messages: Vec<Message>,
        triggered_by: CompactionTrigger,
        summarize: F,
    ) -> Result<(Vec<Message>, CompactionResult), RuntimeError>
    where
        F: FnOnce(&[Message]) -> String,
    {
        if messages.is_empty() {
            return Ok((
                messages,
                CompactionResult {
                    messages_before: 0,
                    messages_after: 0,
                    chars_removed: 0,
                    summary_chars: 0,
                    tokens_saved_estimate: 0,
                    triggered_by,
                },
            ));
        }

        let boundary = find_boundary(&messages, self.keep_recent_turns);

        // Not enough history to compact.
        if boundary == 0 {
            return Ok((
                messages.clone(),
                CompactionResult {
                    messages_before: messages.len(),
                    messages_after: messages.len(),
                    chars_removed: 0,
                    summary_chars: 0,
                    tokens_saved_estimate: 0,
                    triggered_by,
                },
            ));
        }

        let older = &messages[..boundary];
        let recent = &messages[boundary..];

        let chars_removed: usize = older.iter().map(|m| m.text().len()).sum();
        let summary_text = summarize(older);
        let summary_chars = summary_text.len();

        let mut compacted = Vec::with_capacity(1 + recent.len());
        compacted.push(Message::system(format!(
            "[CONTEXT SUMMARY — {} messages compacted]\n\n{}",
            older.len(),
            summary_text,
        )));
        compacted.extend_from_slice(recent);

        let result = CompactionResult {
            messages_before: messages.len(),
            messages_after: compacted.len(),
            chars_removed,
            summary_chars,
            tokens_saved_estimate: (chars_removed / 4) as u64,
            triggered_by,
        };

        Ok((compacted, result))
    }
}

// ── Boundary finding ──────────────────────────────────────────────────────────

/// Find the message index at which to split the history.
///
/// Returns `i` such that `messages[..i]` is summarised and `messages[i..]`
/// is kept verbatim.  The boundary always lands on a `Role::User` message so
/// that `ToolUse`/`ToolResult` pairs are never separated.
///
/// If the history is shorter than `keep_recent_turns` complete turns,
/// returns `0` (no compaction).
pub fn find_boundary(messages: &[Message], keep_recent_turns: usize) -> usize {
    if messages.is_empty() || keep_recent_turns == 0 {
        return 0;
    }

    let mut turns_kept = 0usize;
    let n = messages.len();
    let mut i = n;

    while i > 0 {
        i -= 1;
        if messages[i].role == Role::User {
            turns_kept += 1;
            if turns_kept >= keep_recent_turns {
                return i; // keep messages[i..]
            }
        }
    }

    // Not enough complete turns — do not compact.
    0
}

// ── Summary formatting ────────────────────────────────────────────────────────

/// Format older messages into a human-readable block for the summariser prompt.
pub fn format_for_summary(messages: &[Message]) -> String {
    let mut out = String::new();
    for msg in messages {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
            Role::Tool => "Tool",
        };
        out.push_str(&format!("[{}]\n{}\n\n", role_label, msg.text()));
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::transport::Message;

    fn c() -> Compactor {
        Compactor::new(true, 80, 2, 512)
    }

    fn msgs(roles: &[&str]) -> Vec<Message> {
        roles
            .iter()
            .map(|r| match *r {
                "u" => Message::user("user msg"),
                "a" => Message::assistant("assistant msg"),
                "t" => Message::tool_result("tid", "tool output"),
                "s" => Message::system("system prompt"),
                _ => Message::user("other"),
            })
            .collect()
    }

    #[test]
    fn should_compact_above_threshold() {
        let c = Compactor::new(true, 80, 2, 512);
        assert!(c.should_compact(850, 1000), "850/1000 = 85% >= 80%");
        assert!(!c.should_compact(750, 1000), "750/1000 = 75% < 80%");
    }

    #[test]
    fn should_not_compact_when_disabled() {
        let c = Compactor::disabled();
        assert!(!c.should_compact(999, 1000));
    }

    #[test]
    fn should_not_compact_when_window_is_zero() {
        let c = Compactor::new(true, 80, 2, 512);
        assert!(!c.should_compact(1000, 0));
    }

    #[test]
    fn find_boundary_keeps_two_turns() {
        // [u,a, u,a, u,a, u,a] — 4 turns, keep 2 → boundary at turn 2 start (index 4)
        let m = msgs(&["u", "a", "u", "a", "u", "a", "u", "a"]);
        let b = find_boundary(&m, 2);
        assert_eq!(b, 4, "should keep last 2 user+assistant pairs");
        assert_eq!(m[b].role, Role::User);
    }

    #[test]
    fn find_boundary_returns_zero_when_not_enough_turns() {
        // 2 turns but keep 4 → return 0
        let m = msgs(&["u", "a", "u", "a"]);
        assert_eq!(find_boundary(&m, 4), 0);
    }

    #[test]
    fn find_boundary_never_splits_tool_result() {
        // [u,a,t,a, u,a] — 2 turns; keep 1 → boundary at last user (index 4)
        let m = msgs(&["u", "a", "t", "a", "u", "a"]);
        let b = find_boundary(&m, 1);
        assert_eq!(b, 4);
        assert_eq!(m[b].role, Role::User);
    }

    #[test]
    fn compact_with_summarizer_keeps_recent_turns() {
        let c = Compactor::new(true, 80, 2, 512);
        let m = msgs(&["u", "a", "u", "a", "u", "a", "u", "a"]);
        let (compacted, stats) = c
            .compact_with_summarizer(m.clone(), CompactionTrigger::Manual, |older| {
                format!("summary of {} msgs", older.len())
            })
            .unwrap();
        // 1 system summary + 4 kept messages (last 2 turns = 2×[u,a])
        assert_eq!(compacted.len(), 5, "1 summary + 4 recent");
        assert!(compacted[0].text().starts_with("[CONTEXT SUMMARY"));
        assert_eq!(compacted[1].role, Role::User);
        assert_eq!(stats.messages_before, 8);
        assert_eq!(stats.messages_removed(), 4);
    }

    #[test]
    fn compact_with_empty_history_is_noop() {
        let c = c();
        let (out, stats) = c
            .compact_with_summarizer(vec![], CompactionTrigger::Auto, |_| "summary".into())
            .unwrap();
        assert!(out.is_empty());
        assert_eq!(stats.messages_before, 0);
        assert_eq!(stats.messages_after, 0);
    }

    #[test]
    fn compact_skips_when_history_shorter_than_keep_window() {
        let c = Compactor::new(true, 80, 4, 512);
        let m = msgs(&["u", "a", "u", "a"]); // only 2 turns, keep 4
        let (out, stats) = c
            .compact_with_summarizer(m.clone(), CompactionTrigger::Auto, |_| {
                panic!("LLM must not be called")
            })
            .unwrap();
        // boundary = 0 → no change
        assert_eq!(
            out.len(),
            m.len(),
            "no compaction when shorter than keep window"
        );
        assert_eq!(stats.messages_removed(), 0);
    }

    #[test]
    fn format_for_summary_includes_role_labels() {
        let msgs = vec![Message::user("hello"), Message::assistant("world")];
        let s = format_for_summary(&msgs);
        assert!(s.contains("[User]"));
        assert!(s.contains("[Assistant]"));
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
    }

    #[test]
    fn compact_result_first_message_is_context_summary() {
        let c = c();
        let m = msgs(&["u", "a", "u", "a", "u", "a", "u", "a"]);
        let (compacted, _) = c
            .compact_with_summarizer(m, CompactionTrigger::Manual, |_| "Mocked summary.".into())
            .unwrap();
        assert!(
            compacted[0]
                .text()
                .starts_with("[CONTEXT SUMMARY — 4 messages compacted]")
        );
    }
}
