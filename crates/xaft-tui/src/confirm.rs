//! F3 escape confirmation dialog widget.
//!
//! Modal dialog shown when the user submits a message that contains one or
//! more `@<path>` mentions that escape the workspace (per PRD 30a §8.7).
//!
//! Key bindings:
//! - `a` / `Enter`  — approve this submission
//! - `A`            — approve all escape mentions for the rest of the session
//! - `c` / `Esc`    — cancel; restore the original input bar text
//! - `?`            — toggle the help text
//!
//! The dialog is rendered as an overlay on top of the normal transcript
//! surface. It does **not** clear the screen; it draws inside a bordered
//! block that floats above the conversation.

use crate::{EscapeInfo, EscapeReason};

/// Outcome of a confirmation dialog round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeConfirmOutcome {
    /// User approved this submission (one-shot).
    ApproveOnce,
    /// User approved for the rest of the session (all future escape
    /// mentions skip the dialog).
    ApproveAllSession,
    /// User cancelled; the input bar is restored and no message is sent.
    Cancel,
}

/// State for the escape confirmation dialog.
#[derive(Debug, Clone)]
pub struct EscapeConfirmDialog {
    /// The escape mentions that need confirmation.
    pub mentions: Vec<EscapeInfo>,
    /// Whether the help text is currently expanded.
    pub help_expanded: bool,
    /// Index of the focused mention in `mentions` (for keyboard nav, v2).
    /// v1 doesn't support per-mention approval; we always approve all or
    /// cancel all, so this is informational.
    pub focus_index: usize,
}

impl EscapeConfirmDialog {
    /// Create a new dialog for the given escape mentions.
    pub fn new(mentions: Vec<EscapeInfo>) -> Self {
        Self {
            mentions,
            help_expanded: false,
            focus_index: 0,
        }
    }

    /// Number of escape mentions in this dialog.
    pub fn count(&self) -> usize {
        self.mentions.len()
    }

    /// Total bytes across all escape mentions (for the header summary).
    pub fn total_bytes(&self) -> u64 {
        self.mentions.iter().map(|m| m.byte_size).sum()
    }

    /// Toggle the help panel.
    pub fn toggle_help(&mut self) {
        self.help_expanded = !self.help_expanded;
    }

    /// Render the dialog's header line (one line of text). The renderer
    /// is responsible for box-drawing around it.
    pub fn header(&self) -> String {
        let n = self.count();
        let bytes = self.total_bytes();
        let plural = if n == 1 { "mention" } else { "mentions" };
        format!("{n} escape {plural} ({bytes} bytes total) — review before attaching")
    }

    /// Render one line per escape mention. The renderer inserts row
    /// numbers, alignment, and bullet markers.
    pub fn lines(&self) -> Vec<DialogLine> {
        self.mentions
            .iter()
            .enumerate()
            .map(|(i, m)| DialogLine {
                index: i + 1,
                reason: reason_label(m.reason).to_string(),
                path: m.absolute_path.clone(),
                bytes: m.byte_size,
                depth: m.depth,
            })
            .collect()
    }

    /// Help text (multiline). Returned as separate lines for the renderer
    /// to wrap or format.
    pub fn help_text(&self) -> &'static [&'static str] {
        &[
            "Press a / Enter to approve this submission only.",
            "Press A to approve all escape mentions for the rest of the session.",
            "Press c / Esc to cancel and restore the input bar.",
            "",
            "Escape mentions are paths outside the workspace. The contents",
            "of these files will be sent to the LLM provider, and the LLM",
            "may echo them back or summarise them. They are also written to",
            "the audit log with their absolute paths and SHA-256 hashes.",
            "",
            "To disable this dialog permanently, set",
            "  [mention] escape_policy = \"always\"",
            "in your xaft config. To disable escape mentions entirely, set",
            "  [mention] escape_policy = \"never\".",
        ]
    }
}

/// One row in the dialog's file list.
#[derive(Debug, Clone)]
pub struct DialogLine {
    /// 1-based row number.
    pub index: usize,
    /// Human-readable reason label (e.g. "parent traversal").
    pub reason: String,
    /// Canonical absolute path on disk.
    pub path: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Number of `..` segments in the path (0 for non-traversal).
    pub depth: u32,
}

impl DialogLine {
    /// Render the line as a single string, e.g.
    /// `1. parent_traversal  /etc/hosts  (320 bytes)`.
    pub fn render(&self) -> String {
        let depth_hint = if self.depth > 1 {
            format!("  ({} levels up)", self.depth)
        } else {
            String::new()
        };
        format!(
            "{:>2}. {:<16}  {}  ({} bytes){}",
            self.index, self.reason, self.path, self.bytes, depth_hint,
        )
    }
}

