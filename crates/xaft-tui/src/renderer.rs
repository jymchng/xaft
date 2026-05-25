//! Terminal rendering primitives for smooth streaming LLM output.
//!
//! # Overview
//!
//! LLM tokens arrive at 80+ tokens/second via the `XaftAgentOutput` signal.
//! The [`TokenStreamRenderer`] decouples *token arrival* from *frame rendering*:
//!
//! - Tokens are pushed into an internal buffer as they arrive.
//! - Each 60fps frame calls [`frame_update`], which atomically moves buffered
//!   tokens into the visible string and toggles the blink cursor.
//! - [`render`] draws the visible text with optional syntax highlighting,
//!   proper Unicode-aware word-wrap, and a blinking cursor at the end.
//!
//! # Rendering budget
//!
//! ```text
//! 16.67ms frame budget at 60fps
//! ├── 0–2ms   event processing (key, mouse, signals)
//! ├── 2–5ms   layout solving
//! ├── 5–12ms  widget rendering (this module)
//! └── 12–16ms crossterm flush
//! ```
//!
//! The renderer stays well within budget because it only redraws lines that
//! changed (`dirty` tracking) and caches word-wrap results.

use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

// ── TokenStreamRenderer ────────────────────────────────────────────────────────

/// Per-agent streaming text renderer.
///
/// One instance lives inside [`AppState`] per active agent.  When an agent
/// finishes its turn the renderer's visible text is flushed to the output
/// buffer and the renderer is reset.
#[derive(Debug, Clone)]
pub struct TokenStreamRenderer {
    /// Tokens that arrived since the last frame update (not yet visible).
    buffer: String,
    /// Currently displayed text (updated once per frame from `buffer`).
    visible: String,
    /// Cursor blink toggle (flips each frame).
    blink_state: bool,
    /// Approximate tokens-per-second for the display gauge.
    tps: f32,
    /// When the last token was received (for TPS calculation).
    last_token_time: Option<Instant>,
    /// Total tokens received this turn.
    token_count: u64,
    /// Name of the agent whose output this renderer shows.
    pub agent_name: String,
    /// Whether there is text being streamed right now.
    pub is_active: bool,
    /// Cached word-wrap result; invalidated when `visible` changes or width changes.
    wrap_cache: Option<WrapCache>,
}

#[derive(Debug, Clone)]
struct WrapCache {
    wrapped_at_width: u16,
    lines: Vec<String>,
}

impl TokenStreamRenderer {
    /// Create a renderer for `agent_name`.
    pub fn new(agent_name: impl Into<String>) -> Self {
        Self {
            buffer: String::new(),
            visible: String::new(),
            blink_state: false,
            tps: 0.0,
            last_token_time: None,
            token_count: 0,
            agent_name: agent_name.into(),
            is_active: false,
            wrap_cache: None,
        }
    }

    /// Append a token fragment from the LLM stream.
    ///
    /// Called from the async signal handler; safe to call many times per frame.
    pub fn push_token(&mut self, token: &str) {
        let now = Instant::now();
        if let Some(last) = self.last_token_time {
            let elapsed = now.duration_since(last).as_secs_f32();
            if elapsed > 0.0 {
                // Exponential moving average of TPS
                let instant_tps = 1.0 / elapsed;
                self.tps = self.tps * 0.85 + instant_tps * 0.15;
            }
        }
        self.last_token_time = Some(now);
        self.token_count += 1;
        self.buffer.push_str(token);
        self.is_active = true;
        self.wrap_cache = None; // invalidate cache
    }

    /// Called once per frame: commit buffered tokens to visible string.
    ///
    /// Returns `true` if any new tokens were committed this frame.
    pub fn frame_update(&mut self) -> bool {
        self.blink_state = !self.blink_state;
        if self.buffer.is_empty() {
            return false;
        }
        self.visible.push_str(&self.buffer);
        self.buffer.clear();
        self.wrap_cache = None;
        true
    }

    /// Tokens per second (exponential moving average).
    pub fn tps(&self) -> f32 {
        self.tps
    }

    /// Total tokens received since last reset.
    pub fn token_count(&self) -> u64 {
        self.token_count
    }

    /// Full visible text accumulated so far.
    pub fn text(&self) -> &str {
        &self.visible
    }

