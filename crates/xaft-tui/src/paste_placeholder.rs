//! Bracketed-paste placeholder projection — agenthicc parity (Gap 8).
//!
//! agenthicc (`docs/guides/tui.md`) keeps large bracketed pastes behind a
//! single `[Pasted text #N ...]` composer placeholder while the user edits:
//! - `Home`/`End` operate on the visible one-line projection.
//! - `Backspace` immediately after the closing `]` deletes the whole paste;
//!   elsewhere it deletes one hidden character at a time.
//! - `Ctrl+V` reveals the full pasted text.
//! - `Esc` after the `]` deletes the entire hidden paste.
//! - `Enter` submits the remaining original contents plus edits.
//!
//! This module is a pure, unit-testable projection: it maintains the hidden
//! payload and the visible one-line view, and exposes the operations without
//! touching the terminal.

/// The max hidden-payload preview length shown inside the placeholder.
pub const PLACEHOLDER_PREVIEW_LEN: usize = 40;

/// State of the paste placeholder projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePlaceholder {
    /// The full hidden payload (original pasted text, preserving newlines).
    payload: String,
    /// Whether the placeholder is currently revealed (`Ctrl+V`).
    revealed: bool,
    /// Cursor offset within the *visible* projection (0..=visible_len).
    cursor: usize,
    /// Whether the cursor is immediately after the closing `]`.
    cursor_after_close: bool,
    /// Edits typed in front of the placeholder (inserted before the payload).
    prefix_edits: String,
    /// Edits typed after the placeholder (inserted after the payload).
    suffix_edits: String,
}

impl PastePlaceholder {
    /// Create a placeholder for `payload`.
    pub fn new(payload: impl Into<String>) -> Self {
        Self {
            payload: payload.into(),
            revealed: false,
            cursor: 0,
            cursor_after_close: true,
            prefix_edits: String::new(),
            suffix_edits: String::new(),
        }
    }

    /// The hidden payload (original pasted text).
    pub fn payload(&self) -> &str {
        &self.payload
    }

    /// Whether the full text is revealed.
    pub fn is_revealed(&self) -> bool {
        self.revealed
    }

    /// The placeholder display: `[Pasted text #N ...]` where `N` is the
    /// byte length of the payload and `...` is a preview of the first
    /// `PLACEHOLDER_PREVIEW_LEN` chars (newlines collapsed to `⏎` so the
    /// projection stays on one line). The `…` is a literal separator after
    /// the preview, matching agenthicc's `[Pasted text #N ...]` copy.
    pub fn placeholder_display(&self) -> String {
        let preview: String = self
            .payload
            .chars()
            .take(PLACEHOLDER_PREVIEW_LEN)
            .map(|c| if c == '\n' { '⏎' } else { c })
            .collect();
        format!("[Pasted text #{} {preview}…]", self.payload.len())
    }

    /// The visible one-line projection: the placeholder (or the full payload
    /// when revealed) plus prefix/suffix edits.
    pub fn visible(&self) -> String {
        let core = if self.revealed {
            self.payload.replace('\n', "⏎")
        } else {
            self.placeholder_display()
        };
        format!("{}{}{}", self.prefix_edits, core, self.suffix_edits)
    }

    /// Full text that would be submitted (payload + edits, newlines intact).
    pub fn submit_text(&self) -> String {
        format!("{}{}{}", self.prefix_edits, self.payload, self.suffix_edits)
    }

    /// Toggle reveal (`Ctrl+V`): show the full payload inline.
    pub fn toggle_reveal(&mut self) {
        self.revealed = !self.revealed;
        self.cursor_after_close = false;
    }

