//! History trigger for the xaft TUI input bar.
//!
//! This module provides:
//!
//! - [`InputHistoryStore`] — a ring buffer of all user input bar submissions
//!   (both agent tasks and slash commands). Supersedes `SlashHistory` for
//!   the `#`-trigger dropdown use case; `SlashHistory` is retained for
//!   cursor-based Up/Down navigation.
//!
//! - [`HistoryTriggerHandler`] — implements the local `TriggerHandler` stub
//!   for the `#` trigger character. When PRD-59 merges, update the impl to
//!   `impl crate::trigger::TriggerHandler for HistoryTriggerHandler`.
//!
//! - [`time_ago`] — formats an `Instant` as a human-readable relative time
//!   string (e.g. `"2m ago"`, `"3h ago"`).
//!
//! # Thread safety
//!
//! `InputHistoryStore` is intended to be wrapped in
//! `Arc<RwLock<InputHistoryStore>>` so it can be shared between the TUI main
//! loop and `HistoryTriggerHandler`.
//!
//! # PRD-59 integration note
//!
//! `HistoryTriggerHandler` currently implements the local
//! [`LocalTriggerHandler`](super::LocalTriggerHandler) trait stub rather than
//! the real `TriggerHandler` from PRD-59. When PRD-59 merges:
//!
//! 1. Remove the `// Local mirror` comment block.
//! 2. Change `impl super::LocalTriggerHandler` to
//!    `impl crate::trigger::TriggerHandler`.
//! 3. The method signatures and logic remain unchanged.
//!
//! # Example
//!
//! ```rust
//! use xaft_tui::trigger::history::{InputHistoryStore, HistoryKind, time_ago};
//!
//! let mut store = InputHistoryStore::new(50);
//! store.push("Fix the auth bug".to_string(), HistoryKind::AgentTask);
//! store.push("/compact".to_string(), HistoryKind::SlashCommand);
//!
//! let results = store.search("auth");
//! assert_eq!(results.len(), 1);
//! assert_eq!(results[0].text, "Fix the auth bug");
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use super::{MatchItem, MatchKind, TriggerContext, TriggerHandler as LocalTriggerHandler};

// ── HistoryKind ───────────────────────────────────────────────────────────────

/// Classification of a history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryKind {
    /// A natural-language task sent to the agent.
    AgentTask,
    /// A `/command` that was executed.
    SlashCommand,
}

impl HistoryKind {
    /// Short label shown in the palette hint column.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AgentTask => "task",
            Self::SlashCommand => "cmd",
        }
    }
}

// ── HistoryEntry ──────────────────────────────────────────────────────────────

/// A single entry in the input history store.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The full text that was submitted.
    pub text: String,
    /// Wall-clock instant of submission.
    pub submitted_at: Instant,
    /// What kind of submission this was.
    pub kind: HistoryKind,
}

// ── time_ago ──────────────────────────────────────────────────────────────────

/// Format a `Duration` as a human-readable relative time label.
///
/// The breakpoints are:
/// - < 60 s → `"Xs ago"`
/// - < 3600 s (1 h) → `"Xm ago"`
/// - < 86400 s (1 d) → `"Xh ago"`
/// - ≥ 86400 s → `"Xd ago"`
///
/// # Examples
///
/// ```rust
/// use std::time::Duration;
/// use xaft_tui::trigger::history::time_ago_duration;
///
/// assert_eq!(time_ago_duration(Duration::from_secs(5)), "5s ago");
/// assert_eq!(time_ago_duration(Duration::from_secs(90)), "1m ago");
/// assert_eq!(time_ago_duration(Duration::from_secs(7200)), "2h ago");
/// assert_eq!(time_ago_duration(Duration::from_secs(172800)), "2d ago");
/// ```
pub fn time_ago_duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Format the elapsed time since `submitted_at` as a human-readable relative
/// time label. Delegates to [`time_ago_duration`].
pub fn time_ago(submitted_at: Instant) -> String {
    time_ago_duration(submitted_at.elapsed())
}

// ── InputHistoryStore ─────────────────────────────────────────────────────────

/// Ring buffer of all user input bar submissions.
///
/// Captures both agent tasks and slash commands. Adjacent identical submissions
/// are **not** deduplicated — unlike `SlashHistory`, timing matters for the
/// history view (users may want to see how often they repeated a task).
///
/// Wrap in `Arc<RwLock<InputHistoryStore>>` when sharing across tasks.
pub struct InputHistoryStore {
    entries: VecDeque<HistoryEntry>,
    /// Maximum number of entries retained. When exceeded, the oldest entry
    /// (the one at the back of the deque) is evicted.
    pub max_entries: usize,
}