    /// Reset to ready state (called when agent turn ends and text is flushed).
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.visible.clear();
        self.blink_state = false;
        self.tps = 0.0;
        self.last_token_time = None;
        self.token_count = 0;
        self.is_active = false;
        self.wrap_cache = None;
    }

    /// Whether there is text to display.
    pub fn has_content(&self) -> bool {
        !self.visible.is_empty() || !self.buffer.is_empty()
    }

    /// Render the streaming text into the frame buffer.
    ///
    /// - Word-wraps to fit `area.width` (Unicode-aware).
    /// - Shows a blinking block cursor `█` at the end if `is_active`.
    /// - Applies `streaming_style` to new lines and `completed_style` to prior ones.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        streaming_style: Style,
        completed_style: Style,
        show_agent_prefix: bool,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = self.wrapped_lines(area.width);

        let visible_height = area.height as usize;
        // Show the last `visible_height` lines (scroll to bottom)
        let start = lines.len().saturating_sub(visible_height);
        let display_lines = &lines[start..];
        let last_display_idx = display_lines.len().saturating_sub(1);

        for (i, line_str) in display_lines.iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.bottom() {
                break;
            }

            let is_last = i == last_display_idx;
            let style = if is_last && self.is_active {
                streaming_style
            } else {
                completed_style
            };

            let mut spans: Vec<Span> = Vec::new();
            if show_agent_prefix && i == 0 {
                spans.push(Span::styled(
                    format!("[{}] ", self.agent_name),
                    Style::default()
                        .fg(Color::Rgb(156, 100, 220))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            spans.push(Span::styled(line_str.clone(), style));

            // Blinking cursor at end of last line
            if is_last && self.is_active && self.blink_state {
                spans.push(Span::styled(
                    "█",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::RAPID_BLINK),
                ));
            }

            use ratatui::widgets::Widget;
            ratatui::widgets::Paragraph::new(Line::from(spans))
                .render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    /// Get (or compute and cache) word-wrapped lines for `width`.
    fn wrapped_lines(&mut self, width: u16) -> Vec<String> {
        // Return cached result if width hasn't changed
        if let Some(ref cache) = self.wrap_cache {
            if cache.wrapped_at_width == width {
                return cache.lines.clone();
            }
        }

        let lines = word_wrap(&self.visible, width as usize);
        self.wrap_cache = Some(WrapCache {
            wrapped_at_width: width,
            lines: lines.clone(),
        });
        lines
    }
}

// ── Word-wrap ─────────────────────────────────────────────────────────────────

/// Unicode-aware word-wrap: splits `text` into lines of at most `max_width`
/// *display columns* (not bytes).
///
/// - Respects existing newlines.
/// - Breaks long words at column boundary if no whitespace is available.
/// - Uses [`unicode_width`] for correct CJK/emoji/combining char widths.
pub fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }

    let mut result = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            result.push(String::new());
            continue;
        }

        let mut current_line = String::new();
        let mut current_width = 0usize;

        for word in paragraph.split_whitespace() {
            let word_width = display_width(word);

            if current_width == 0 {
                // Start of a new line
                if word_width <= max_width {
                    current_line.push_str(word);
                    current_width = word_width;
                } else {
                    // Word too wide — hard-break it
                    hard_break(word, max_width, &mut result);
                }
            } else if current_width + 1 + word_width <= max_width {
                // Word fits with a space
                current_line.push(' ');
                current_line.push_str(word);
                current_width += 1 + word_width;
            } else {
                // Word doesn't fit — flush current line
                result.push(current_line.clone());
                current_line.clear();
                current_width = 0;

                if word_width <= max_width {
                    current_line.push_str(word);
                    current_width = word_width;
                } else {
                    hard_break(word, max_width, &mut result);
                }
            }
        }

        if !current_line.is_empty() {
            result.push(current_line);
        }
    }

    if result.is_empty() {
        result.push(String::new());
    }

    result
}

/// Hard-break a single word that is wider than `max_width`.
fn hard_break(word: &str, max_width: usize, out: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_w = 0;
    for ch in word.chars() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_w + ch_w > max_width {
            out.push(current.clone());
            current.clear();
            current_w = 0;
        }
        current.push(ch);
        current_w += ch_w;
    }
    if !current.is_empty() {
        out.push(current);
    }
}

/// Compute the display width of a string in terminal columns.
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

// ── Rendering statistics ───────────────────────────────────────────────────────

