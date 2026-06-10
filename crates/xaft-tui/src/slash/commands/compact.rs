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
