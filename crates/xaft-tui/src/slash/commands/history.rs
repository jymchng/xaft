//! `/history` handler — browse the input history.
//!
//! agenthicc's `_cmd_history` prints `/history: not available in this mode.`
//! outside an interactive overlay. xaft has a real `InputHistoryStore` behind
//! its `#` trigger, so this lists that store instead of reproducing the stub:
//! copying a degraded path would make the row look implemented while doing
//! nothing, which is worse than leaving it open.
//!
//! With an argument, it searches instead of listing, matching the `?query`
//! behaviour of the `#` picker.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};
use crate::trigger::history::time_ago;

/// Render one history row: kind label, relative age, then the text.
fn row(e: &crate::trigger::history::HistoryEntry) -> String {
    format!(
        "  [{}] {}  {}",
        e.kind.label(),
        time_ago(e.submitted_at),
        e.text
    )
}

/// How many entries an argument-free `/history` shows.
const MAX_SHOWN: usize = 20;

pub struct HistoryHandler;

impl SlashHandler for HistoryHandler {
    fn description(&self) -> &'static str {
        "Browse the input history"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[query]")
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let store = match ctx.input_history.as_ref() {
            Some(s) => s,
            None => {
                return CommandResult::Lines(vec![
                    "History is unavailable in this context.".into(),
                ]);
            }
        };
        let store = match store.read() {
            Ok(s) => s,
            Err(_) => {
                return CommandResult::Error("History store lock was poisoned.".into());
            }
        };

        let query = ctx.args.trim();
        let entries: Vec<String> = if query.is_empty() {
            store.all_newest_first().take(MAX_SHOWN).map(row).collect()
        } else {
            store
                .search(query)
                .iter()
                .take(MAX_SHOWN)
                .map(|e| row(e))
                .collect()
        };

        if entries.is_empty() {
            return CommandResult::Lines(vec![if query.is_empty() {
                "No input history yet.".into()
            } else {
                format!("No history entries matching {query:?}.")
            }]);
        }

        let total = if query.is_empty() {
            store.len()
        } else {
            store.search(query).len()
        };
        let mut lines = vec![format!(
            "History ({total} entr{}):",
            if total == 1 { "y" } else { "ies" }
        )];
        lines.extend(entries);
        CommandResult::Lines(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use crate::trigger::history::{HistoryKind, InputHistoryStore};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;

    fn ctx(args: &str, store: Option<Arc<RwLock<InputHistoryStore>>>) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::History,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("/tmp"),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: store,
        }
    }

    fn store_with(entries: &[&str]) -> Arc<RwLock<InputHistoryStore>> {
        let mut s = InputHistoryStore::new(50);
        for e in entries {
            s.push((*e).to_string(), HistoryKind::AgentTask);
        }
        Arc::new(RwLock::new(s))
    }

    fn lines(out: CommandResult) -> Vec<String> {
        match out {
            CommandResult::Lines(v) => v,
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn lists_stored_entries() {
        let out = lines(HistoryHandler.execute(ctx("", Some(store_with(&["first", "second"])))));
        assert_eq!(out[0], "History (2 entries):");
        assert!(out.iter().any(|l| l.contains("first")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("second")), "{out:?}");
    }

    #[test]
    fn empty_store_says_so() {
        let out = lines(HistoryHandler.execute(ctx("", Some(store_with(&[])))));
        assert_eq!(out, vec!["No input history yet.".to_string()]);
    }

    #[test]
    fn query_filters_entries() {
        let store = store_with(&["build the thing", "run tests", "build again"]);
        let out = lines(HistoryHandler.execute(ctx("build", Some(store))));
        assert!(out.iter().any(|l| l.contains("build the thing")), "{out:?}");
        assert!(!out.iter().any(|l| l.contains("run tests")), "{out:?}");
    }

    #[test]
    fn query_with_no_match_reports_none() {
        let out = lines(HistoryHandler.execute(ctx("zzz", Some(store_with(&["alpha"])))));
        assert!(out[0].contains("No history entries matching"), "{out:?}");
    }

    #[test]
    fn missing_store_reports_unavailable() {
        let out = lines(HistoryHandler.execute(ctx("", None)));
        assert!(out[0].contains("unavailable"), "{out:?}");
    }
}