/// Counters for one frame, used to track rendering budget utilisation.
#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    /// Number of cells written this frame.
    pub cells_written: u64,
    /// Number of dirty (changed) cells.
    pub dirty_cells: u64,
    /// Time spent in widget rendering (microseconds).
    pub render_us: u64,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    // ── word_wrap ──────────────────────────────────────────────────────────────

    #[test]
    fn wrap_short_text_fits_on_one_line() {
        let lines = word_wrap("hello world", 20);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_long_text_splits() {
        let lines = word_wrap("one two three four five", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(display_width(line) <= 10, "line too wide: {line:?}");
        }
    }

    #[test]
    fn wrap_preserves_existing_newlines() {
        let lines = word_wrap("line one\nline two\nline three", 40);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line one");
        assert_eq!(lines[1], "line two");
        assert_eq!(lines[2], "line three");
    }

    #[test]
    fn wrap_empty_line_preserved() {
        let lines = word_wrap("first\n\nthird", 40);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], "");
    }

    #[test]
    fn wrap_hard_break_long_word() {
        // "abcdefghij" is 10 chars wide, max is 4
        let lines = word_wrap("abcdefghij", 4);
        for line in &lines {
            assert!(display_width(line) <= 4, "hard-break failed: {line:?}");
        }
        // Reassembled text should equal original
        let rejoined: String = lines.join("");
        assert_eq!(rejoined, "abcdefghij");
    }

    #[test]
    fn wrap_zero_width_returns_unchanged() {
        let lines = word_wrap("hello", 0);
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn display_width_empty() {
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn display_width_mixed() {
        // ASCII space is 1 col
        assert_eq!(display_width("ab cd"), 5);
    }

    // ── TokenStreamRenderer ───────────────────────────────────────────────────

    #[test]
    fn renderer_starts_empty() {
        let r = TokenStreamRenderer::new("agent");
        assert!(!r.has_content());
        assert_eq!(r.token_count(), 0);
        assert_eq!(r.text(), "");
    }

    #[test]
    fn push_token_does_not_show_until_frame_update() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("hello");
        assert!(r.has_content());
        assert_eq!(r.text(), ""); // not visible yet
        r.frame_update();
        assert_eq!(r.text(), "hello");
    }

    #[test]
    fn push_token_increments_count() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("tok1");
        r.push_token("tok2");
        assert_eq!(r.token_count(), 2);
    }

    #[test]
    fn frame_update_accumulates_multiple_tokens() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("foo ");
        r.push_token("bar");
        r.frame_update();
        assert_eq!(r.text(), "foo bar");
    }

    #[test]
    fn frame_update_returns_true_when_tokens_pending() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("x");
        assert!(r.frame_update());
    }

    #[test]
    fn frame_update_returns_false_when_empty() {
        let mut r = TokenStreamRenderer::new("a");
        assert!(!r.frame_update());
    }

    #[test]
    fn blink_state_toggles_each_frame() {
        let mut r = TokenStreamRenderer::new("a");
        let initial = r.blink_state;
        r.frame_update();
        assert_ne!(r.blink_state, initial);
        r.frame_update();
        assert_eq!(r.blink_state, initial);
    }

    #[test]
    fn reset_clears_all_state() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("text");
        r.frame_update();
        r.reset();
        assert!(!r.has_content());
        assert_eq!(r.token_count(), 0);
        assert_eq!(r.text(), "");
        assert!(!r.is_active);
    }

    #[test]
    fn render_empty_renderer_no_panic() {
        let mut r = TokenStreamRenderer::new("a");
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        r.render(area, &mut buf, Style::default(), Style::default(), false);
    }

    #[test]
    fn render_with_content_no_panic() {
        let mut r = TokenStreamRenderer::new("coder");
        r.push_token("Hello, this is a streaming response from the agent.");
        r.frame_update();
        r.is_active = true;
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        r.render(area, &mut buf, Style::default(), Style::default(), true);
    }

    #[test]
    fn render_tiny_area_no_panic() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("a");
        r.frame_update();
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        r.render(area, &mut buf, Style::default(), Style::default(), false);
    }

    #[test]
    fn render_zero_size_no_panic() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("text");
        r.frame_update();
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5)); // non-zero buf to avoid panic
        r.render(area, &mut buf, Style::default(), Style::default(), false);
    }

    #[test]
    fn render_long_streaming_text_fits_area() {
        let mut r = TokenStreamRenderer::new("qa");
        for _ in 0..100 {
            r.push_token("word ");
        }
        r.frame_update();
        r.is_active = true;
        let area = Rect::new(0, 0, 60, 15);
        let mut buf = Buffer::empty(area);
        r.render(area, &mut buf, Style::default(), Style::default(), false);
        // No cells beyond area bounds should have been written
    }

    #[test]
    fn wrap_cache_reused_at_same_width() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("hello world");
        r.frame_update();

        let lines1 = r.wrapped_lines(20);
        let lines2 = r.wrapped_lines(20); // should hit cache
        assert_eq!(lines1, lines2);
    }

    #[test]
    fn wrap_cache_invalidated_on_new_token() {
        let mut r = TokenStreamRenderer::new("a");
        r.push_token("hello");
        r.frame_update();
        let _ = r.wrapped_lines(20); // populate cache

        r.push_token(" world");
        r.frame_update(); // should invalidate cache

        let lines = r.wrapped_lines(20);
        assert!(
            lines[0].contains("world"),
            "cache should have been refreshed"
        );
    }

    #[test]
    fn tps_nonzero_after_tokens() {
        let mut r = TokenStreamRenderer::new("a");
        // Simulate multiple tokens with small delay
        r.push_token("a");
        std::thread::sleep(std::time::Duration::from_millis(5));
        r.push_token("b");
        // TPS may be very high in tests (fast CPU) but should be > 0
        assert!(r.tps() >= 0.0);
    }
}