    /// Delete at the cursor.
    ///
    /// When the cursor is immediately after the closing `]` and not revealed,
    /// deletes the **entire** hidden paste (agenthicc parity). Otherwise
    /// deletes one hidden character at a time from the suffix (or a prefix
    /// char when in the prefix region).
    pub fn backspace(&mut self) {
        if !self.revealed && self.cursor_after_close {
            // Cursor right after `]` → delete the whole paste.
            self.payload.clear();
            self.cursor_after_close = false;
            self.cursor = self.visible().len();
            return;
        }
        if !self.suffix_edits.is_empty() {
            self.suffix_edits.pop();
        } else if !self.payload.is_empty() {
            self.payload.pop();
        } else if !self.prefix_edits.is_empty() {
            self.prefix_edits.pop();
        }
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// `Esc` behaviour: when the cursor is immediately after the closing `]`
    /// and not revealed, delete the entire hidden paste. Otherwise no-op
    /// (the caller keeps normal Esc handling).
    pub fn escape(&mut self) {
        if !self.revealed && self.cursor_after_close {
            self.payload.clear();
            self.cursor_after_close = false;
            self.cursor = self.visible().len();
        }
    }

    /// `Home`: move to the start of the visible projection.
    pub fn home(&mut self) {
        self.cursor = 0;
        self.cursor_after_close = false;
    }

    /// `End`: move to the end of the visible projection.
    pub fn end(&mut self) {
        self.cursor = self.visible().len();
        self.cursor_after_close = true;
    }

    /// Type a character at the cursor (suffix region when after the close).
    pub fn type_char(&mut self, c: char) {
        if self.cursor_after_close && !self.revealed {
            self.suffix_edits.push(c);
        } else {
            // In the prefix region or revealed — prepend to suffix edits
            // (simplified: all typed text goes to the suffix when after the
            // core; otherwise to the prefix).
            self.prefix_edits.push(c);
        }
        self.cursor = self.cursor.saturating_add(1);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_display_short() {
        let p = PastePlaceholder::new("hello");
        assert_eq!(p.placeholder_display(), "[Pasted text #5 hello…]");
    }

    #[test]
    fn placeholder_display_truncates() {
        let long = "x".repeat(100);
        let p = PastePlaceholder::new(&long);
        let d = p.placeholder_display();
        assert!(d.contains("[Pasted text #100 "));
        assert!(d.ends_with("…]"));
        assert!(d.chars().count() < 100);
    }

    #[test]
    fn visible_shows_placeholder() {
        let p = PastePlaceholder::new("line1\nline2");
        assert_eq!(p.visible(), "[Pasted text #11 line1⏎line2…]");
    }

    #[test]
    fn reveal_shows_payload_with_newline_marker() {
        let mut p = PastePlaceholder::new("a\nb");
        p.toggle_reveal();
        assert!(p.is_revealed());
        assert_eq!(p.visible(), "a⏎b");
        assert_eq!(p.submit_text(), "a\nb");
    }

    #[test]
    fn backspace_after_close_deletes_whole() {
        let mut p = PastePlaceholder::new("hello world");
        p.end(); // cursor after ]
        p.backspace();
        assert_eq!(p.payload(), "");
        assert_eq!(p.submit_text(), "");
    }

    #[test]
    fn backspace_not_after_close_deletes_one() {
        let mut p = PastePlaceholder::new("abc");
        p.home(); // not after ]
        p.backspace();
        // prefix region is empty; payload shrinks by one
        assert_eq!(p.payload(), "ab");
    }

    #[test]
    fn escape_after_close_deletes_whole() {
        let mut p = PastePlaceholder::new("data");
        p.end();
        p.escape();
        assert_eq!(p.payload(), "");
    }

    #[test]
    fn escape_not_after_close_noop() {
        let mut p = PastePlaceholder::new("data");
        p.home();
        p.escape();
        assert_eq!(p.payload(), "data");
    }

    #[test]
    fn home_end_cursor_positions() {
        let mut p = PastePlaceholder::new("x");
        p.end();
        assert!(p.cursor_after_close);
        p.home();
        assert!(!p.cursor_after_close);
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn submit_preserves_newlines() {
        let mut p = PastePlaceholder::new("a\nb\nc");
        p.end();
        p.type_char('!');
        assert_eq!(p.submit_text(), "a\nb\nc!");
    }

    #[test]
    fn type_after_close_goes_to_suffix() {
        let mut p = PastePlaceholder::new("core");
        p.end();
        p.type_char('Z');
        assert_eq!(p.submit_text(), "coreZ");
    }
}
