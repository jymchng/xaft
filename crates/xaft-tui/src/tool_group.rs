//! Tool-group collapse tracker — agenthicc parity (Gap 3).
//!
//! agenthicc's `ScrollBufferAppender` batches contiguous tool completions
//! into a group; the overflow count lives in the live footer while the group
//! is open, and at the next conversation boundary (or interrupt) the appender
//! flushes `...and N more tool calls` to the scroll buffer — never left only
//! in the footer (`src/agenthicc/tui/workspace/appender.py`).
//!
//! This module provides the same bookkeeping as a pure, unit-testable state
//! machine: count consecutive completed tool calls, snapshot the group, and
//! produce the flush summary line when a boundary is reached.

/// Live bookkeeping for a contiguous run of completed tool calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolGroupTracker {
    /// Number of tool calls completed in the current open group.
    count: usize,
    /// Names of tools completed in the current group (for the summary line).
    names: Vec<String>,
}

impl Default for ToolGroupTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolGroupTracker {
    /// Create an empty tracker (no open group).
    pub fn new() -> Self {
        Self {
            count: 0,
            names: Vec::new(),
        }
    }

    /// Record a completed tool call. `tool_name` is the snake_case tool name
    /// (e.g. `"read_file"`); it is stored for the summary.
    pub fn record_completed(&mut self, tool_name: &str) {
        self.count += 1;
        if self.names.len() < 8 {
            self.names.push(tool_name.to_string());
        }
    }

    /// Whether a group is currently open (at least one completed call since
    /// the last boundary).
    pub fn is_open(&self) -> bool {
        self.count > 0
    }

    /// Number of tool calls in the open group.
    pub fn count(&self) -> usize {
        self.count
    }

    /// The distinct tool names seen in this group (first-seen order, capped at
    /// 8 for the summary line).
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Close the group and return the flush summary line, or `None` when the
    /// group had fewer than 2 calls (agenthicc only shows the overflow line
    /// when there is more than one call to collapse).
    ///
    /// The returned line matches agenthicc's copy:
    /// `...and N more tool calls` (where `N = count - 1`).
    pub fn flush(&mut self) -> Option<String> {
        let summary = if self.count > 1 {
            Some(format!(
                "…and {} more tool call{}",
                self.count - 1,
                if self.count - 1 == 1 { "" } else { "s" }
            ))
        } else {
            None
        };
        self.reset();
        summary
    }

    /// Abandon the group without producing a summary (used at a hard reset).
    pub fn reset(&mut self) {
        self.count = 0;
        self.names.clear();
    }

    /// First-seen distinct names (for display).
    pub fn distinct_names(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for n in &self.names {
            if !seen.contains(n) {
                seen.push(n.clone());
            }
        }
        seen
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_closed() {
        let mut t = ToolGroupTracker::new();
        assert!(!t.is_open());
        assert_eq!(t.count(), 0);
        assert!(t.flush().is_none());
    }

    #[test]
    fn records_completed_calls() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("read_file");
        t.record_completed("grep");
        assert!(t.is_open());
        assert_eq!(t.count(), 2);
        assert_eq!(t.names(), &["read_file", "grep"]);
    }

    #[test]
    fn flush_single_call_returns_none() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("read_file");
        assert!(t.flush().is_none());
        assert!(!t.is_open());
    }

    #[test]
    fn flush_multi_call_returns_summary() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("read_file");
        t.record_completed("read_file");
        t.record_completed("grep");
        let summary = t.flush().unwrap();
        assert_eq!(summary, "…and 2 more tool calls");
        assert!(!t.is_open());
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn flush_two_calls_singular() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("read_file");
        t.record_completed("write_file");
        let summary = t.flush().unwrap();
        assert_eq!(summary, "…and 1 more tool call");
    }

    #[test]
    fn reset_clears() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("a");
        t.record_completed("b");
        t.reset();
        assert!(!t.is_open());
        assert!(t.names().is_empty());
    }

    #[test]
    fn distinct_names_dedupes() {
        let mut t = ToolGroupTracker::new();
        t.record_completed("read_file");
        t.record_completed("read_file");
        t.record_completed("grep");
        assert_eq!(t.distinct_names(), vec!["read_file", "grep"]);
    }

    #[test]
    fn names_capped_at_eight() {
        let mut t = ToolGroupTracker::new();
        for i in 0..12 {
            t.record_completed(&format!("tool_{i}"));
        }
        assert_eq!(t.names().len(), 8);
        assert_eq!(t.count(), 12);
    }
}
