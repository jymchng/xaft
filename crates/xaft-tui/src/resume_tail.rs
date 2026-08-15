//! Bounded transcript-tail loader — agenthicc parity (Gap 4).
//!
//! agenthicc (`docs/guides/tui.md`) loads the newest `N` (default 20) complete
//! turns from the tail of the session log when the TUI opens with an existing
//! session, shows a `Loading transcript…` label while replaying, and chunks
//! the replay so the TUI stays responsive. This module provides the pure
//! bookkeeping: bounding a list of lines to the newest `N` turns, chunking
//! the result, and exposing the loading label state.

/// Default number of turns replayed from the tail (agenthicc default 20).
pub const DEFAULT_RESUME_TRANSCRIPT_TURNS: usize = 20;

/// A single transcript "turn": a sequence of lines produced by one agent
/// activity. Turns are delimited by `LineKind::UserMessage` or
/// `LineKind::AgentMarker` in the transcript stream.
#[derive(Debug, Clone)]
pub struct Turn {
    /// All lines belonging to this turn, in order.
    pub lines: Vec<crate::transcript::StyledLine>,
}

/// Result of bounding a transcript to the newest `N` turns.
#[derive(Debug, Clone)]
pub struct BoundedTail {
    /// The newest `turn_limit` turns (or fewer when the source is shorter).
    pub turns: Vec<Turn>,
    /// Number of turns omitted from the head (0 when nothing was trimmed).
    pub trimmed_head: usize,
    /// Whether the result is non-empty (i.e. a `Loading transcript…` label
    /// should be shown during replay).
    pub has_content: bool,
}

/// Partition a flat line list into turns.
///
/// A new turn starts at every `UserMessage` or `AgentMarker` line. Lines
/// before the first boundary are treated as a leading turn only if they are
/// non-empty; otherwise they are ignored.
pub fn partition_turns(lines: &[crate::transcript::StyledLine]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for line in lines {
        let is_boundary = matches!(
            line.kind,
            crate::transcript::LineKind::UserMessage | crate::transcript::LineKind::AgentMarker
        );
        if is_boundary {
            turns.push(Turn {
                lines: vec![line.clone()],
            });
        } else if let Some(last) = turns.last_mut() {
            last.lines.push(line.clone());
        } else if !line.text.trim().is_empty() {
            // Leading content before the first boundary becomes a turn.
            turns.push(Turn {
                lines: vec![line.clone()],
            });
        }
    }
    turns
}

/// Bound `lines` to the newest `turn_limit` turns, returning the tail and the
/// number of turns trimmed from the head.
pub fn tail_turns(lines: &[crate::transcript::StyledLine], turn_limit: usize) -> BoundedTail {
    let turns = partition_turns(lines);
    let total = turns.len();
    let keep_from = total.saturating_sub(turn_limit);
    let kept: Vec<Turn> = turns.into_iter().skip(keep_from).collect();
    let has_content = !kept.is_empty();
    BoundedTail {
        turns: kept,
        trimmed_head: keep_from,
        has_content,
    }
}

/// Chunk a bounded tail into batches of at most `batch_size` turns so the
/// appender can replay without blocking the TUI for a long transcript.
pub fn chunk_turns(turns: &[Turn], batch_size: usize) -> Vec<Vec<Turn>> {
    if batch_size == 0 {
        return vec![];
    }
    turns.chunks(batch_size).map(|c| c.to_vec()).collect()
}

/// The loading label shown while the transcript tail is being replayed
/// (agenthicc: `Loading transcript…`).
pub fn loading_label() -> &'static str {
    "Loading transcript…"
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{LineKind, StyledLine};

    fn line(text: &str, kind: LineKind) -> StyledLine {
        StyledLine::new(text.to_string(), kind)
    }

    #[test]
    fn partition_creates_turns_at_boundaries() {
        let lines = vec![
            line("User: hi", LineKind::UserMessage),
            line("agent reply", LineKind::AgentText),
            line("User: next", LineKind::UserMessage),
            line("another reply", LineKind::AgentText),
        ];
        let turns = partition_turns(&lines);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].lines.len(), 2);
        assert_eq!(turns[1].lines.len(), 2);
    }

    #[test]
    fn partition_ignores_leading_empty() {
        let lines = vec![
            line("", LineKind::System),
            line("User: hi", LineKind::UserMessage),
            line("reply", LineKind::AgentText),
        ];
        let turns = partition_turns(&lines);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].lines.len(), 2);
    }

    #[test]
    fn tail_keeps_newest_n() {
        let mut lines = Vec::new();
        for i in 0..25 {
            lines.push(line(&format!("User msg {i}"), LineKind::UserMessage));
            lines.push(line(&format!("reply {i}"), LineKind::AgentText));
        }
        let tail = tail_turns(&lines, DEFAULT_RESUME_TRANSCRIPT_TURNS);
        assert_eq!(tail.turns.len(), 20);
        assert_eq!(tail.trimmed_head, 5);
        assert!(tail.has_content);
        // The newest turn (24) must be present.
        let last_text = &tail.turns.last().unwrap().lines[0].text;
        assert!(last_text.contains("User msg 24"));
    }

    #[test]
    fn tail_keeps_all_when_under_limit() {
        let mut lines = Vec::new();
        for i in 0..3 {
            lines.push(line(&format!("User msg {i}"), LineKind::UserMessage));
        }
        let tail = tail_turns(&lines, DEFAULT_RESUME_TRANSCRIPT_TURNS);
        assert_eq!(tail.turns.len(), 3);
        assert_eq!(tail.trimmed_head, 0);
        assert!(tail.has_content);
    }

    #[test]
    fn tail_empty() {
        let tail = tail_turns(&[], DEFAULT_RESUME_TRANSCRIPT_TURNS);
        assert!(!tail.has_content);
        assert_eq!(tail.turns.len(), 0);
        assert_eq!(tail.trimmed_head, 0);
    }

    #[test]
    fn chunk_splits_by_batch() {
        let turns: Vec<Turn> = (0..7)
            .map(|i| Turn {
                lines: vec![line(&format!("l{i}"), LineKind::AgentText)],
            })
            .collect();
        let chunks = chunk_turns(&turns, 3);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 3);
        assert_eq!(chunks[1].len(), 3);
        assert_eq!(chunks[2].len(), 1);
    }

    #[test]
    fn chunk_zero_batch_empty() {
        let turns: Vec<Turn> = (0..3)
            .map(|i| Turn {
                lines: vec![line(&format!("l{i}"), LineKind::AgentText)],
            })
            .collect();
        assert!(chunk_turns(&turns, 0).is_empty());
    }

    #[test]
    fn loading_label_text() {
        assert_eq!(loading_label(), "Loading transcript…");
    }
}
