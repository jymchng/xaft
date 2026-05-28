//! Terminal surface abstraction — manages screen buffer lifecycle.
//!
//! Two implementations:
//! - [`PrimaryScreenSurface`]: renders to the primary terminal buffer. Content
//!   stays in scrollback naturally. No alternate screen.
//! - [`AlternateScreenSurface`]: uses the alternate screen buffer. On shutdown,
//!   replays the final frame with ANSI escapes so content persists in scrollback.

use std::io::{self, Write};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    style::{Attribute, Color as CColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, buffer::Buffer};

use crate::error::TuiError;

/// Trait abstracting over primary-screen and alternate-screen rendering.
pub trait TerminalSurface {
    /// Initialize the terminal (raw mode, alternate screen if applicable).
    fn init(&mut self) -> Result<(), TuiError>;
    /// Shut down the terminal. If `preserve` is true, replay final content.
    fn shutdown(&mut self, preserve: bool) -> Result<(), TuiError>;
    /// Whether this surface uses the alternate screen.
    fn is_alternate(&self) -> bool;
    /// Capture the current buffer for replay.
    fn capture_buffer(&mut self) -> Option<Buffer>;
}

// ── PrimaryScreenSurface ──────────────────────────────────────────────────────

/// Renders directly to the primary terminal buffer.
/// Content stays in scrollback naturally — no replay needed.
pub struct PrimaryScreenSurface {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse: bool,
}

impl PrimaryScreenSurface {
    /// Create a new primary-screen surface.
    pub fn new(mouse: bool) -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal, mouse })
    }

    /// Access the underlying terminal.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }
}

impl TerminalSurface for PrimaryScreenSurface {
    fn init(&mut self) -> Result<(), TuiError> {
        tracing::info!("xaft: primary screen surface init");
        // Raw mode already enabled in new(). No alternate screen to enter.
        Ok(())
    }

    fn shutdown(&mut self, _preserve: bool) -> Result<(), TuiError> {
        tracing::info!("xaft: primary screen surface shutdown");
        disable_raw_mode()?;
        if self.mouse {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn is_alternate(&self) -> bool {
        false
    }

    fn capture_buffer(&mut self) -> Option<Buffer> {
        None // Primary screen content stays naturally
    }
}

// ── AlternateScreenSurface ────────────────────────────────────────────────────

/// Uses the alternate screen buffer. On shutdown, replays the final frame
/// with ANSI escapes to preserve content in the primary scrollback.
pub struct AlternateScreenSurface {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse: bool,
    /// Snapshot of the last rendered frame, captured inside `draw()`.
    last_frame: Option<Buffer>,
}

impl AlternateScreenSurface {
    /// Create a new alternate-screen surface.
    pub fn new(mouse: bool) -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if mouse {
            execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        } else {
            execute!(stdout, EnterAlternateScreen)?;
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            mouse,
            last_frame: None,
        })
    }

    /// Access the underlying terminal.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }

    /// Render a frame and snapshot the buffer for later replay.
    /// Call this instead of `terminal.draw()` directly.
    pub fn draw_and_snapshot<F>(&mut self, f: F) -> Result<(), TuiError>
    where
        F: FnOnce(&mut Frame),
    {
        let buf = self.terminal.draw(f)?.buffer.clone();
        self.last_frame = Some(buf);
        Ok(())
    }
}

impl TerminalSurface for AlternateScreenSurface {
    fn init(&mut self) -> Result<(), TuiError> {
        tracing::info!("xaft: alternate screen surface init");
        // Already initialized in new().
        Ok(())
    }

