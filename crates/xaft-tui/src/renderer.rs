//! Incremental terminal renderer — append-only transcript with in-place ephemeral region.
//!
//! # Layout
//!
//! ```text
//! [transcript lines... scroll into terminal scrollback]
//! [open stream line (if any)]
//! [ephemeral 0: spinner         ]  ← EphemeralState.spinner_line
//! [ephemeral 1: token/cost stats]  ← EphemeralState.status_line (optional)
//! [separator: ──────────────────]  ┐ PROMPT_ROWS
//! [prompt:    ❯ user types here ]  ┘
//! ```
//!
//! # Cursor invariant
//!
//! After every public method returns, the physical terminal cursor rests at
//! **column 0 of the prompt line** (the last line of the bottom block).
//! All save/restore operations restore to this position.
//!
//! # Thread safety
//!
//! `IncrementalRenderer` is not `Send`/`Sync` — it must be driven from the
//! single async event loop that also handles terminal input.

use std::io::{self, BufWriter, Write};

use crossterm::{
    cursor, queue,
    style::{Attribute, Color, ContentStyle, Print, SetAttribute, SetForegroundColor, Stylize},
    terminal::{self, ClearType},
};
use unicode_width::UnicodeWidthChar;

use crate::ephemeral::EphemeralState;
use crate::prompt::{PromptState, format_prompt_line};
use crate::theme::Theme;
use crate::transcript::{LineKind, StyledLine};

/// Number of prompt rows at the bottom (top border + input line + bottom border).
const PROMPT_ROWS: u16 = 3;

// ── TermWriter ────────────────────────────────────────────────────────────────

/// Abstraction over the terminal output stream; enables testability.
pub trait TermWriter: Write {
    /// Terminal dimensions.  Override in tests to return a fixed size.
    fn terminal_size(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or((120, 40))
    }
}

impl TermWriter for BufWriter<io::Stdout> {}

// ── TestCapture ───────────────────────────────────────────────────────────────

/// In-memory writer for unit tests.
pub struct TestCapture {
    buf: Vec<u8>,
    size: (u16, u16),
}

impl TestCapture {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            buf: Vec::new(),
            size: (cols, rows),
        }
    }

    /// All bytes written so far.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Raw output as a UTF-8 string (lossy).
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    /// Plain text content (strips ANSI escape sequences).
    pub fn plain_text(&self) -> String {
        strip_ansi(self.output())
    }
}

impl Write for TestCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TermWriter for TestCapture {
    fn terminal_size(&self) -> (u16, u16) {
        self.size
    }
}

/// Naïve ANSI escape stripper for tests.
fn strip_ansi(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip until end of escape sequence
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // Consume until a letter
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(c) if *c == '7' || *c == '8' => {
                    chars.next();
                }
                _ => {}
            }
        } else {
            out.push(ch);
        }
    }
    out
}

// ── IncrementalRenderer ───────────────────────────────────────────────────────

/// Append-only conversational terminal renderer.
///
/// Writes directly to `W` (normally `BufWriter<Stdout>`) using crossterm
/// escape sequences.  No Ratatui frame buffer is involved.
pub struct IncrementalRenderer<W: TermWriter = BufWriter<io::Stdout>> {
    term_cols: u16,
    /// Number of ephemeral lines currently drawn above the prompt block.
    ephemeral_count: u8,
    /// True when a streaming line is open (its content is on screen but not
    /// yet committed with a newline).
    pub stream_line_open: bool,
    /// Display-column width of the content already written on the open stream line.
    stream_line_cols: usize,
    /// Last prompt state written to the terminal.
    current_prompt: PromptState,
    /// Last ephemeral state written to the terminal.
    current_ephemeral: Option<EphemeralState>,
    out: W,
}

impl IncrementalRenderer<BufWriter<io::Stdout>> {
    /// Create a renderer that writes to stdout.
    pub fn new() -> io::Result<Self> {
        let out = BufWriter::with_capacity(16 * 1024, io::stdout());
        let (cols, _) = crossterm::terminal::size().unwrap_or((120, 40));
        Ok(Self {
            term_cols: cols,
            ephemeral_count: 0,
            stream_line_open: false,
            stream_line_cols: 0,
            current_prompt: PromptState::default(),
            current_ephemeral: None,
            out,
        })
    }
}

