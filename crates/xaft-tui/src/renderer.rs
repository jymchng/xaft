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
//! **end of the prompt text on the input line** — visually the blinking caret
//! appears right after the last typed character.  The bottom border is one row
//! below the cursor and is never the resting position.
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
use crate::input_bar::{self, MAX_VISIBLE_ROWS, PREFIX_WIDTH};
use crate::prompt::PromptState;
use crate::theme::Theme;
use crate::transcript::{LineKind, StyledLine};

/// Minimum terminal width the renderer will plan around (soft-wrap floor).
const MIN_TERM_COLS: u16 = 10;
/// Rows reserved for prompt chrome: top border + bottom border.
const BORDER_ROWS: u16 = 2;

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

    /// Snapshot of the output (lossy UTF-8).
    pub fn snapshot(&self) -> String {
        self.output()
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
    /// Total rows the prompt block currently occupies (borders + visible input rows).
    last_prompt_height: u16,
    /// Last ephemeral state written to the terminal.
    current_ephemeral: Option<EphemeralState>,
    /// The underlying terminal writer. Exposed for snapshot testing from
    /// integration tests; production code should not write to it directly.
    pub out: W,
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
            last_prompt_height: BORDER_ROWS,
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
            last_prompt_height: BORDER_ROWS,
            current_ephemeral: None,
            out: writer,
        }
    }

    /// Draw the initial prompt. Call once after `surface.init()`.
    /// Cursor ends at the visual position of the prompt's logical cursor.
    pub fn init_prompt(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        self.current_prompt = prompt.clone();
        self.draw_prompt_block(prompt, theme)?;
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

    /// Overwrite the prompt block with updated content.
    /// Erases the previous prompt block and redraws it at the new height.
    pub fn update_prompt(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        self.current_prompt = prompt.clone();
        self.clear_bottom_block()?;
        self.draw_prompt_block(prompt, theme)?;
        self.out.flush()
    }

    /// Handle terminal resize.
    pub fn handle_resize(&mut self, cols: u16, _rows: u16, theme: &Theme) -> io::Result<()> {
        self.term_cols = cols;
        // Redraw the prompt block (width may have changed; visible_rows
        // may differ from before).
        self.clear_bottom_block()?;
        self.redraw_bottom_block(theme)?;
        self.out.flush()
    }

    /// Clear the bottom block, disable raw mode, and position cursor for post-TUI output.
    ///
    /// Call this BEFORE printing any exit summary. Raw mode is disabled here so
    /// subsequent writeln! calls produce correct line endings.
    pub fn clear_for_exit(&mut self, theme: &Theme) -> io::Result<()> {
        self.clear_bottom_block()?;
        self.ephemeral_count = 0;
        self.current_ephemeral = None;
        if self.stream_line_open {
            self.do_flush_stream(theme)?;
        }
        self.out.flush()?;
        // Disable raw mode NOW so subsequent writeln! uses normal \n semantics.
        let _ = crossterm::terminal::disable_raw_mode();
        Ok(())
    }

    /// Idempotent shutdown — raw mode already disabled by `clear_for_exit`.
    pub fn shutdown(&mut self, _theme: &Theme) -> io::Result<()> {
        // disable_raw_mode is safe to call twice; ensure it's done.
        let _ = crossterm::terminal::disable_raw_mode();
        self.out.flush()
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Erase the entire bottom block (ephemeral + prompt block).
    ///
    /// Cursor invariant: cursor rests on the input row containing the caret
    /// (which may not be the last input row in a multi-line buffer).
    /// After this call cursor is at col 0 of the first row of the cleared area.
    fn clear_bottom_block(&mut self) -> io::Result<()> {
        // Count rows above the caret row back to the top of the ephemeral
        // region. From caret row going up:
        //   - `caret_vis_row`           preceding input rows
        //   - 1                          top border
        //   - ephemeral_count            ephemeral rows above the top border
        let caret_vis_row = visual_cursor_position(&self.current_prompt, self.wrap_width()).0;
        let rows_up = self.ephemeral_count as u16 + 1 + caret_vis_row as u16;
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(rows_up),
            terminal::Clear(ClearType::FromCursorDown),
        )?;
        Ok(())
    }

    /// Print ephemeral lines + prompt block below the current cursor.
    ///
    /// Cursor ends at the visual position of the prompt's logical cursor on
    /// the last visible input row (invariant).
    fn redraw_bottom_block(&mut self, theme: &Theme) -> io::Result<()> {
        let eph = self.current_ephemeral.clone();
        let prompt = self.current_prompt.clone();

        if let Some(ref e) = eph {
            // Spinner line: yellow
            queue!(
                self.out,
                SetForegroundColor(theme.warning),
                Print(&e.spinner_line),
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
            )?;
            // Status line: dim
            if let Some(ref status) = e.status_line.clone() {
                queue!(
                    self.out,
                    SetForegroundColor(theme.dim),
                    Print(status),
                    SetAttribute(Attribute::Reset),
                    Print("\r\n"),
                )?;
            }
        }
        self.draw_prompt_block(&prompt, theme)?;
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
            // Cursor is on the INPUT LINE. Stream line is:
            //   ephemeral_count + 2 rows above (1 for top border + 1 for stream above that).
            let rows_up = self.ephemeral_count as u16 + 2;
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

    /// Yellow horizontal border line spanning the terminal width.
    fn draw_border(&mut self, theme: &Theme) -> io::Result<()> {
        let border = "─".repeat(self.term_cols as usize);
        queue!(
            self.out,
            SetForegroundColor(theme.warning),
            Print(&border),
            SetAttribute(Attribute::Reset),
        )?;
        Ok(())
    }

    /// Draw the full prompt block: top border + N input rows + bottom border.
    /// Updates `last_prompt_height`. Cursor ends on the input row containing
    /// the caret at its visual column.
    fn draw_prompt_block(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        let wrap_width = self.wrap_width();
        let visible = visible_prompt_rows(prompt, wrap_width);
        let total = visible + BORDER_ROWS as usize;
        self.last_prompt_height = total as u16;

        // Top border
        self.draw_border(theme)?;
        queue!(self.out, Print("\r\n"))?;

        // Input rows. Always render at least one row so the prompt glyph
        // appears even when the buffer is empty.
        let lines = if prompt.lines.is_empty() {
            vec![String::new()]
        } else {
            prompt.lines.clone()
        };

        let mut rendered_rows: usize = 0;
        for (i, line) in lines.iter().enumerate() {
            let fragments = soft_wrap_line(line, wrap_width);
            for (j, frag) in fragments.iter().enumerate() {
                let is_first = i == 0 && j == 0;
                let prefix = if is_first { "❯ " } else { "│ " };
                if is_first {
                    queue!(self.out, SetForegroundColor(theme.warning))?;
                    queue!(self.out, Print(prefix), SetAttribute(Attribute::Reset))?;
                } else {
                    queue!(self.out, SetForegroundColor(theme.dim))?;
                    queue!(self.out, Print(prefix), SetAttribute(Attribute::Reset))?;
                }
                queue!(
                    self.out,
                    SetForegroundColor(theme.fg),
                    Print(frag),
                    SetAttribute(Attribute::Reset)
                )?;
                rendered_rows += 1;
                if rendered_rows < visible {
                    queue!(self.out, Print("\r\n"))?;
                }
            }
        }
        // Pad with empty rows so the block is always the same height
        while rendered_rows < visible {
            rendered_rows += 1;
            if rendered_rows < visible {
                queue!(self.out, Print("\r\n"))?;
            }
        }

        queue!(self.out, Print("\r\n"))?;
        // Bottom border
        self.draw_border(theme)?;

        // The cursor now sits on the bottom border row. Move it back UP to the
        // input row containing the caret, then to the correct visual column.
        let (vis_row_in_block, vis_col) = visual_cursor_position(prompt, wrap_width);
        // visible=1, vis=0 → 1 row up (caret on the only input row)
        // visible=3, vis=2 → 1 row up (caret on the last input row)
        // visible=3, vis=0 → 3 rows up (caret on the first input row)
        let rows_from_bottom = (visible - vis_row_in_block) as u16;
        let col = (vis_col + PREFIX_WIDTH) as u16;
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(rows_from_bottom),
            cursor::MoveToColumn(col),
        )?;
        Ok(())
    }

    /// Compute the soft-wrap width (term_cols - PREFIX_WIDTH), with a floor.
    fn wrap_width(&self) -> usize {
        (self.term_cols.max(MIN_TERM_COLS) as usize)
            .saturating_sub(PREFIX_WIDTH)
            .max(1)
    }

    /// Total rows the most recently rendered prompt block occupies
    /// (borders + visible input rows).
    pub fn prompt_block_height(&self) -> u16 {
        self.last_prompt_height
    }

    fn stream_style(&self, theme: &Theme) -> ContentStyle {
        ContentStyle {
            foreground_color: Some(theme.dim),
            ..Default::default()
        }
    }
}