    fn shutdown(&mut self, preserve: bool) -> Result<(), TuiError> {
        tracing::info!(preserve, "xaft: alternate screen surface shutdown");

        // Use the snapshot captured by draw_and_snapshot().
        let final_frame = if preserve {
            self.last_frame.take()
        } else {
            None
        };

        disable_raw_mode()?;

        if self.mouse {
            execute!(
                self.terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
        } else {
            execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        }

        self.terminal.show_cursor()?;

        // Replay final frame to primary scrollback.
        if let Some(buf) = final_frame {
            replay_buffer_ansi(&buf);
        }

        Ok(())
    }

    fn is_alternate(&self) -> bool {
        true
    }

    fn capture_buffer(&mut self) -> Option<Buffer> {
        self.last_frame.clone()
    }
}

// ── ANSI-aware frame replay ──────────────────────────────────────────────────

/// Convert ratatui `Color` to crossterm `Color`.
fn ratatui_to_crossterm(c: ratatui::style::Color) -> CColor {
    match c {
        ratatui::style::Color::Reset => CColor::Reset,
        ratatui::style::Color::Black => CColor::Black,
        ratatui::style::Color::Red => CColor::Red,
        ratatui::style::Color::Green => CColor::Green,
        ratatui::style::Color::Yellow => CColor::Yellow,
        ratatui::style::Color::Blue => CColor::Blue,
        ratatui::style::Color::Magenta => CColor::Magenta,
        ratatui::style::Color::Cyan => CColor::Cyan,
        ratatui::style::Color::White => CColor::White,
        ratatui::style::Color::Gray => CColor::Grey,
        ratatui::style::Color::DarkGray => CColor::DarkGrey,
        ratatui::style::Color::LightRed => CColor::DarkRed,
        ratatui::style::Color::LightGreen => CColor::DarkGreen,
        ratatui::style::Color::LightYellow => CColor::DarkYellow,
        ratatui::style::Color::LightBlue => CColor::DarkBlue,
        ratatui::style::Color::LightMagenta => CColor::DarkMagenta,
        ratatui::style::Color::LightCyan => CColor::DarkCyan,
        ratatui::style::Color::Indexed(i) => CColor::AnsiValue(i),
        ratatui::style::Color::Rgb(r, g, b) => CColor::Rgb { r, g, b },
        _ => CColor::Reset,
    }
}

/// Replay a ratatui `Buffer` to stdout with ANSI escape codes for colors
/// and attributes, so the content appears styled in the terminal scrollback.
pub fn replay_buffer_ansi(buf: &Buffer) {
    let mut out = io::stdout();
    let area = buf.area;
    let mut last_fg: Option<CColor> = None;
    let mut last_bg: Option<CColor> = None;

    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let fg = ratatui_to_crossterm(cell.fg);
            let bg = ratatui_to_crossterm(cell.bg);

            if Some(fg) != last_fg {
                line.push_str(&format!("{}", SetForegroundColor(fg)));
                last_fg = Some(fg);
            }
            if Some(bg) != last_bg {
                line.push_str(&format!("{}", SetBackgroundColor(bg)));
                last_bg = Some(bg);
            }

            let mods = cell.modifier;
            if mods.contains(ratatui::style::Modifier::BOLD) {
                line.push_str(&format!("{}", SetAttribute(Attribute::Bold)));
            }
            if mods.contains(ratatui::style::Modifier::DIM) {
                line.push_str(&format!("{}", SetAttribute(Attribute::Dim)));
            }
            if mods.contains(ratatui::style::Modifier::ITALIC) {
                line.push_str(&format!("{}", SetAttribute(Attribute::Italic)));
            }
            if mods.contains(ratatui::style::Modifier::UNDERLINED) {
                line.push_str(&format!("{}", SetAttribute(Attribute::Underlined)));
            }

            line.push_str(cell.symbol());

            if !mods.is_empty() {
                line.push_str(&format!(
                    "{}{}",
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(fg)
                ));
                last_fg = Some(fg);
            }
        }
        line.push_str(&format!(
            "{}{}",
            SetAttribute(Attribute::Reset),
            SetForegroundColor(CColor::Reset)
        ));
        last_fg = None;
        last_bg = None;

        let trimmed = line.trim_end();
        let _ = writeln!(out, "{}", trimmed);
    }
    let _ = out.flush();
}

/// Render a session summary footer to stdout after TUI exit.
pub fn render_exit_summary(
    elapsed: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    session_id: Option<&str>,
) {
    let mut out = io::stdout();
    let elapsed_str = crate::state::format_elapsed(elapsed);
    let in_tok = crate::state::format_tokens_compact(input_tokens);
    let out_tok = crate::state::format_tokens_compact(output_tokens);

    let _ = writeln!(out);
    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = writeln!(out, "  ✻ Worked for {elapsed_str}");
    let _ = writeln!(
        out,
        "  Tokens: {in_tok} in / {out_tok} out  ·  Cost: ${cost_usd:.4}"
    );
    if let Some(sid) = session_id {
        let _ = writeln!(out, "  Session: {sid}");
    }
    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = out.flush();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Cell;
    use ratatui::layout::Rect;

    #[test]
    fn replay_buffer_handles_empty() {
        let buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        // Should not panic
        replay_buffer_ansi(&buf);
    }

    #[test]
    fn replay_buffer_handles_content() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        buf[(0, 0)].set_symbol("H");
        buf[(1, 0)].set_symbol("i");
        // Should not panic
        replay_buffer_ansi(&buf);
    }

    #[test]
    fn exit_summary_formats() {
        // Should not panic
        render_exit_summary(
            std::time::Duration::from_secs(125),
            12400,
            3100,
            0.42,
            Some("test-session-id"),
        );
    }

    #[test]
    fn exit_summary_no_session() {
        render_exit_summary(std::time::Duration::from_secs(5), 100, 50, 0.01, None);
    }
}
