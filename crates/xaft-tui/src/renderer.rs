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
//! [prompt:    ❯ user types here ]  ┘ cursor rests here
//! [separator: ──────────────────]
//! [  > file_autocomplete.rs     ]  ← autocomplete dropdown (when @-typing)
//! [    other_file.rs            ]
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

    /// Visual row of the caret within the input area as of the last `draw_prompt_block`.
    ///
    /// Stored directly from the cursor-positioning calculation in `draw_prompt_block`
    /// so that `clear_bottom_block` can invert the move without recomputing from
    /// prompt state — recomputation diverges when lines soft-wrap past `MAX_VISIBLE_ROWS`,
    /// causing a `usize` underflow that corrupts the transcript.
    rendered_caret_vis_row: usize,
    /// Whether the scroll indicator row was drawn in the last `draw_prompt_block`.
    rendered_indicator: bool,
    /// Rows occupied by the mode footer below the prompt block (0 or 1).
    footer_rows: usize,

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
            rendered_caret_vis_row: 0,
            rendered_indicator: false,
            footer_rows: 0,
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
            rendered_caret_vis_row: 0,
            rendered_indicator: false,
            footer_rows: 0,
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
        // clear_bottom_block uses self.current_prompt to find the cursor's
        // current screen position — it must reflect where the cursor IS, not
        // where it's going. Update current_prompt only after the clear.
        self.clear_bottom_block()?;
        self.current_prompt = prompt.clone();
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

    /// Erase the entire bottom block (ephemeral + prompt block + autocomplete).
    ///
    /// Cursor invariant: cursor rests on the input row containing the caret
    /// (which may not be the last input row in a multi-line buffer).
    /// After this call cursor is at col 0 of the first row of the cleared area.
    fn clear_bottom_block(&mut self) -> io::Result<()> {
        // Count rows above the caret row back to the top of the ephemeral
        // region. From caret row going up:
        //   - `rendered_caret_vis_row`   preceding input rows (stored from last draw)
        //   - (1 if indicator shown)     scroll indicator row
        //   - 1                          top border
        //   - ephemeral_count            ephemeral rows above the top border
        //
        // We use the stored `rendered_caret_vis_row` rather than recomputing via
        // `visible_cursor_position` to avoid a correctness hazard: if soft-wrapped
        // lines push the cursor's logical vis_row past `max_in_view`, the subtraction
        // `max_in_view - vis_row` underflows (usize → u16 wrap → ~65535), sending
        // `MoveUp` far into the transcript and then `ClearFromCursorDown` wipes it.
        let rows_up = self.ephemeral_count as u16
            + 1
            + u16::from(self.rendered_indicator)
            + self.rendered_caret_vis_row as u16;
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(rows_up),
            terminal::Clear(ClearType::FromCursorDown),
        )?;
        Ok(())
    }

    /// Print ephemeral lines + autocomplete dropdown + prompt block below the
    /// current cursor.
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
            // Cursor is at the caret's visual row within the input block.
            // Rows up to stream line: caret_vis_row (to first input) + 1 (top border)
            // + 1 (stream line is one row above the top border) + ephemeral rows.
            // Autocomplete rows are BELOW the cursor, so they don't affect this offset.
            // Use stored rendered_caret_vis_row to avoid the same underflow hazard as
            // in clear_bottom_block (see that function's comment for the full rationale).
            let rows_up = self.ephemeral_count as u16
                + 1
                + u16::from(self.rendered_indicator)
                + self.rendered_caret_vis_row as u16
                + 1;
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
        // Lines with rich spans (produced by MarkdownRenderer) use per-span styling.
        if let Some(ref spans) = line.spans {
            return self.write_spans(spans, theme);
        }
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

    /// Render a list of inline `StyledSpan`s to the terminal.
    fn write_spans(
        &mut self,
        spans: &[crate::transcript::StyledSpan],
        theme: &Theme,
    ) -> io::Result<()> {
        use crate::transcript::SpanColor;
        for span in spans {
            let fg = match span.fg {
                Some(SpanColor::Accent) => theme.accent,
                Some(SpanColor::Dim) => theme.dim,
                Some(SpanColor::Success) => theme.success,
                Some(SpanColor::Warning) => theme.warning,
                Some(SpanColor::Error) => theme.error,
                Some(SpanColor::Code) => theme.tool,
                Some(SpanColor::Inherit) | None => theme.fg,
            };
            if span.bold {
                queue!(self.out, SetAttribute(Attribute::Bold))?;
            }
            if span.italic {
                queue!(self.out, SetAttribute(Attribute::Italic))?;
            }
            if span.underline {
                queue!(self.out, SetAttribute(Attribute::Underlined))?;
            }
            if span.strikethrough {
                queue!(self.out, SetAttribute(Attribute::CrossedOut))?;
            }
            if span.dim {
                queue!(self.out, SetAttribute(Attribute::Dim))?;
            }
            queue!(
                self.out,
                SetForegroundColor(fg),
                Print(&span.text),
                SetAttribute(Attribute::Reset)
            )?;
        }
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

    /// Draw the full prompt block: top border + optional scroll indicator +
    /// `max_in_view` input rows + bottom border + autocomplete dropdown.
    /// Updates `last_prompt_height`. Cursor ends on the input row containing
    /// the caret at its visual column.
    fn draw_prompt_block(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        let wrap_width = self.wrap_width();

        // `max_in_view` is the number of post-soft-wrap input rows
        // actually rendered on screen. We walk every line from
        // `scroll_top`, accumulating the soft-wrap row count of each
        // line, capped at `MAX_VISIBLE_ROWS`. The floor of 1 ensures
        // empty buffers and buffers with all-empty lines still render
        // at least one input row.
        let start = prompt.scroll_top.min(prompt.lines.len().saturating_sub(1));
        let mut visual_rows: usize = 0;
        for line in prompt.lines.iter().skip(start) {
            visual_rows += input_bar::wrap_rows(line, wrap_width).max(1);
            if visual_rows >= MAX_VISIBLE_ROWS as usize {
                visual_rows = MAX_VISIBLE_ROWS as usize;
                break;
            }
        }
        let max_in_view = visual_rows.max(1);

        // The scroll indicator eats one row from the visible input region.
        let show_indicator = prompt.hidden_above > 0;

        // Single trigger_rows calculation replaces the former ac_rows + palette_row_count.
        let trigger_rows = prompt
            .active_trigger
            .as_ref()
            .map(|snap| {
                let visible = snap
                    .items
                    .len()
                    .saturating_sub(snap.scroll_top)
                    .min(snap.max_visible);
                let hint_row = if snap.hint.is_some() { 1 } else { 0 };
                // Always show hint line for slash (navigation instructions).
                let nav_hint_row = if snap.trigger_char == '/' && visible > 0 {
                    1
                } else {
                    0
                };
                visible + hint_row.max(nav_hint_row)
            })
            .unwrap_or(0);

        let total = BORDER_ROWS as usize
            + (if show_indicator { 1 } else { 0 })
            + max_in_view
            + trigger_rows;
        self.last_prompt_height = total as u16;

        // Top border
        self.draw_border(theme)?;
        queue!(self.out, Print("\r\n"))?;

        // Optional scroll indicator (occupies the row immediately below the
        // top border when shown).
        if show_indicator {
            let text = if prompt.hidden_above == 1 {
                "▲ 1 more line above".to_string()
            } else {
                format!("▲ {} more lines above", prompt.hidden_above)
            };
            queue!(self.out, SetForegroundColor(theme.dim), Print("  "))?;
            queue!(
                self.out,
                SetForegroundColor(theme.dim),
                Print(&text),
                SetAttribute(Attribute::Reset)
            )?;
            // Pad the rest of the row with spaces so any leftover glyphs from
            // the previous frame are erased visually.
            let used = 2 + display_width(&text);
            let pad = (wrap_width + PREFIX_WIDTH).saturating_sub(used);
            for _ in 0..pad {
                queue!(self.out, Print(" "))?;
            }
            queue!(self.out, Print("\r\n"))?;
        }

        // Input rows. Always render at least one row so the prompt glyph
        // appears even when the buffer is empty.
        let lines: Vec<String> = if prompt.lines.is_empty() {
            vec![String::new()]
        } else {
            prompt.lines.clone()
        };

        // Apply scroll: only show lines starting at scroll_top, up to
        // max_in_view rows total (after soft-wrap).
        let start = prompt.scroll_top.min(lines.len().saturating_sub(1));
        let visible_lines = &lines[start..];

        let mut rendered_rows: usize = 0;
        for (i, line) in visible_lines.iter().enumerate() {
            if rendered_rows >= max_in_view {
                break;
            }
            let fragments = soft_wrap_line(line, wrap_width);
            for (j, frag) in fragments.iter().enumerate() {
                if rendered_rows >= max_in_view {
                    break;
                }
                let is_first = i == 0 && j == 0 && !show_indicator;
                if is_first {
                    // Print mode badge (if any) then the prompt glyph.
                    if let Some(ref badge) = prompt.mode_badge {
                        queue!(self.out, Print(format!("  {} \x1b[1;32m❯\x1b[0m ", badge)))?;
                    } else {
                        queue!(self.out, SetForegroundColor(theme.warning))?;
                        queue!(self.out, Print("❯ "), SetAttribute(Attribute::Reset))?;
                    }
                } else {
                    queue!(self.out, SetForegroundColor(theme.dim))?;
                    queue!(self.out, Print("❯ "), SetAttribute(Attribute::Reset))?;
                }
                queue!(
                    self.out,
                    SetForegroundColor(theme.fg),
                    Print(frag),
                    SetAttribute(Attribute::Reset)
                )?;
                rendered_rows += 1;
                if rendered_rows < max_in_view {
                    queue!(self.out, Print("\r\n"))?;
                }
            }
        }
        // Pad with empty rows so the block is always the same height
        while rendered_rows < max_in_view {
            rendered_rows += 1;
            if rendered_rows < max_in_view {
                queue!(self.out, Print("\r\n"))?;
            }
        }

        queue!(self.out, Print("\r\n"))?;
        // Bottom border
        self.draw_border(theme)?;

        // Unified trigger dropdown (replaces former @-autocomplete + slash palette sections).
        if prompt.active_trigger.is_some() {
            self.draw_trigger_dropdown(prompt, theme)?;
        }

        // Mode footer — always rendered, occupies exactly 1 row.
        let footer = &prompt.mode_footer;
        if !footer.is_empty() {
            queue!(
                self.out,
                Print("\r\n"),
                terminal::Clear(terminal::ClearType::CurrentLine),
                Print(format!("  \x1b[2m{footer}\x1b[0m")),
            )?;
            self.footer_rows = 1;
        } else {
            self.footer_rows = 0;
        }

        // Move cursor back UP to the input row containing the caret.
        //
        // Block layout (top → bottom):
        //   top border, [indicator row], max_in_view input rows, bottom border,
        //   [trigger_rows dropdown rows], [footer_rows mode footer]
        //
        // rows_from_bottom counts input rows from caret to bottom border.
        let (vis_row, vis_col) = visible_cursor_position(prompt, wrap_width);

        // Clamp vis_row to max_in_view - 1 to prevent usize underflow when
        // soft-wrapped lines push vis_row past max_in_view.
        let vis_row = vis_row.min(max_in_view.saturating_sub(1));

        // Store for clear_bottom_block — it must invert this exact cursor move.
        self.rendered_caret_vis_row = vis_row;
        self.rendered_indicator = show_indicator;

        let rows_from_bottom = (max_in_view - vis_row) as u16;
        let col = (vis_col + PREFIX_WIDTH) as u16;
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            cursor::MoveUp(rows_from_bottom + trigger_rows as u16 + self.footer_rows as u16),
            cursor::MoveToColumn(col),
        )?;
        Ok(())
    }

    /// Render the active trigger dropdown below the bottom border.
    ///
    /// Supports two visual modes keyed by `trigger_char`:
    ///
    /// - `'@'` (file-mention mode): `" + <candidate>"` rows, no trigger prefix.
    /// - `'/'` (command mode): `" ▶/command  description  <args>"` rows with
    ///   a 12-char padded trigger column, a description column, and an optional
    ///   args-hint column.
    /// - All other trigger chars: `" ▶ <display>"` rows (generic mode).
    ///
    /// A hint line is appended after the last visible row when `snap.hint` is `Some`.
    fn draw_trigger_dropdown(&mut self, prompt: &PromptState, theme: &Theme) -> io::Result<()> {
        let Some(ref snap) = prompt.active_trigger else {
            return Ok(());
        };
        let visible_count = snap
            .items
            .len()
            .saturating_sub(snap.scroll_top)
            .min(snap.max_visible);

        for (offset, item) in snap
            .items
            .iter()
            .skip(snap.scroll_top)
            .take(visible_count)
            .enumerate()
        {
            let abs_index = snap.scroll_top + offset;
            let is_selected = abs_index == snap.selected;

            let row_text = self.format_trigger_row(snap.trigger_char, item);
            let max_len = self.term_cols.saturating_sub(1) as usize;
            let display: String = row_text.chars().take(max_len).collect();

            queue!(self.out, Print("\r\n"))?;
            if is_selected {
                queue!(
                    self.out,
                    SetForegroundColor(theme.warning),
                    SetAttribute(Attribute::Bold),
                    Print(&display),
                    SetAttribute(Attribute::Reset),
                )?;
            } else {
                queue!(
                    self.out,
                    SetForegroundColor(theme.fg),
                    Print(&display),
                    SetAttribute(Attribute::Reset),
                )?;
            }
        }

        // Hint line: item-specific hint first, then fall back to navigation help.
        let hint_text = if let Some(ref h) = snap.hint {
            h.as_str()
        } else {
            match snap.trigger_char {
                '/' => "  ↑/↓ navigate · Tab complete · Enter execute · Esc dismiss",
                '@' => "", // no hint line for @-mention (matches current behaviour)
                _ => "  ↑/↓ navigate · Tab/Enter select · Esc dismiss",
            }
        };
        if !hint_text.is_empty() && visible_count > 0 {
            queue!(
                self.out,
                Print("\r\n"),
                SetForegroundColor(theme.dim),
                Print(hint_text),
                SetAttribute(Attribute::Reset),
            )?;
        }

        Ok(())
    }

    /// Format a single row string for the given trigger char and item.
    ///
    /// The slash command format preserves the pre-refactor SlashPalette rendering:
    /// indicator + padded trigger + description + args.
    fn format_trigger_row(&self, trigger_char: char, item: &crate::trigger::MatchItem) -> String {
        match trigger_char {
            '@' => format!(" + {}", item.display),
            '/' => {
                // Command mode: "▶/command       description  args"
                // Matches the pre-refactor SlashPalette rendering.
                let inner_w = (self.term_cols as usize).saturating_sub(1);
                let indicator = "▶";
                let trigger = format!("/{:<12}", item.display);
                let fixed_w = 1 /* indicator */ + 13 /* trigger */ + 1 /* space */;
                let avail = inner_w.saturating_sub(fixed_w);

                // Build description from hint field of the item (stored in hint by SlashCommandTriggerHandler).
                // The hint for slash is args_hint; the description is not separately tracked in MatchItem.
                // We look up description separately here via the display name.
                let description = crate::slash::parser::COMMAND_TABLE
                    .iter()
                    .find(|(k, _)| k == &item.display.as_str())
                    .map(|(_, m)| m.description)
                    .unwrap_or("");
                let args_str = item
                    .hint
                    .as_deref()
                    .map(|a| format!("  {a}"))
                    .unwrap_or_default();
                let args_w = display_width(&args_str);
                let desc_avail = avail.saturating_sub(args_w);
                let desc: String = {
                    let mut s = String::new();
                    let mut w = 0usize;
                    let needs_ellipsis = display_width(description) > desc_avail;
                    for c in description.chars() {
                        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                        let budget = if needs_ellipsis {
                            desc_avail.saturating_sub(1)
                        } else {
                            desc_avail
                        };
                        if w + cw > budget {
                            if needs_ellipsis {
                                s.push('…');
                            }
                            break;
                        }
                        s.push(c);
                        w += cw;
                    }
                    s
                };
                format!(" {indicator}{trigger} {desc}{args_str}")
            }
            _ => format!(" ▶ {}", item.display),
        }
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

/// Compute the `(row, col)` of the prompt's cursor **within the currently
/// visible input region**. `row` is 0-indexed from the first on-screen input
/// row (i.e. after the top border and any scroll indicator, before the
/// bottom border). `col` is the visual column within that row.
///
/// When the buffer has not been scrolled (`scroll_top == 0`), this matches
/// the older whole-buffer view: the first input row is row 0.
///
/// When the buffer has been scrolled, the iteration starts at
/// `scroll_top` and the returned row is relative to the first visible
/// line. If the logical cursor is on a line that has scrolled out of
/// view, the result is clamped to the last row of the visible region.
fn visible_cursor_position(prompt: &PromptState, wrap_width: usize) -> (usize, usize) {
    if prompt.lines.is_empty() || wrap_width == 0 {
        return (0, 0);
    }
    let cursor_row = prompt.cursor.row.min(prompt.lines.len().saturating_sub(1));
    let start = prompt.scroll_top.min(prompt.lines.len().saturating_sub(1));
    let mut vis_row = 0usize;
    let mut last_vis_row = 0usize;
    for (i, line) in prompt.lines.iter().enumerate().skip(start) {
        let line_vis_rows = input_bar::wrap_rows(line, wrap_width).max(1);
        if i == cursor_row {
            let col = snap_to_boundary(line, prompt.cursor.col);
            let w = display_width(&line[..col]);
            let local_row = (w / wrap_width).min(line_vis_rows.saturating_sub(1));
            let local_col = w % wrap_width;
            return (vis_row + local_row, local_col);
        }
        vis_row += line_vis_rows;
        last_vis_row = vis_row.saturating_sub(1);
    }
    // Cursor on a line outside the visible window — clamp to the last
    // position in the visible region.
    (last_vis_row, 0)
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
        // Code blocks: use the tool/code colour. F27 may override this later.
        LineKind::CodeBlock => ContentStyle::new().with(theme.tool),
        // Bash mode output lines.
        LineKind::BashStdout => ContentStyle::new().with(theme.fg),
        LineKind::BashStderr => ContentStyle::new().with(theme.warning),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
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
    fn multi_line_continuation_prefix_is_chevron() {
        // All rows (first and continuation) use "❯ " prefix.
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["first".into(), "second".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 6 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.init_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("❯ first"), "first row prefix: {output:?}");
        assert!(output.contains("❯ second"), "second row prefix: {output:?}");
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
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        assert_eq!(r.prompt_block_height(), 7);
        // Resize to a narrow terminal.
        r.handle_resize(20, 30, &theme()).unwrap();
        // Visible rows may differ after resize (soft-wrap changes).
        // The important thing is no panic and the block still renders.
        assert!(r.prompt_block_height() >= 2);
    }

    // ── Scroll indicator (PRD 29a §8.4, acceptance #7) ──────────────────────

    /// 12-line buffer should display only the bottom 8 rows plus a
    /// `▲ 4 more lines above` indicator.
    #[test]
    fn scroll_indicator_appears_when_buffer_exceeds_max() {
        let mut r = make_renderer();
        // Build a 12-line buffer.
        let lines: Vec<String> = (1..=12).map(|i| format!("line {i}")).collect();
        let prompt = PromptState {
            lines: lines.clone(),
            cursor: crate::input_bar::Cursor {
                row: lines.len() - 1,
                col: lines.last().unwrap().len(),
            },
            scroll_top: 4, // 4 scrolled off the top
            agent_active: false,
            is_empty: false,
            hidden_above: 4,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("▲ 4 more lines above"),
            "scroll indicator missing: {output:?}"
        );
    }

    /// Singular "1 more line" form.
    #[test]
    fn scroll_indicator_singular() {
        let mut r = make_renderer();
        let lines: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let prompt = PromptState {
            lines,
            cursor: crate::input_bar::Cursor { row: 9, col: 2 },
            scroll_top: 1,
            agent_active: false,
            is_empty: false,
            hidden_above: 1,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("▲ 1 more line above"),
            "singular indicator missing: {output:?}"
        );
    }

    /// When nothing is scrolled, no indicator is shown.
    #[test]
    fn no_scroll_indicator_when_buffer_fits() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["a".into(), "b".into(), "c".into()],
            cursor: crate::input_bar::Cursor { row: 2, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            !output.contains("more lines above"),
            "indicator should not appear: {output:?}"
        );
    }

    // ── Snapshot tests (PRD 29b §12.3) ──────────────────────────────────────

    /// Helper: count occurrences of the prompt glyph `❯` in the output.
    fn count_prompt_glyphs(output: &str) -> usize {
        output.matches('❯').count()
    }

    /// Helper: count occurrences of the continuation prefix `❯` excluding the
    /// first-row prompt glyph. Since all rows now use `❯`, this counts total
    /// `❯` occurrences minus 1 (the first-row prompt).
    fn count_continuation_glyphs(output: &str) -> usize {
        output.matches('❯').count().saturating_sub(1)
    }

    /// Snapshot: empty buffer renders with one prompt glyph, no continuation
    /// glyphs, and 3 total rows (2 borders + 1 input).
    #[test]
    fn snapshot_empty_buffer() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        let output = r.out.plain_text();
        assert_eq!(
            count_prompt_glyphs(&output),
            1,
            "exactly one prompt glyph: {output:?}"
        );
        assert_eq!(count_continuation_glyphs(&output), 0);
        assert_eq!(r.prompt_block_height(), 3);
    }

    /// Snapshot: single-line buffer renders with one prompt glyph.
    #[test]
    fn snapshot_single_line() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["hello".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 5 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert_eq!(count_prompt_glyphs(&output), 1);
        assert!(output.contains("hello"));
    }

    /// Snapshot: two-line buffer renders with one prompt glyph + one
    /// continuation glyph.
    #[test]
    fn snapshot_two_lines() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["hello".into(), "world".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 5 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        // Both rows use "❯ " prefix now, so 2 total prompt glyphs.
        assert_eq!(count_prompt_glyphs(&output), 2);
        assert_eq!(count_continuation_glyphs(&output), 1);
        assert!(output.contains("hello"));
        assert!(output.contains("world"));
    }

    /// Snapshot: 12-line buffer renders with 8 input rows + 1 indicator row
    /// + 2 borders = 11 rows.
    #[test]
    fn snapshot_twelve_line_buffer() {
        let mut r = make_renderer();
        let lines: Vec<String> = (1..=12).map(|i| format!("line {i}")).collect();
        let prompt = PromptState {
            lines,
            cursor: crate::input_bar::Cursor { row: 11, col: 6 },
            scroll_top: 4,
            agent_active: false,
            is_empty: false,
            hidden_above: 4,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        // Block: 8 input rows + 1 indicator row + 2 borders = 11 rows.
        assert_eq!(r.prompt_block_height(), 11);
        let output = r.out.plain_text();
        // The bottom 4 lines should be visible. Use a per-line check to
        // avoid "line 1" being a substring of "line 11" / "line 12".
        let rendered_lines: Vec<&str> = output
            .split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .map(|l| l.trim_start_matches(['❯', ' ']))
            .filter(|l| l.starts_with("line "))
            .collect();
        assert!(
            rendered_lines.contains(&"line 9"),
            "line 9 missing: {rendered_lines:?}"
        );
        assert!(
            rendered_lines.contains(&"line 10"),
            "line 10 missing: {rendered_lines:?}"
        );
        assert!(
            rendered_lines.contains(&"line 11"),
            "line 11 missing: {rendered_lines:?}"
        );
        assert!(
            rendered_lines.contains(&"line 12"),
            "line 12 missing: {rendered_lines:?}"
        );
        // The top 4 should NOT be visible (scrolled off).
        assert!(
            !rendered_lines.contains(&"line 1"),
            "line 1 should be scrolled: {rendered_lines:?}"
        );
        assert!(
            !rendered_lines.contains(&"line 2"),
            "line 2 should be scrolled: {rendered_lines:?}"
        );
        assert!(
            !rendered_lines.contains(&"line 3"),
            "line 3 should be scrolled: {rendered_lines:?}"
        );
        assert!(
            !rendered_lines.contains(&"line 4"),
            "line 4 should be scrolled: {rendered_lines:?}"
        );
    }

    /// Snapshot: cursor at start, middle, and end of line 1 of 3 — the visual
    /// position must update correctly each time.
    #[test]
    fn snapshot_cursor_position_variants() {
        let mut r = make_renderer();
        // Cursor at start
        let p_start = PromptState {
            lines: vec!["abc".into(), "def".into(), "ghi".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 0 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&p_start, &theme()).unwrap();
        // Cursor at middle
        let p_mid = PromptState {
            lines: vec!["abc".into(), "def".into(), "ghi".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 1 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&p_mid, &theme()).unwrap();
        // Cursor at end of line 1
        let p_end = PromptState {
            lines: vec!["abc".into(), "def".into(), "ghi".into()],
            cursor: crate::input_bar::Cursor { row: 1, col: 3 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&p_end, &theme()).unwrap();
        // All three updates should succeed without panic.
        let output = r.out.plain_text();
        assert!(output.contains("abc"));
        assert!(output.contains("def"));
        assert!(output.contains("ghi"));
    }

    /// Snapshot: soft-wrapped line at 40-col terminal.
    #[test]
    fn snapshot_soft_wrapped_line() {
        let mut r = make_renderer();
        // 80-char line, 40-col terminal → 2+ visible rows
        let long = "a".repeat(80);
        let prompt = PromptState {
            lines: vec![long.clone()],
            cursor: crate::input_bar::Cursor { row: 0, col: 80 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        // wrap_width = 38, 80 / 38 = 3 rows; total block = 3 + 2 = 5
        assert!(r.prompt_block_height() >= 4);
    }

    // ── Unicode edge cases (PRD 29b acceptance #8) ──────────────────────────

    /// Emoji (4-byte UTF-8) cursor navigation is char-aware.
    #[test]
    fn unicode_emoji_round_trip() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["hello 😀 world".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 13 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("hello 😀 world"),
            "emoji must render: {output:?}"
        );
    }

    /// CJK wide chars (3 bytes each) — wrap-width accounts for double columns.
    #[test]
    fn unicode_cjk_wide_chars() {
        let mut r = make_renderer();
        // Each 日 character is 2 display columns
        let prompt = PromptState {
            lines: vec!["日本語のテスト".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 18 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("日本語のテスト"),
            "CJK must render: {output:?}"
        );
    }

    /// Combining marks (`e` + U+0301 = é) must not split the base+mark pair.
    #[test]
    fn unicode_combining_marks() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["café résumé".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 13 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(
            output.contains("café résumé"),
            "combining marks must render: {output:?}"
        );
    }

    /// Flag emoji (regional indicator pairs, 2 codepoints × 4 bytes = 8 bytes).
    #[test]
    fn unicode_flag_emoji() {
        let mut r = make_renderer();
        let prompt = PromptState {
            lines: vec!["flag: 🇺🇸".into()],
            cursor: crate::input_bar::Cursor { row: 0, col: 10 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        let output = r.out.plain_text();
        assert!(output.contains("🇺🇸"), "flag emoji must render: {output:?}");
    }

    // ── rendered_caret_vis_row / rendered_indicator storage ──────────────────
    // These tests guard the fix for the "eating transcript" regression where
    // clear_bottom_block used to recompute caret_vis_row from the prompt state
    // and could get a wrong (underflowing) result when soft-wrapped lines pushed
    // the cursor beyond max_in_view.

    fn make_prompt(lines: Vec<&str>, cursor_row: usize, cursor_col: usize) -> PromptState {
        PromptState {
            lines: lines.into_iter().map(String::from).collect(),
            cursor: crate::input_bar::Cursor {
                row: cursor_row,
                col: cursor_col,
            },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        }
    }

    #[test]
    fn rendered_caret_vis_row_stored_for_single_line() {
        let mut r = make_renderer();
        let p = make_prompt(vec!["hi"], 0, 2);
        r.init_prompt(&p, &theme()).unwrap();
        // Cursor is at vis_row 0 (only input row).
        assert_eq!(r.rendered_caret_vis_row, 0);
    }

    #[test]
    fn rendered_caret_vis_row_stored_for_multiline_last_row() {
        let mut r = make_renderer();
        // 4 logical lines, cursor on the last one.
        let p = make_prompt(vec!["a", "b", "c", "d"], 3, 1);
        r.init_prompt(&p, &theme()).unwrap();
        // Cursor should be at vis_row 3.
        assert_eq!(r.rendered_caret_vis_row, 3);
    }

    #[test]
    fn rendered_caret_vis_row_stored_for_cursor_on_first_row() {
        let mut r = make_renderer();
        let p = make_prompt(vec!["first", "second", "third"], 0, 5);
        r.init_prompt(&p, &theme()).unwrap();
        assert_eq!(r.rendered_caret_vis_row, 0);
    }

    #[test]
    fn rendered_indicator_false_when_no_scroll() {
        let mut r = make_renderer();
        let p = make_prompt(vec!["a", "b"], 1, 0);
        r.init_prompt(&p, &theme()).unwrap();
        assert!(!r.rendered_indicator);
    }

    #[test]
    fn rendered_indicator_true_when_lines_scrolled() {
        let mut r = make_renderer();
        let lines: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let prompt = PromptState {
            lines: lines.clone(),
            cursor: crate::input_bar::Cursor { row: 9, col: 2 },
            scroll_top: 2,
            agent_active: false,
            is_empty: false,
            hidden_above: 2,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&prompt, &theme()).unwrap();
        assert!(
            r.rendered_indicator,
            "indicator must be stored when hidden_above > 0"
        );
    }

    #[test]
    fn rendered_caret_vis_row_updates_on_prompt_change() {
        let mut r = make_renderer();
        // Start with cursor on row 2 of 3.
        r.init_prompt(&make_prompt(vec!["a", "b", "c"], 2, 1), &theme())
            .unwrap();
        assert_eq!(r.rendered_caret_vis_row, 2);
        // Now shrink to 1-line (cursor on row 0).
        r.update_prompt(&make_prompt(vec!["abc"], 0, 3), &theme())
            .unwrap();
        assert_eq!(r.rendered_caret_vis_row, 0);
    }

    // ── Transcript-eating regression tests ────────────────────────────────────
    // Each test commits at least one transcript line, then exercises a prompt
    // height change that previously corrupted the display.

    #[test]
    fn multiline_backspace_does_not_eat_transcript() {
        // Simulates: commit a line, type 4-line input, press backspace (shrink to 3).
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("ANCHOR_LINE"), &theme())
            .unwrap();

        // 4-line prompt.
        r.update_prompt(&make_prompt(vec!["", "", "", ""], 3, 0), &theme())
            .unwrap();
        // 3-line prompt (backspace at start of last line merges).
        r.update_prompt(&make_prompt(vec!["", "", ""], 2, 0), &theme())
            .unwrap();

        assert!(
            r.out.plain_text().contains("ANCHOR_LINE"),
            "transcript eaten after multiline backspace"
        );
    }

    #[test]
    fn growing_then_shrinking_prompt_preserves_multiple_transcript_lines() {
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("LINE_ONE"), &theme())
            .unwrap();
        r.commit_line(&agent_text_line("LINE_TWO"), &theme())
            .unwrap();
        r.commit_line(&agent_text_line("LINE_THREE"), &theme())
            .unwrap();

        // Grow to 5 lines.
        r.update_prompt(&make_prompt(vec!["a", "b", "c", "d", "e"], 4, 1), &theme())
            .unwrap();
        // Shrink back to 2.
        r.update_prompt(&make_prompt(vec!["a", "b"], 1, 1), &theme())
            .unwrap();
        // Shrink to 1.
        r.update_prompt(&make_prompt(vec!["ab"], 0, 2), &theme())
            .unwrap();

        let text = r.out.plain_text();
        assert!(text.contains("LINE_ONE"), "LINE_ONE eaten");
        assert!(text.contains("LINE_TWO"), "LINE_TWO eaten");
        assert!(text.contains("LINE_THREE"), "LINE_THREE eaten");
    }

    #[test]
    fn ephemeral_toggle_does_not_eat_transcript() {
        // Simulates agent starting / stopping while multi-line input is active.
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("IMPORTANT_OUTPUT"), &theme())
            .unwrap();

        // Add a multi-line prompt.
        r.update_prompt(&make_prompt(vec!["first", "second"], 1, 6), &theme())
            .unwrap();

        // Ephemeral spinner appears.
        let eph = EphemeralState {
            spinner_line: "✣ thinking…".into(),
            status_line: None,
        };
        r.set_ephemeral(&eph, &theme()).unwrap();

        // Ephemeral cleared.
        r.clear_ephemeral(&theme()).unwrap();

        // Prompt shrinks (user typed backspace).
        r.update_prompt(&make_prompt(vec!["first"], 0, 5), &theme())
            .unwrap();

        assert!(
            r.out.plain_text().contains("IMPORTANT_OUTPUT"),
            "transcript eaten during ephemeral toggle"
        );
    }

    #[test]
    fn soft_wrap_cursor_beyond_max_in_view_does_not_eat_transcript() {
        // Regression: when cursor vis_row >= max_in_view (soft-wrap overflow),
        // rendered_caret_vis_row must be clamped to prevent MoveUp underflow.
        // Here we use a terminal width of 10 so short lines wrap quickly.
        let capture = TestCapture::new(10, 24);
        let mut r = IncrementalRenderer::with_writer(capture);
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.commit_line(&agent_text_line("TRANSCRIPT_SAFE"), &theme())
            .unwrap();

        // A 40-char line on a 10-col terminal wraps to 4+ visual rows.
        // With MAX_VISIBLE_ROWS=8 and 3 such lines = 12+ visual rows > cap.
        let long_line = "A".repeat(40);
        let lines = vec![long_line.as_str(), long_line.as_str(), long_line.as_str()];
        let p = PromptState {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            // cursor at end of last long line, which would be visual row >8
            cursor: crate::input_bar::Cursor { row: 2, col: 40 },
            scroll_top: 0,
            agent_active: false,
            is_empty: false,
            hidden_above: 0,
            hidden_below: 0,
            active_trigger: None,
            menu_active: false,
            mode_badge: None,
            mode_footer: String::new(),
        };
        r.update_prompt(&p, &theme()).unwrap();
        // Verify caret vis_row was clamped (must be < max_in_view = 8).
        assert!(
            r.rendered_caret_vis_row < 8,
            "caret vis_row must be clamped: got {}",
            r.rendered_caret_vis_row
        );

        // Now shrink the prompt — this must NOT eat the transcript.
        r.update_prompt(&make_prompt(vec!["x"], 0, 1), &theme())
            .unwrap();
        assert!(
            r.out.plain_text().contains("TRANSCRIPT_SAFE"),
            "transcript eaten when cursor was beyond max_in_view"
        );
    }

    #[test]
    fn stream_line_and_multiline_prompt_shrink_preserves_transcript() {
        // Variant: transcript committed via stream, not commit_line.
        let mut r = make_renderer();
        r.init_prompt(&PromptState::default(), &theme()).unwrap();
        r.update_stream("streamed output", &theme()).unwrap();
        r.flush_stream(&theme()).unwrap();

        r.update_prompt(&make_prompt(vec!["a", "b", "c", "d"], 3, 0), &theme())
            .unwrap();
        r.update_prompt(&make_prompt(vec!["a", "b"], 1, 0), &theme())
            .unwrap();

        assert!(
            r.out.plain_text().contains("streamed output"),
            "streamed content eaten during prompt shrink"
        );
    }
}