/// Map a [`EscapeReason`] to a human-readable label.
pub fn reason_label(r: EscapeReason) -> &'static str {
    match r {
        EscapeReason::Absolute => "absolute path",
        EscapeReason::ParentTraversal => "parent traversal",
        EscapeReason::HomeExpansion => "home expansion",
    }
}

/// Public alias for [`reason_label`] — re-exported so external tests
/// can assert on the same string the dialog renders.
pub fn escape_reason_str(r: EscapeReason) -> &'static str {
    reason_label(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EscapeReason;

    fn info(reason: EscapeReason, path: &str, bytes: u64, depth: u32) -> EscapeInfo {
        EscapeInfo {
            reason,
            absolute_path: path.to_string(),
            depth,
            byte_size: bytes,
        }
    }

    #[test]
    fn dialog_count_and_total_bytes() {
        let d = EscapeConfirmDialog::new(vec![
            info(EscapeReason::Absolute, "/etc/hosts", 320, 0),
            info(EscapeReason::ParentTraversal, "../sibling/x.rs", 1024, 1),
        ]);
        assert_eq!(d.count(), 2);
        assert_eq!(d.total_bytes(), 1344);
    }

    #[test]
    fn dialog_header_pluralisation() {
        let d1 = EscapeConfirmDialog::new(vec![info(EscapeReason::Absolute, "/etc/hosts", 320, 0)]);
        assert!(d1.header().contains("1 escape mention"));
        assert!(!d1.header().contains("mentions"));
        let d2 = EscapeConfirmDialog::new(vec![
            info(EscapeReason::Absolute, "/a", 1, 0),
            info(EscapeReason::Absolute, "/b", 2, 0),
        ]);
        assert!(d2.header().contains("2 escape mentions"));
    }

    #[test]
    fn dialog_lines_render_with_index_and_bytes() {
        let d = EscapeConfirmDialog::new(vec![info(
            EscapeReason::ParentTraversal,
            "../foo.rs",
            100,
            1,
        )]);
        let lines = d.lines();
        assert_eq!(lines.len(), 1);
        let rendered = lines[0].render();
        assert!(rendered.starts_with(" 1."));
        assert!(rendered.contains("parent traversal"));
        assert!(rendered.contains("../foo.rs"));
        assert!(rendered.contains("100 bytes"));
    }

    #[test]
    fn dialog_lines_render_with_depth_hint() {
        let d = EscapeConfirmDialog::new(vec![info(
            EscapeReason::ParentTraversal,
            "../../deep.rs",
            50,
            2,
        )]);
        let rendered = d.lines()[0].render();
        assert!(rendered.contains("2 levels up"));
    }

    #[test]
    fn dialog_lines_no_depth_hint_for_depth_1() {
        let d = EscapeConfirmDialog::new(vec![info(
            EscapeReason::ParentTraversal,
            "../foo.rs",
            50,
            1,
        )]);
        let rendered = d.lines()[0].render();
        assert!(!rendered.contains("levels up"));
    }

    #[test]
    fn toggle_help_flips_state() {
        let mut d = EscapeConfirmDialog::new(vec![]);
        assert!(!d.help_expanded);
        d.toggle_help();
        assert!(d.help_expanded);
        d.toggle_help();
        assert!(!d.help_expanded);
    }

    #[test]
    fn help_text_contains_key_bindings() {
        let d = EscapeConfirmDialog::new(vec![]);
        let h = d.help_text().join("\n");
        assert!(h.contains("a / Enter"));
        assert!(h.contains("A"));
        assert!(h.contains("c / Esc"));
        assert!(h.contains("escape_policy"));
    }

    #[test]
    fn reason_label_covers_all_variants() {
        assert_eq!(reason_label(EscapeReason::Absolute), "absolute path");
        assert_eq!(
            reason_label(EscapeReason::ParentTraversal),
            "parent traversal"
        );
        assert_eq!(reason_label(EscapeReason::HomeExpansion), "home expansion");
    }

    #[test]
    fn empty_dialog_is_well_formed() {
        let d = EscapeConfirmDialog::new(vec![]);
        assert_eq!(d.count(), 0);
        assert_eq!(d.total_bytes(), 0);
        assert!(d.lines().is_empty());
        // Header still renders (no panics).
        let _ = d.header();
    }
}