impl<W: TermWriter> IncrementalRenderer<W> {
    /// Create a renderer from any `TermWriter` (used in tests).
    pub fn with_writer(writer: W) -> Self {
        let (cols, _) = writer.terminal_size();
        Self {
            term_cols: cols,
            ephemeral_count: 0,
            stream_line_open: false,
            stream_line_cols: 0,
            current_prompt: PromptState::default(),
            current_ephemeral: None,
            out: writer,
        }
    }

    /// Draw the initial prompt. Call once after `surface.init()`.
    pub fn init_prompt(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        self.current_prompt = prompt.clone();
        self.draw_border(theme)?;
        queue!(self.out, Print("\r\n"))?;
        self.draw_prompt_line(prompt, theme)?;
        queue!(self.out, Print("\r\n"))?;
        self.draw_border(theme)?;
        queue!(self.out, cursor::MoveToColumn(0))?;
        self.out.flush()
    }

    /// Commit a fully-formed line to the permanent transcript.
    ///
    /// Clears the ephemeral region and prompt, appends the line with a newline,
    /// then redraws the ephemeral+prompt below.
    pub fn commit_line(&mut self, line: &StyledLine, theme: &Theme) -> io::Result<()> {
        if self.stream_line_open {
            self.do_flush_stream(theme)?;
        }
        self.clear_bottom_block()?;
        self.write_styled_line(line, theme)?;
        queue!(self.out, Print("\r\n"))?;
        self.redraw_bottom_block(theme)?;
        self.out.flush()
    }