impl InputHistoryStore {
    /// Create a new store with the given maximum capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
        }
    }

    /// Push a new entry. No adjacent-dedup (unlike `SlashHistory`).
    ///
    /// When the store is at capacity the oldest entry is evicted first.
    pub fn push(&mut self, text: String, kind: HistoryKind) {
        if self.entries.len() >= self.max_entries {
            self.entries.pop_back();
        }
        self.entries.push_front(HistoryEntry {
            text,
            submitted_at: Instant::now(),
            kind,
        });
    }

    /// Return all entries whose `text` contains `query` (case-insensitive
    /// substring match), newest-first. An empty query returns all entries.
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry> {
        let q = query.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| q.is_empty() || e.text.to_ascii_lowercase().contains(q.as_str()))
            .collect()
    }

    /// Iterate over all entries, newest first.
    pub fn all_newest_first(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter()
    }

    /// Total number of entries currently stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no entries have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── HistoryTriggerHandler ─────────────────────────────────────────────────────

/// `LocalTriggerHandler` implementation for the `#` character.
///
/// Shows a searchable list of prior input bar submissions, newest first.
/// When PRD-59 merges, change `impl LocalTriggerHandler` to
/// `impl crate::trigger::TriggerHandler` — the method bodies are identical.
pub struct HistoryTriggerHandler {
    /// Shared, lock-guarded history store.
    pub history: Arc<RwLock<InputHistoryStore>>,
    /// If true, show the AgentTask/SlashCommand kind label in the hint column.
    pub show_kind: bool,
    /// Maximum items to show in the dropdown (default: 20).
    pub max_items: usize,
}

impl HistoryTriggerHandler {
    /// Create a handler backed by the given history store.
    pub fn new(history: Arc<RwLock<InputHistoryStore>>) -> Self {
        Self {
            history,
            show_kind: true,
            max_items: 20,
        }
    }
}

impl LocalTriggerHandler for HistoryTriggerHandler {
    /// This handler activates on `#`.
    fn trigger_char(&self) -> char {
        '#'
    }