/// Column position of cursor after rendering the prompt line: `"❯ " + buffer`.
#[allow(dead_code)]
fn prompt_end_col(prompt: &PromptState) -> u16 {
    // "❯ " is 2 display columns, then the buffer text.
    (PREFIX_WIDTH as u16) + display_width(&prompt.lines.join("\n")) as u16
}

// ── Multi-line prompt rendering helpers ───────────────────────────────────────

/// Number of post-soft-wrap input rows the prompt block will display,
/// clamped to `MAX_VISIBLE_ROWS`.
fn visible_prompt_rows(prompt: &PromptState, wrap_width: usize) -> usize {
    if prompt.lines.is_empty() {
        return 1;
    }
    let mut total = 0usize;
    for line in &prompt.lines {
        total += input_bar::wrap_rows(line, wrap_width).max(1);
    }
    total.clamp(1, MAX_VISIBLE_ROWS as usize)
}

/// Compute the (visual_row_within_input_block, visual_col) of the prompt's
/// cursor. `visual_row` is the offset from the top of the input rows (i.e.
/// the first input row is row 0, not the top border).
fn visual_cursor_position(prompt: &PromptState, wrap_width: usize) -> (usize, usize) {
    if prompt.lines.is_empty() || wrap_width == 0 {
        return (0, 0);
    }
    let cursor_row = prompt.cursor.row.min(prompt.lines.len().saturating_sub(1));
    let mut vis_row = 0usize;
    for (i, line) in prompt.lines.iter().enumerate() {
        if i == cursor_row {
            let col = snap_to_boundary(line, prompt.cursor.col);
            let w = display_width(&line[..col]);
            let line_vis_rows = input_bar::wrap_rows(line, wrap_width).max(1);
            let local_row = (w / wrap_width).min(line_vis_rows.saturating_sub(1));
            let local_col = w % wrap_width;
            return (vis_row + local_row, local_col);
        }
        vis_row += input_bar::wrap_rows(line, wrap_width).max(1);
    }
    (vis_row.saturating_sub(1), 0)
}