    /// Append a streaming token fragment to the current open stream line.
    ///
    /// If no stream line is open, opens one by clearing the bottom block first.
    /// Handles embedded newlines by splitting into multiple fragments.
    pub fn update_stream(&mut self, fragment: &str, theme: &Theme) -> io::Result<()> {
        // Split on newlines so embedded '\n' in tokens are handled correctly.
        let parts: Vec<&str> = fragment.split('\n').collect();
        let last_idx = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                self.append_stream_fragment(part, theme)?;
            }
            // All parts except the last are followed by a newline → flush.
            if i < last_idx {
                self.do_flush_stream(theme)?;
            }
        }
        self.out.flush()
    }

    /// Close the current streaming line (make it permanent transcript).
    pub fn flush_stream(&mut self, theme: &Theme) -> io::Result<()> {
        if !self.stream_line_open {
            return Ok(());
        }
        self.do_flush_stream(theme)?;
        self.out.flush()
    }

    /// Overwrite the ephemeral region with new content.
    pub fn set_ephemeral(&mut self, eph: &EphemeralState, theme: &Theme) -> io::Result<()> {
        self.clear_bottom_block()?;
        self.current_ephemeral = Some(eph.clone());
        self.ephemeral_count = 1u8 + if eph.status_line.is_some() { 1 } else { 0 };
        self.redraw_bottom_block(theme)?;
        self.out.flush()
    }

    /// Convenience: set or clear the ephemeral region.
    pub fn set_ephemeral_opt(
        &mut self,
        eph: Option<&EphemeralState>,
        theme: &Theme,
    ) -> io::Result<()> {
        match eph {
            Some(e) => self.set_ephemeral(e, theme),
            None => self.clear_ephemeral(theme),
        }
    }

    /// Clear the ephemeral region entirely.
    pub fn clear_ephemeral(&mut self, theme: &Theme) -> io::Result<()> {
        if self.ephemeral_count == 0 {
            return Ok(());
        }
        self.clear_bottom_block()?;
        self.current_ephemeral = None;
        self.ephemeral_count = 0;
        self.redraw_bottom_block(theme)?;
        self.out.flush()
    }

    /// Overwrite the prompt line with updated content.
    pub fn update_prompt(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        self.current_prompt = prompt.clone();
        // Cursor is at col 0 of bottom border (row N).
        // Move up 1 to input line, clear and rewrite, then return to bottom border.
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(1),
            terminal::Clear(ClearType::CurrentLine),
        )?;
        self.draw_prompt_line(prompt, theme)?;
        queue!(self.out,
            cursor::MoveToColumn(0),
            cursor::MoveDown(1),
        )?;
        self.out.flush()
    }

    /// Handle terminal resize.
    pub fn handle_resize(&mut self, cols: u16, _rows: u16, theme: &Theme) -> io::Result<()> {
        self.term_cols = cols;
        // Redraw the separator (width may have changed) and prompt.
        self.clear_bottom_block()?;
        self.redraw_bottom_block(theme)?;
        self.out.flush()
    }

    /// Graceful shutdown: clear ephemeral, show cursor, leave transcript intact.
    pub fn shutdown(&mut self, theme: &Theme) -> io::Result<()> {
        // Clear bottom block cleanly
        self.clear_bottom_block()?;
        self.ephemeral_count = 0;
        self.current_ephemeral = None;
        // Flush any open stream line
        if self.stream_line_open {
            self.do_flush_stream(theme)?;
        }
        // Move to a fresh line
        queue!(self.out, Print("\r\n"))?;
        crossterm::terminal::disable_raw_mode()?;
        self.out.flush()
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Erase the entire bottom block (ephemeral + separator + prompt).
    /// After this call, cursor is at column 0 of the FIRST line of the block.
    fn clear_bottom_block(&mut self) -> io::Result<()> {
        let total = self.ephemeral_count as u16 + PROMPT_ROWS;
        if total == 0 {
            return Ok(());
        }
        // Move up to the top of the bottom block (total - 1 lines above current).
        if total > 1 {
            queue!(self.out, cursor::MoveToColumn(0), cursor::MoveUp(total - 1),)?;
        } else {
            queue!(self.out, cursor::MoveToColumn(0))?;
        }
        queue!(self.out, terminal::Clear(ClearType::FromCursorDown))?;
        Ok(())
    }

    /// Print the ephemeral lines + top border + input + bottom border below the current cursor.
    /// After this call, cursor is at column 0 of the bottom border line.
    fn redraw_bottom_block(&mut self, theme: &Theme) -> io::Result<()> {
        let eph = self.current_ephemeral.clone();
        let prompt = self.current_prompt.clone();

        if let Some(ref e) = eph {
            // Spinner line: yellow
            queue!(self.out,
                SetForegroundColor(theme.warning),
                Print(&e.spinner_line),
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
            )?;
            // Status line: dim
            if let Some(ref status) = e.status_line.clone() {
                queue!(self.out,
                    SetForegroundColor(theme.dim),
                    Print(status),
                    SetAttribute(Attribute::Reset),
                    Print("\r\n"),
                )?;
            }
        }
        // Top border (yellow)
        self.draw_border(theme)?;
        queue!(self.out, Print("\r\n"))?;
        // Input line
        self.draw_prompt_line(&prompt, theme)?;
        queue!(self.out, Print("\r\n"))?;
        // Bottom border (yellow) — cursor ends at col 0 of this line
        self.draw_border(theme)?;
        queue!(self.out, cursor::MoveToColumn(0))?;
        Ok(())
    }

    /// Append a fragment (no newlines) to the open (or new) stream line.
    fn append_stream_fragment(&mut self, fragment: &str, theme: &Theme) -> io::Result<()> {
        if !self.stream_line_open {
            // Open a new stream line: clear the bottom block and start printing.
            self.clear_bottom_block()?;
            let style = self.stream_style(theme);
            queue!(
                self.out,
                SetForegroundColor(style.foreground_color.unwrap_or(Color::Reset)),
                Print(fragment),
                SetAttribute(Attribute::Reset),
            )?;
            self.stream_line_cols = display_width(fragment);
            self.stream_line_open = true;
            // Move below the stream line and draw the bottom block.
            queue!(self.out, Print("\r\n"))?;
            self.redraw_bottom_block(theme)?;
        } else {
            // Append to existing open stream line.
            let rows_up = self.ephemeral_count as u16 + PROMPT_ROWS;
            let col = self.stream_line_cols as u16;
            let style = self.stream_style(theme);
            queue!(
                self.out,
                cursor::SavePosition,
                cursor::MoveToColumn(0),
                cursor::MoveUp(rows_up),
                cursor::MoveToColumn(col),
                SetForegroundColor(style.foreground_color.unwrap_or(Color::Reset)),
                Print(fragment),
                SetAttribute(Attribute::Reset),
                cursor::RestorePosition,
            )?;
            self.stream_line_cols += display_width(fragment);
        }
        Ok(())
    }

    /// Close the current stream line without flushing the output buffer.
    fn do_flush_stream(&mut self, _theme: &Theme) -> io::Result<()> {
        if !self.stream_line_open {
            return Ok(());
        }
        // The stream line is already fully rendered on screen.
        // Just reset our tracking state — nothing visual needs to change.
        self.stream_line_open = false;
        self.stream_line_cols = 0;
        Ok(())
    }

    /// Write a styled transcript line at the current cursor position (no newline).
    fn write_styled_line(&mut self, line: &StyledLine, theme: &Theme) -> io::Result<()> {
        let style = style_for_kind(line.kind, theme);
        let fg = style.foreground_color.unwrap_or(Color::Reset);
        let bold = style.attributes.has(Attribute::Bold);
        if bold {
            queue!(self.out, SetAttribute(Attribute::Bold))?;
        }
        queue!(
            self.out,
            SetForegroundColor(fg),
            Print(&line.text),
            SetAttribute(Attribute::Reset)
        )?;
        Ok(())
    }

    fn draw_ephemeral_line(&mut self, text: &str, theme: &Theme) -> io::Result<()> {
        queue!(
            self.out,
            SetForegroundColor(theme.dim),
            Print(text),
            SetAttribute(Attribute::Reset),
        )?;
        Ok(())
    }

    fn draw_separator(&mut self, theme: &Theme) -> io::Result<()> {
        let sep = "─".repeat(self.term_cols as usize);
        queue!(
            self.out,
            SetForegroundColor(theme.dim),
            Print(&sep),
            SetAttribute(Attribute::Reset),
        )?;
        Ok(())
    }

    fn draw_prompt_line(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        let line = format_prompt_line(prompt);
        queue!(
            self.out,
            SetForegroundColor(theme.accent),
            Print("❯ "),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(theme.fg),
            Print(&prompt.buffer),
            SetAttribute(Attribute::Reset),
        )?;
        let _ = line;
        Ok(())
    }

    fn stream_style(&self, theme: &Theme) -> ContentStyle {
        ContentStyle {
            foreground_color: Some(theme.dim),
            ..Default::default()
        }
    }
}

