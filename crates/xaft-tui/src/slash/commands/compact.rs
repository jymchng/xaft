//! /compact command handler — summarises stored conversation to free context.

use agtrs_runtime::memory::ConversationStore as _;
use xaft_runtime::compactor::{CompactionTrigger, Compactor};

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct CompactHandler;

impl SlashHandler for CompactHandler {
    fn description(&self) -> &'static str {
        "Summarise older messages to free context-window space"
    }

    fn is_async(&self) -> bool {
        true
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Error("use async execute".into())
    }

    fn execute_boxed_async(
        &self,
        ctx: CommandContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>> {
        Box::pin(async move {
            let (Some(store), Some(session_id)) =
                (ctx.conversation_store.as_ref(), ctx.session_id.as_ref())
            else {
                return CommandResult::Error(
                    "No active session to compact. Start a task first.".into(),
                );
            };

            // Try the planner key (most common).
            let planner_key = format!("{}::workflow::planner", session_id);
            let messages = match store.load(&planner_key).await {
                Ok(m) if !m.is_empty() => m,
                _ => {
                    // Fall back to the workflow key.
                    match store.load(&format!("{}::workflow", session_id)).await {
                        Ok(m) if !m.is_empty() => m,
                        _ => {
                            return CommandResult::Lines(vec![
                                "  Nothing to compact (no conversation history found).".into(),
                            ]);
                        }
                    }
                }
            };

            let cfg = &ctx.config.compaction;
            let compactor = Compactor::new(
                true,
                cfg.threshold_pct,
                cfg.keep_recent_turns,
                cfg.summary_max_tokens,
            );

            // Use a simple extractive summary (no LLM call needed from the
            // slash-command path — a real LLM provider is not wired here).
            let summarize = |older: &[agtrs_runtime::transport::Message]| -> String {
                let mut lines = vec![format!("Earlier conversation ({} messages):", older.len())];
                for msg in older.iter().take(20) {
                    let role = format!("{:?}", msg.role);
                    let text = msg.text();
                    let preview: String = text.chars().take(120).collect();
                    let ellipsis = if text.chars().count() > 120 {
                        "…"
                    } else {
                        ""
                    };
                    lines.push(format!("  [{role}] {preview}{ellipsis}"));
                }
                if older.len() > 20 {
                    lines.push(format!("  … and {} more messages", older.len() - 20));
                }
                lines.join("\n")
            };

            match compactor.compact_with_summarizer(messages, CompactionTrigger::Manual, summarize)
            {
                Ok((compacted, stats)) if stats.messages_removed() > 0 => {
                    let _ = store.save(&planner_key, &compacted).await;

                    // Emit signal so the TUI renders the [compact] marker line.
                    let _ = ctx
                        .signals
                        .emit(xaft_agent::signals::XaftContextCompacted {
                            agent_name: "planner".into(),
                            messages_removed: stats.messages_removed(),
                            chars_removed: stats.chars_removed,
                            summary_chars: stats.summary_chars,
                            tokens_saved_estimate: stats.tokens_saved_estimate,
                        })
                        .await;

                    CommandResult::Lines(vec![format!(
                        "  ✓ Compacted: {} → {} messages  (~{} tokens freed)",
                        stats.messages_before, stats.messages_after, stats.tokens_saved_estimate,
                    )])
                }
                Ok(_) => CommandResult::Lines(vec![
                    "  Nothing to compact (history shorter than keep_recent_turns).".into(),
                ]),
                Err(e) => CommandResult::Error(format!("Compaction failed: {e}")),
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::memory::{ConversationStore, InMemoryConversationStore};
    use agtrs_runtime::signals::SignalBus;
    use agtrs_runtime::transport::{Message, Role};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_agent::signals::XaftContextCompacted;
    use xaft_config::XaftConfig;

    /// A context with the given store/session wiring. `None` for either is how
    /// the "no active session" and "no store attached" refusals are reached.
    fn ctx(
        store: Option<Arc<dyn ConversationStore>>,
        session_id: Option<&str>,
        keep_recent_turns: usize,
    ) -> CommandContext {
        let mut config = XaftConfig::default();
        config.compaction.keep_recent_turns = keep_recent_turns;
        CommandContext {
            args: String::new(),
            command: SlashCommand::Compact,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(config),
            session_id: session_id.map(str::to_string),
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: store,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    fn store() -> Arc<dyn ConversationStore> {
        Arc::new(InMemoryConversationStore::new())
    }

    /// `turns` complete user+assistant pairs, each `text_len` chars long.
    fn turns(turns: usize, text_len: usize) -> Vec<Message> {
        let mut v = Vec::with_capacity(turns * 2);
        for i in 0..turns {
            v.push(Message::user("u".repeat(text_len)));
            v.push(Message::assistant(format!("a{i}")));
        }
        v
    }

    async fn run(ctx: CommandContext) -> CommandResult {
        CompactHandler.execute_boxed_async(ctx).await
    }

    fn lines_of(r: &CommandResult) -> &Vec<String> {
        match r {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    fn error_of(r: &CommandResult) -> &String {
        match r {
            CommandResult::Error(e) => e,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// `Message` has no `PartialEq`, so histories are compared as
    /// `(role, text)` pairs.
    fn texts(msgs: &[Message]) -> Vec<(Role, String)> {
        msgs.iter().map(|m| (m.role, m.text())).collect()
    }

    #[test]
    fn handler_is_async_and_declines_the_sync_path() {
        assert!(
            CompactHandler.is_async(),
            "/compact awaits the conversation store, so the sync path must not be used"
        );
        assert_eq!(
            CompactHandler.description(),
            "Summarise older messages to free context-window space"
        );
        // The sync entry point must never appear to succeed, or a caller that
        // ignores `is_async` would print nothing and look like a no-op.
        let out = CompactHandler.execute(ctx(Some(store()), Some("s1"), 4));
        assert_eq!(error_of(&out), "use async execute");
    }

    #[tokio::test]
    async fn compact_without_a_session_is_refused() {
        let out = run(ctx(Some(store()), None, 4)).await;
        assert_eq!(
            error_of(&out),
            "No active session to compact. Start a task first."
        );
    }

    #[tokio::test]
    async fn compact_without_a_conversation_store_is_refused() {
        let out = run(ctx(None, Some("s1"), 4)).await;
        assert_eq!(
            error_of(&out),
            "No active session to compact. Start a task first."
        );
    }

    #[tokio::test]
    async fn an_empty_history_reports_nothing_to_compact() {
        let out = run(ctx(Some(store()), Some("s1"), 4)).await;
        assert_eq!(
            lines_of(&out),
            &vec!["  Nothing to compact (no conversation history found).".to_string()],
            "neither the planner nor the workflow key had any messages"
        );
    }

    #[tokio::test]
    async fn a_history_shorter_than_the_keep_window_is_left_alone() {
        let s = store();
        s.save("s1::workflow::planner", &turns(2, 8)).await.unwrap();
        let before = s.load("s1::workflow::planner").await.unwrap();

        // keep_recent_turns = 4 but there are only 2 turns, so
        // find_boundary() returns 0 and nothing may be removed.
        let out = run(ctx(Some(s.clone()), Some("s1"), 4)).await;
        assert_eq!(
            lines_of(&out),
            &vec!["  Nothing to compact (history shorter than keep_recent_turns).".to_string()]
        );
        let after = s.load("s1::workflow::planner").await.unwrap();
        assert_eq!(
            after.len(),
            before.len(),
            "a no-op compaction must not rewrite the stored history"
        );
        assert_eq!(texts(&after), texts(&before));
    }

    #[tokio::test]
    async fn compact_summarises_old_turns_and_saves_the_result() {
        let s = store();
        s.save("s1::workflow::planner", &turns(6, 200))
            .await
            .unwrap();

        let signals = Arc::new(SignalBus::new());
        let mut rx = signals.subscribe::<XaftContextCompacted>().await;
        let mut c = ctx(Some(s.clone()), Some("s1"), 4);
        c.signals = signals;

        let out = run(c).await;
        let line = &lines_of(&out)[0];
        assert!(
            line.starts_with("  \u{2713} Compacted: 12 \u{2192} 9 messages"),
            "{line}"
        );

        let stored = s.load("s1::workflow::planner").await.unwrap();
        assert_eq!(stored.len(), 9, "1 summary + the last 4 kept turns");
        assert_eq!(
            stored[0].role,
            Role::System,
            "compaction replaces the older turns with a system summary block"
        );
        assert!(
            stored[0]
                .text()
                .starts_with("[CONTEXT SUMMARY \u{2014} 4 messages compacted]"),
            "{}",
            stored[0].text()
        );
        assert_eq!(
            stored[1].role,
            Role::User,
            "the kept window must start at the boundary turn, verbatim"
        );

        // The TUI renders a [compact] marker from this signal, so it must be
        // emitted with counts that agree with what was actually written.
        let ev = rx.try_recv().expect("compaction must emit its signal");
        assert_eq!(ev.agent_name, "planner");
        assert_eq!(ev.messages_removed, 4);
        assert!(ev.tokens_saved_estimate > 0);
        assert_eq!(
            ev.summary_chars as usize,
            stored[0]
                .text()
                .len()
                .saturating_sub("[CONTEXT SUMMARY \u{2014} 4 messages compacted]\n\n".len()),
            "the reported summary size must describe the summary that was saved"
        );
    }

    #[tokio::test]
    async fn an_empty_planner_key_falls_back_to_the_workflow_key() {
        let s = store();
        s.save("s1::workflow", &turns(6, 40)).await.unwrap();

        let out = run(ctx(Some(s.clone()), Some("s1"), 4)).await;
        assert!(lines_of(&out)[0].contains("Compacted: 12 \u{2192} 9 messages"));

        // The handler always writes back under the PLANNER key, whatever it
        // read from. Pin that, because it decides where a resumed session
        // looks for its history.
        assert_eq!(
            s.load("s1::workflow::planner").await.unwrap().len(),
            9,
            "the compacted history is written under the planner key"
        );
        assert_eq!(
            s.load("s1::workflow").await.unwrap().len(),
            12,
            "the workflow key it fell back to is left untouched"
        );
    }

    #[tokio::test]
    async fn the_extractive_summary_truncates_and_counts_the_remainder() {
        let s = store();
        // keep_recent_turns = 2, 13 turns \u2192 22 older messages, so the summary
        // hits both its 20-message preview cap and the 120-char truncation.
        s.save("s1::workflow::planner", &turns(13, 300))
            .await
            .unwrap();

        let out = run(ctx(Some(s.clone()), Some("s1"), 2)).await;
        assert!(lines_of(&out)[0].contains("Compacted: 26 \u{2192} 5 messages"));

        let summary = s.load("s1::workflow::planner").await.unwrap()[0].text();
        assert!(
            summary.contains("Earlier conversation (22 messages):"),
            "{}",
            summary
        );
        assert!(
            summary.contains("\u{2026} and 2 more messages"),
            "22 older messages \u{2192} 20 previewed, 2 must be reported as the remainder: {summary}"
        );
        assert!(
            summary.contains(&"u".repeat(120)),
            "a 300-char message is previewed to exactly 120 chars"
        );
        assert!(
            !summary.contains(&"u".repeat(121)),
            "the preview must stop at 120 chars"
        );
        assert!(
            summary.contains("[User] "),
            "each previewed message is labelled with its role"
        );
    }

    #[tokio::test]
    async fn compaction_is_idempotent_once_the_history_is_short() {
        let s = store();
        s.save("s1::workflow::planner", &turns(6, 40))
            .await
            .unwrap();

        run(ctx(Some(s.clone()), Some("s1"), 4)).await;
        let after_first = s.load("s1::workflow::planner").await.unwrap();

        // A second /compact must find only the summary plus the kept window,
        // which is fewer turns than keep_recent_turns, and change nothing.
        let out = run(ctx(Some(s.clone()), Some("s1"), 4)).await;
        assert_eq!(
            lines_of(&out),
            &vec!["  Nothing to compact (history shorter than keep_recent_turns).".to_string()]
        );
        assert_eq!(
            texts(&s.load("s1::workflow::planner").await.unwrap()),
            texts(&after_first)
        );
    }
}