fn snap_to_boundary(s: &str, col: usize) -> usize {
    let mut c = col.min(s.len());
    while c > 0 && !s.is_char_boundary(c) {
        c -= 1;
    }
    c
}

/// Soft-wrap a single line into visual row fragments of `width` display
/// columns. Never splits a multi-byte character; empty inputs yield one empty
/// fragment so the prompt always shows at least one input row.
fn soft_wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for ch in line.chars() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_w + ch_w > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push(ch);
        current_w += ch_w;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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
        LineKind::UserMessage => ContentStyle::new().with(theme.warning),
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
            lines: vec!["my input".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 8 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
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

    // ── Multi-line prompt block (F21) ─────────────────────────────────────────

    /// Helper: count the number of terminal lines written by inspecting the
    /// raw ANSI output. We count `\r\n` sequences (the boundary between rows).
    fn count_output_rows(output: &str) -> usize {
        // Each rendered row in the block is terminated by a `\r\n` (except the
        // very last row of the block, which may end with the cursor positioning
        // sequence). The bottom border is followed by MoveToColumn + MoveUp
        // sequences, not `\r\n`, so we count `\r\n` occurrences.
        output.matches("\r\n").count()
    }

    #[test]
    fn single_line_prompt_block_has_three_rows() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["hi".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 2 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.init_prompt(&prompt, &theme()).unwrap();
        // Block: top border + 1 input row + bottom border = 3 rows
        // → 2 `\r\n` sequences (between top/input and input/bottom)
        assert_eq!(count_output_rows(r.out.output().as_str()), 2);
        // last_prompt_height should be 3
        assert_eq!(r.prompt_block_height(), 3);
    }

    #[test]
    fn multi_line_prompt_block_grows() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["a".into(), "b".into(), "c".into()],
            cursor: crate::input_bar::Cursor { row: 2, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.init_prompt(&prompt, &theme()).unwrap();
        // 3 input rows + 2 borders = 5 rows → 4 `\r\n`
        assert_eq!(count_output_rows(r.out.output().as_str()), 4);
        assert_eq!(r.prompt_block_height(), 5);
    }

    #[test]
    fn update_prompt_resizes_block() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        // Empty default prompt: 1 visible input row + 2 borders = 3
        assert_eq!(r.prompt_block_height(), 3);
        // Grow the buffer to 3 lines.
        let prompt = PromptState {
            lines: vec!["x".into(), "y".into(), "z".into()],
            cursor: crate::input_bar::Cursor { row: 2, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        // Now the block has 5 rows (2 borders + 3 input).
        assert_eq!(r.prompt_block_height(), 5);
    }

    #[test]
    fn update_prompt_does_not_eat_above_line() {
        // Regression for F21: after `update_prompt` on a multi-line buffer,
        // the prior transcript line must still be present in the output.
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        // Commit a transcript line.
        r.commit_line(&agent_text_line("TRANSCRIPT_LINE"), &theme())
            .unwrap();
        // Now type a multi-line input.
        let prompt = PromptState {
            lines: vec!["first".into(), "second".into(), "third".into()],
            cursor: crate::input_bar::Cursor { row: 2, col: 5 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("TRANSCRIPT_LINE"),
            "transcript line above prompt must remain: {output:?}"
        );
        assert!(output.contains("first"), "input line 0 missing: {output:?}");
        assert!(
            output.contains("second"),
            "input line 1 missing: {output:?}"
        );
        assert!(output.contains("third"), "input line 2 missing: {output:?}");
    }

    #[test]
    fn commit_line_after_multiline_prompt_preserves_buffer() {
        // After typing a multi-line buffer, pressing Enter (which calls
        // commit_line with the user message, then update_prompt), the new
        // empty buffer must be rendered without overwriting the transcript.
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("earlier"), &theme())
            .unwrap();
        // Type multi-line input.
        let prompt = PromptState {
            lines: vec!["a".into(), "b".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        // Submit (commit user message + redraw empty prompt).
        let user_line = crate::transcript::StyledLine::new(
            "❯ a\nb".to_string(),
            crate::transcript::LineKind::UserMessage,
        );
        r.commit_line(&user_line, &theme()).unwrap();
        // Now the prompt is empty; redraw with empty state.
        r.update_prompt(&PromptState::default(), &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("earlier"), "earlier transcript: {output:?}");
        assert!(output.contains("❯ a"), "submitted msg: {output:?}");
        assert!(output.contains("b"), "second line: {output:?}");
    }

    #[test]
    fn multi_line_continuation_prefix_is_pipe() {
        // First row uses "❯ " prefix, subsequent rows use "│ " prefix.
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["first".into(), "second".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 6 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.init_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("❯ first"), "first row prefix: {output:?}");
        assert!(output.contains("│ second"), "second row prefix: {output:?}");
    }

    #[test]
    fn resize_changes_block_layout() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        // Make a 5-line buffer.
        let prompt = PromptState {
            lines: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
            cursor: crate::input_bar::Cursor { row: 4, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        assert_eq!(r.prompt_block_height(), 7);
        // Resize to a narrow terminal.
        r.handle_resize(20, 30, &theme()).unwrap();
        // Visible rows may differ after resize (soft-wrap changes).
        // The important thing is no panic and the block still renders.
        assert!(r.prompt_block_height() >= 2);
    }
}
