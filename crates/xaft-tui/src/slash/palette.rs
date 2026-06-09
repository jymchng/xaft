//! Autocomplete palette overlay for slash commands.

use std::collections::VecDeque;

use super::parser::{COMMAND_TABLE, SlashCommandParser};

// ── SlashPalette ──────────────────────────────────────────────────────────────

/// Maximum number of palette rows visible at one time.
pub const MAX_PALETTE_VISIBLE: usize = 8;

/// State for the slash-command autocomplete palette.
#[derive(Debug, Clone)]
pub struct SlashPalette {
    /// The text currently typed after '/' (no leading slash).
    pub partial: String,
    /// Filtered, sorted list of matching trigger strings.
    pub candidates: Vec<&'static str>,
    /// Index of the highlighted candidate (0-based).
    pub selected: usize,
    /// Index of the first visible candidate (scroll offset).
    pub scroll_top: usize,
    /// Per-candidate descriptions (parallel to `candidates`).
    pub descriptions: Vec<&'static str>,
    /// Per-candidate args hints (parallel to `candidates`).
    pub args_hints: Vec<Option<&'static str>>,
}

impl SlashPalette {
    /// Create a new palette for the given partial trigger text (no leading slash).
    pub fn new(partial: &str) -> Self {
        let candidates = SlashCommandParser::completions(partial);
        let descriptions: Vec<&'static str> = candidates
            .iter()
            .map(|t| {
                COMMAND_TABLE
                    .iter()
                    .find(|(k, _)| k == t)
                    .map(|(_, m)| m.description)
                    .unwrap_or("")
            })
            .collect();
        let args_hints: Vec<Option<&'static str>> = candidates
            .iter()
            .map(|t| {
                COMMAND_TABLE
                    .iter()
                    .find(|(k, _)| k == t)
                    .and_then(|(_, m)| m.args_hint)
            })
            .collect();
        Self {
            partial: partial.to_string(),
            candidates,
            selected: 0,
            scroll_top: 0,
            descriptions,
            args_hints,
        }
    }

    /// Update the palette with a new partial string (resets selection).
    pub fn update(&mut self, partial: &str) {
        *self = Self::new(partial);
    }

    /// Return the currently selected trigger, if any.
    pub fn selected_trigger(&self) -> Option<&&'static str> {
        self.candidates.get(self.selected)
    }

    /// Advance selection to the next candidate (wraps around), scrolling
    /// the visible window so the selected row is always in view.
    pub fn select_next(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        let new_sel = (self.selected + 1) % self.candidates.len();
        self.selected = new_sel;
        if new_sel == 0 {
            self.scroll_top = 0;
        } else if new_sel >= self.scroll_top + MAX_PALETTE_VISIBLE {
            self.scroll_top = new_sel + 1 - MAX_PALETTE_VISIBLE;
        }
    }

    /// Move selection to the previous candidate (wraps around), scrolling
    /// the visible window so the selected row is always in view.
    pub fn select_prev(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        let new_sel = self
            .selected
            .checked_sub(1)
            .unwrap_or(self.candidates.len() - 1);
        self.selected = new_sel;
        if new_sel == self.candidates.len() - 1 {
            self.scroll_top = self.candidates.len().saturating_sub(MAX_PALETTE_VISIBLE);
        } else if new_sel < self.scroll_top {
            self.scroll_top = new_sel;
        }
    }
}

// ── SlashHistory ──────────────────────────────────────────────────────────────

/// Ring buffer of recently executed slash commands.
#[derive(Debug, Clone)]
pub struct SlashHistory {
    ring: VecDeque<String>,
    capacity: usize,
    cursor: Option<usize>, // None = not browsing history
}

impl Default for SlashHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashHistory {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::new(),
            capacity: 50,
            cursor: None,
        }
    }

    /// Push a command to history. Oldest entries are dropped when capacity is exceeded.
    pub fn push(&mut self, cmd: String) {
        // Don't push duplicates adjacent to each other.
        if self.ring.front().map(|s| s.as_str()) != Some(&cmd) {
            self.ring.push_front(cmd);
            while self.ring.len() > self.capacity {
                self.ring.pop_back();
            }
        }
        self.cursor = None;
    }

    /// Navigate backward through history (older entries).
    /// Returns the entry to put in the input bar, or `None` if at oldest.
    pub fn prev(&mut self, _current: &str) -> Option<&str> {
        if self.ring.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => 0,
            Some(c) => (c + 1).min(self.ring.len() - 1),
        };
        self.cursor = Some(next);
        self.ring.get(next).map(|s| s.as_str())
    }

    /// Navigate forward through history (more recent entries).
    /// Returns `None` when cursor is back at the current input.
    pub fn next(&mut self) -> Option<&str> {
        match self.cursor {
            None => None,
            Some(0) => {
                self.cursor = None;
                None
            }
            Some(c) => {
                self.cursor = Some(c - 1);
                self.ring.get(c - 1).map(|s| s.as_str())
            }
        }
    }

    /// Reset cursor (called when user types a non-navigation character).
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
    }

    /// Number of entries in history.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_new_empty_partial_has_all_candidates() {
        let p = SlashPalette::new("");
        assert!(
            p.candidates.len() >= 21,
            "expected at least 21 candidates, got {}",
            p.candidates.len()
        );
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn palette_new_partial_filters() {
        let p = SlashPalette::new("cle");
        // "cle" matches "clear" only (not "cls")
        assert!(
            p.candidates.contains(&"clear"),
            "expected clear in {:?}",
            p.candidates
        );
        // should not have unrelated commands
        assert!(!p.candidates.contains(&"help"));
    }

    #[test]
    fn palette_cl_has_clear_and_cls() {
        let p = SlashPalette::new("cl");
        assert!(p.candidates.contains(&"clear"), "expected clear");
        assert!(p.candidates.contains(&"cls"), "expected cls");
    }

    #[test]
    fn palette_select_next_wraps() {
        let mut p = SlashPalette::new("c");
        let n = p.candidates.len();
        assert!(n > 0, "need candidates for wrap test");
        for _ in 0..n {
            p.select_next();
        }
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn palette_select_prev_wraps() {
        let mut p = SlashPalette::new("c");
        let n = p.candidates.len();
        assert!(n > 0);
        p.select_prev();
        assert_eq!(p.selected, n - 1);
    }

    #[test]
    fn palette_update_resets_selection() {
        let mut p = SlashPalette::new("c");
        p.select_next();
        p.select_next();
        p.update("cle");
        assert_eq!(p.selected, 0);
        assert!(p.candidates.contains(&"clear"));
    }

    #[test]
    fn palette_selected_trigger_none_when_empty() {
        let p = SlashPalette::new("zzz");
        assert!(p.candidates.is_empty());
        assert!(p.selected_trigger().is_none());
    }

    #[test]
    fn history_push_and_len() {
        let mut h = SlashHistory::new();
        h.push("/clear".into());
        h.push("/help".into());
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn history_prev_next() {
        let mut h = SlashHistory::new();
        h.push("/clear".into());
        h.push("/help".into());
        // Most recent is first
        assert_eq!(h.prev(""), Some("/help"));
        assert_eq!(h.prev(""), Some("/clear"));
        assert_eq!(h.next(), Some("/help"));
        assert_eq!(h.next(), None);
    }
}
