//! Integration tests for PRD 50 — Auto-Compaction.

use agtrs_runtime::transport::{Message, Role};
use xaft_runtime::compactor::{CompactionTrigger, Compactor, find_boundary};

fn make_turns(count: usize) -> Vec<Message> {
    let mut msgs = Vec::new();
    for _ in 0..count {
        msgs.push(Message::user("User question about the task."));
        msgs.push(Message::assistant("Assistant response with analysis."));
    }
    msgs
}

fn make_turns_with_tools(count: usize) -> Vec<Message> {
    let mut msgs = Vec::new();
    for _ in 0..count {
        msgs.push(Message::user("User task request."));
        msgs.push(Message::assistant("Calling tool\u{2026}"));
        msgs.push(Message::tool_result("tid", "Tool output data."));
        msgs.push(Message::assistant("Done with tool."));
    }
    msgs
}

#[test]
fn compactor_summarises_older_turns() {
    let c = Compactor::new(true, 80, 2, 512);
    let msgs = make_turns(4); // 8 messages = 4 turns
    let (compacted, stats) = c
        .compact_with_summarizer(msgs, CompactionTrigger::Auto, |older| {
            format!("Summary of {} earlier messages.", older.len())
        })
        .unwrap();
    // 1 summary + last 2 turns (4 messages)
    assert_eq!(compacted.len(), 5);
    assert!(compacted[0].text().starts_with("[CONTEXT SUMMARY"));
    assert_eq!(compacted[0].role, Role::System);
    assert_eq!(stats.messages_before, 8);
}

#[test]
fn compactor_preserves_recent_n_turns() {
    let c = Compactor::new(true, 80, 3, 512);
    let all_msgs = make_turns(6); // 12 messages = 6 turns
    let (compacted, _) = c
        .compact_with_summarizer(all_msgs.clone(), CompactionTrigger::Auto, |older| {
            format!("Summary of {} msgs.", older.len())
        })
        .unwrap();
    // Last 3 turns = 6 messages, + 1 summary
    assert_eq!(compacted.len(), 7);
    // The last 6 messages from the original must be verbatim in compacted[1..].
    let kept_original = &all_msgs[all_msgs.len() - 6..];
    let kept_compacted = &compacted[1..];
    for (orig, compacted_msg) in kept_original.iter().zip(kept_compacted.iter()) {
        assert_eq!(
            orig.text(),
            compacted_msg.text(),
            "recent turns must be verbatim"
        );
    }
}

#[test]
fn compactor_skips_when_history_shorter_than_keep_window() {
    let c = Compactor::new(true, 80, 4, 512);
    let msgs = make_turns(2); // only 2 turns
    let (out, stats) = c
        .compact_with_summarizer(msgs.clone(), CompactionTrigger::Auto, |_| {
            panic!("summariser must not be called when boundary == 0")
        })
        .unwrap();
    assert_eq!(out.len(), msgs.len(), "messages unchanged");
    assert_eq!(stats.messages_removed(), 0);
}

#[test]
fn compactor_handles_empty_history_gracefully() {
    let c = Compactor::new(true, 80, 4, 512);
    let (out, stats) = c
        .compact_with_summarizer(vec![], CompactionTrigger::Auto, |_| "summary".into())
        .unwrap();
    assert!(out.is_empty());
    assert_eq!(stats.messages_before, 0);
    assert_eq!(stats.messages_after, 0);
}

#[test]
fn compactor_emits_correct_stats() {
    let c = Compactor::new(true, 80, 2, 512);
    let msgs = make_turns(4);
    let (_, stats) = c
        .compact_with_summarizer(msgs, CompactionTrigger::Manual, |_| "short summary".into())
        .unwrap();
    assert!(stats.tokens_saved_estimate > 0);
    assert!(stats.chars_removed > 0);
    assert_eq!(stats.triggered_by, CompactionTrigger::Manual);
}

#[test]
fn compactor_never_splits_tool_use_result_pair() {
    let c = Compactor::new(true, 80, 1, 512);
    let msgs = make_turns_with_tools(3); // 3 turns × 4 msgs = 12 msgs
    // Keep 1 turn = keep last 4 messages (user + 3 assistant/tool msgs)
    let boundary = find_boundary(&msgs, 1);
    // boundary must point to a User message
    assert_eq!(
        msgs[boundary].role,
        Role::User,
        "boundary must be at a User message"
    );
}

#[test]
fn compactor_respects_threshold_config() {
    let c = Compactor::new(true, 90, 2, 512);
    // 80% is below threshold of 90%
    assert!(
        !c.should_compact(800, 1000),
        "80% < 90% \u{2192} no compact"
    );
    // 95% is above threshold
    assert!(c.should_compact(950, 1000), "95% >= 90% \u{2192} compact");
}