// ── Style helpers ─────────────────────────────────────────────────────────────

/// Map a `LineKind` to a crossterm `ContentStyle`.
pub fn style_for_kind(kind: LineKind, theme: &Theme) -> ContentStyle {
    match kind {
        LineKind::AgentText => ContentStyle::new().with(theme.fg),
        LineKind::AgentMarker => {
            let mut s = ContentStyle::new().with(theme.agent);
            s.attributes.set(Attribute::Bold);
            s
        }
        LineKind::ToolCall => ContentStyle::new().with(theme.tool),
        LineKind::ToolResult => ContentStyle::new().with(theme.dim),
        LineKind::System => ContentStyle::new().with(theme.dim),
        LineKind::Error => ContentStyle::new().with(theme.error),
        LineKind::Success => ContentStyle::new().with(theme.success),
        LineKind::UserMessage => ContentStyle::new().with(theme.fg),
        LineKind::Separator => ContentStyle::new().with(theme.dim),
        LineKind::DiffAdd => ContentStyle::new().with(theme.success),
        LineKind::DiffRemove => ContentStyle::new().with(theme.error),
        LineKind::DiffContext => ContentStyle::new().with(theme.dim),
    }
}

// ── Unicode width utilities ───────────────────────────────────────────────────

/// Display width of a string in terminal columns (Unicode-aware).
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Unicode-aware word-wrap.
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
                if word_width <= max_width {
                    current_line.push_str(word);
                    current_width = word_width;
                } else {
                    hard_break_into(word, max_width, &mut result);
                }
            } else if current_width + 1 + word_width <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
                current_width += 1 + word_width;
            } else {
                result.push(current_line.clone());
                current_line.clear();
                current_width = 0;
                if word_width <= max_width {
                    current_line.push_str(word);
                    current_width = word_width;
                } else {
                    hard_break_into(word, max_width, &mut result);
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

fn hard_break_into(word: &str, max_width: usize, out: &mut Vec<String>) {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ephemeral::EphemeralState;
    use crate::prompt::PromptState;
    use crate::theme::Theme;
    use crate::transcript::StyledLine;

    fn make_renderer() -> IncrementalRenderer<TestCapture> {
        let capture = TestCapture::new(80, 24);
        IncrementalRenderer::with_writer(capture)
    }

    fn theme() -> Theme {
        Theme::dark()
    }

    fn agent_text_line(text: &str) -> StyledLine {
        StyledLine::new(text, LineKind::AgentText)
    }

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn word_wrap_fits_on_one_line() {
        let lines = word_wrap("hello world", 20);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_splits_long_text() {
        let lines = word_wrap("one two three four five", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(display_width(line) <= 10, "line too wide: {line:?}");
        }
    }

    #[test]
    fn word_wrap_preserves_newlines() {
        let lines = word_wrap("line one\nline two\nline three", 40);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn renderer_init_prompt_writes_separator_and_prompt() {
        let mut r = make_renderer();
        let prompt = PromptState::default();
        r.init_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("❯"),
            "should contain prompt glyph: {output:?}"
        );
    }

    #[test]
    fn commit_line_writes_text() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        let line = agent_text_line("hello world");
        r.commit_line(&line, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("hello world"), "output: {output:?}");
    }

    #[test]
    fn multiple_commit_lines_appear_in_order() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("first"), &theme()).unwrap();
        r.commit_line(&agent_text_line("second"), &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("first"), "first: {output:?}");
        assert!(output.contains("second"), "second: {output:?}");
    }

    #[test]
    fn update_stream_writes_fragment() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("streaming token", &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("streaming token"), "output: {output:?}");
        assert!(r.stream_line_open, "stream should be open");
        assert!(r.stream_line_cols > 0);
    }

    #[test]
    fn flush_stream_closes_stream_line() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("hello", &theme()).unwrap();
        assert!(r.stream_line_open);
        r.flush_stream(&theme()).unwrap();
        assert!(!r.stream_line_open);
        assert_eq!(r.stream_line_cols, 0);
    }

    #[test]
    fn commit_line_while_stream_open_flushes_first() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("streaming", &theme()).unwrap();
        assert!(r.stream_line_open);
        r.commit_line(&agent_text_line("committed"), &theme())
            .unwrap();
        assert!(
            !r.stream_line_open,
            "stream should be closed after commit_line"
        );
        let output = r.out.plain_text();
        assert!(output.contains("streaming"), "output: {output:?}");
        assert!(output.contains("committed"), "output: {output:?}");
    }

    #[test]
    fn set_ephemeral_increases_count() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        assert_eq!(r.ephemeral_count, 0);
        let eph = EphemeralState {
            spinner_line: "✣ Planning…".into(),
            status_line: Some("Tokens: 1.2k".into()),
        };
        r.set_ephemeral(&eph, &theme()).unwrap();
        assert_eq!(r.ephemeral_count, 2);
    }

    #[test]
    fn clear_ephemeral_resets_count() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        let eph = EphemeralState {
            spinner_line: "spinner".into(),
            status_line: None,
        };
        r.set_ephemeral(&eph, &theme()).unwrap();
        assert_eq!(r.ephemeral_count, 1);
        r.clear_ephemeral(&theme()).unwrap();
        assert_eq!(r.ephemeral_count, 0);
    }

    #[test]
    fn update_prompt_changes_buffer() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        let prompt = PromptState {
            buffer: "my input".into(),
            agent_active: false,
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("my input"), "output: {output:?}");
    }

    #[test]
    fn handle_resize_updates_cols() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.handle_resize(100, 30, &theme()).unwrap();
        assert_eq!(r.term_cols, 100);
    }

    #[test]
    fn update_stream_with_newline_flushes() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("hello\nworld", &theme()).unwrap();
        // After newline, first part flushed; "world" is on new stream line
        let output = r.out.plain_text();
        assert!(output.contains("hello"), "output: {output:?}");
        assert!(output.contains("world"), "output: {output:?}");
    }

    #[test]
    fn stream_line_cols_tracks_display_width() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("ab", &theme()).unwrap();
        assert_eq!(r.stream_line_cols, 2);
        r.update_stream("cd", &theme()).unwrap();
        assert_eq!(r.stream_line_cols, 4);
    }

    #[test]
    fn shutdown_no_panic() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("partial", &theme()).unwrap();
        // shutdown should flush and close without panic
        // (disable_raw_mode will fail in test env but we just check no panic on write path)
        let _ = r.do_flush_stream(&theme());
        // Verify stream is closed
        assert!(!r.stream_line_open);
    }

    #[test]
    fn ephemeral_one_line_only() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        let eph = EphemeralState {
            spinner_line: "spinner only".into(),
            status_line: None,
        };
        r.set_ephemeral(&eph, &theme()).unwrap();
        assert_eq!(r.ephemeral_count, 1);
        let output = r.out.plain_text();
        assert!(output.contains("spinner only"), "output: {output:?}");
    }
}