    /// Return `MatchItem`s for the current prefix (substring search on full text).
    ///
    /// - `MatchItem.display` — first 60 chars of the entry text (truncated with `…`).
    /// - `MatchItem.insert` — **full** entry text (restored into input bar on select).
    /// - `MatchItem.hint`   — `"time_ago  ·  kind"` when `show_kind` is true.
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let store = self.history.read().expect("history RwLock poisoned");
        store
            .search(ctx.prefix)
            .iter()
            .take(self.max_items)
            .map(|entry| {
                let display = if entry.text.chars().count() > 60 {
                    let truncated: String = entry.text.chars().take(57).collect();
                    format!("{truncated}…")
                } else {
                    entry.text.clone()
                };
                let hint = if self.show_kind {
                    Some(format!(
                        "{}  ·  {}",
                        time_ago(entry.submitted_at),
                        entry.kind.label()
                    ))
                } else {
                    Some(time_ago(entry.submitted_at))
                };
                MatchItem {
                    display,
                    insert: entry.text.clone(),
                    hint,
                    kind: MatchKind::Custom("history".into()),
                }
            })
            .collect()
    }

    /// Selecting a history item restores the full text into the input bar.
    ///
    /// The entire trigger prefix (`#query`) is replaced by `item.insert`.
    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    /// History trigger does not allow free text: pressing Enter with no
    /// selection closes the dropdown without changing the buffer.
    fn allows_free_text(&self) -> bool {
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(max: usize) -> InputHistoryStore {
        InputHistoryStore::new(max)
    }

    #[test]
    fn push_records_entry() {
        let mut store = make_store(50);
        store.push("hello world".to_string(), HistoryKind::AgentTask);
        assert_eq!(store.len(), 1);
        assert_eq!(store.entries[0].text, "hello world");
        assert_eq!(store.entries[0].kind, HistoryKind::AgentTask);
    }

    #[test]
    fn search_finds_substring() {
        let mut store = make_store(50);
        store.push("Fix the auth bug".to_string(), HistoryKind::AgentTask);
        store.push("/compact".to_string(), HistoryKind::SlashCommand);
        let results = store.search("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "Fix the auth bug");
    }

    #[test]
    fn search_returns_newest_first() {
        let mut store = make_store(50);
        store.push("first entry".to_string(), HistoryKind::AgentTask);
        store.push("second entry".to_string(), HistoryKind::AgentTask);
        store.push("third entry".to_string(), HistoryKind::AgentTask);
        let all = store.search("");
        // newest first
        assert_eq!(all[0].text, "third entry");
        assert_eq!(all[1].text, "second entry");
        assert_eq!(all[2].text, "first entry");
    }

    #[test]
    fn max_entries_evicts_oldest() {
        let mut store = make_store(3);
        store.push("one".to_string(), HistoryKind::AgentTask);
        store.push("two".to_string(), HistoryKind::AgentTask);
        store.push("three".to_string(), HistoryKind::AgentTask);
        assert_eq!(store.len(), 3);
        store.push("four".to_string(), HistoryKind::AgentTask);
        assert_eq!(store.len(), 3);
        // "one" must have been evicted (it's the oldest).
        let texts: Vec<&str> = store.search("").iter().map(|e| e.text.as_str()).collect();
        assert!(!texts.contains(&"one"), "oldest entry must be evicted");
        assert!(texts.contains(&"four"), "newest entry must be present");
    }

    #[test]
    fn time_ago_formats_seconds() {
        let d = Duration::from_secs(5);
        assert_eq!(time_ago_duration(d), "5s ago");
    }

    #[test]
    fn time_ago_formats_minutes() {
        let d = Duration::from_secs(90);
        assert_eq!(time_ago_duration(d), "1m ago");
        let d2 = Duration::from_secs(3599);
        assert_eq!(time_ago_duration(d2), "59m ago");
    }

    #[test]
    fn time_ago_formats_hours() {
        let d = Duration::from_secs(7200);
        assert_eq!(time_ago_duration(d), "2h ago");
    }

    #[test]
    fn time_ago_formats_days() {
        let d = Duration::from_secs(172800);
        assert_eq!(time_ago_duration(d), "2d ago");
    }

    #[test]
    fn history_trigger_char_is_hash() {
        let store = Arc::new(RwLock::new(make_store(50)));
        let handler = HistoryTriggerHandler::new(store);
        assert_eq!(handler.trigger_char(), '#');
    }

    #[test]
    fn history_trigger_does_not_allow_free_text() {
        let store = Arc::new(RwLock::new(make_store(50)));
        let handler = HistoryTriggerHandler::new(store);
        assert!(!handler.allows_free_text());
    }

    #[test]
    fn history_trigger_matches_returns_items() {
        let store_inner = Arc::new(RwLock::new(make_store(50)));
        {
            let mut s = store_inner.write().unwrap();
            s.push(
                "Fix the type error in auth.rs".to_string(),
                HistoryKind::AgentTask,
            );
            s.push("/compact".to_string(), HistoryKind::SlashCommand);
        }
        let handler = HistoryTriggerHandler::new(Arc::clone(&store_inner));
        let ctx = TriggerContext::new_for_test("", '#');
        let items = handler.matches(&ctx);
        assert_eq!(items.len(), 2, "must return 2 items");
        // Newest first: /compact was pushed last.
        assert_eq!(items[0].insert, "/compact");
        assert_eq!(items[1].insert, "Fix the type error in auth.rs");
    }

    #[test]
    fn history_trigger_display_truncates_long_text() {
        let long_text = "A".repeat(80);
        let store_inner = Arc::new(RwLock::new(make_store(50)));
        {
            let mut s = store_inner.write().unwrap();
            s.push(long_text.clone(), HistoryKind::AgentTask);
        }
        let handler = HistoryTriggerHandler::new(Arc::clone(&store_inner));
        let ctx = TriggerContext::new_for_test("", '#');
        let items = handler.matches(&ctx);
        assert_eq!(items.len(), 1);
        // display must be <= 60 visible chars (57 + "…")
        let display = &items[0].display;
        let char_count = display.chars().count();
        assert!(
            char_count <= 60,
            "display must be ≤ 60 chars but got {char_count}: {display:?}"
        );
        assert!(
            display.ends_with('…'),
            "truncated display must end with '…'"
        );
        // insert must be the full text.
        assert_eq!(items[0].insert, long_text);
    }

    #[test]
    fn history_trigger_on_select_returns_full_text() {
        let store_inner = Arc::new(RwLock::new(make_store(50)));
        let handler = HistoryTriggerHandler::new(Arc::clone(&store_inner));
        let item = MatchItem {
            display: "short…".to_string(),
            insert: "full original text".to_string(),
            hint: None,
            kind: MatchKind::Custom("history".into()),
        };
        let ctx = TriggerContext::new_for_test("", '#');
        let result = handler.on_select(&item, &ctx);
        assert_eq!(result, "full original text");
    }

    #[test]
    fn history_kind_label_is_correct() {
        assert_eq!(HistoryKind::AgentTask.label(), "task");
        assert_eq!(HistoryKind::SlashCommand.label(), "cmd");
    }

    #[test]
    fn search_case_insensitive() {
        let mut store = make_store(50);
        store.push("Fix THE AUTH Bug".to_string(), HistoryKind::AgentTask);
        let results = store.search("auth");
        assert_eq!(results.len(), 1);
        let results_upper = store.search("AUTH");
        assert_eq!(results_upper.len(), 1);
    }

    #[test]
    fn adjacent_identical_pushes_are_not_deduped() {
        let mut store = make_store(50);
        store.push("same task".to_string(), HistoryKind::AgentTask);
        store.push("same task".to_string(), HistoryKind::AgentTask);
        assert_eq!(
            store.len(),
            2,
            "InputHistoryStore must NOT dedup adjacent identical entries (unlike SlashHistory)"
        );
    }

    #[test]
    fn all_newest_first_iterator() {
        let mut store = make_store(50);
        store.push("a".to_string(), HistoryKind::AgentTask);
        store.push("b".to_string(), HistoryKind::AgentTask);
        let texts: Vec<&str> = store.all_newest_first().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["b", "a"]);
    }
}
