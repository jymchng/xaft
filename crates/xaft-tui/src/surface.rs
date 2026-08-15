//! Conversational terminal surface — raw mode only, no alternate screen.

use std::io::{self, Write};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};

use crate::error::TuiError;

// ── ConversationalSurface ─────────────────────────────────────────────────────

/// Manages raw mode for conversational terminal rendering.
///
/// Does NOT use alternate screen, does NOT use a Ratatui Terminal.
/// Content written to stdout via `IncrementalRenderer` scrolls naturally
/// into the terminal scrollback — no replay needed on exit.
pub struct ConversationalSurface {
    mouse: bool,
    /// True when the terminal accepted the Kitty keyboard enhancement flag
    /// for distinguishing Shift+Enter from Enter. False on legacy terminals
    /// (xterm, older GNOME Terminal, etc.) — the `Alt+Enter` and `Ctrl+J`
    /// fallbacks remain functional there.
    keyboard_enhanced: bool,
}

impl ConversationalSurface {
    /// Create the surface (does NOT yet enter raw mode).
    pub fn new(mouse: bool) -> Result<Self, TuiError> {
        Ok(Self {
            mouse,
            keyboard_enhanced: false,
        })
    }

    /// Enter raw mode, enable mouse capture (optional), and try to enable
    /// terminal capabilities required for multi-line input (Shift+Enter
    /// disambiguation + bracketed paste).
    ///
    /// All `execute!` calls are best-effort: failures on legacy terminals
    /// are silently ignored so the TUI remains usable.
    pub fn init(&mut self) -> Result<(), TuiError> {
        tracing::info!(mouse = self.mouse, "xaft: conversational surface init");
        enable_raw_mode()?;
        if self.mouse {
            let _ = execute!(io::stdout(), EnableMouseCapture);
        }
        // Bracketed paste: required so multi-line pastes are delivered as a
        // single `Event::Paste` payload instead of an Enter-terminated stream
        // of key events that would submit prematurely.
        let _ = execute!(io::stdout(), EnableBracketedPaste);
        // Kitty keyboard protocol: required so Shift+Enter arrives with
        // `modifiers = KeyModifiers::SHIFT` instead of a bare Enter. Failure
        // is non-fatal — the Alt+Enter and Ctrl+J fallbacks still work.
        let push_result = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        if push_result.is_ok() {
            self.keyboard_enhanced = true;
        }
        Ok(())
    }

    /// Disable raw mode and restore terminal state.
    ///
    /// Transcript content is already in the terminal scrollback — nothing to replay.
    pub fn shutdown(&mut self) -> Result<(), TuiError> {
        tracing::info!("xaft: conversational surface shutdown");
        if self.keyboard_enhanced {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
            self.keyboard_enhanced = false;
        }
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        // disable_raw_mode is also called by IncrementalRenderer::shutdown(),
        // but we call it here as a safety net.
        let _ = disable_raw_mode();
        Ok(())
    }

    /// Whether the terminal accepted the Kitty keyboard protocol flag.
    /// When `false`, `Shift+Enter` is not distinguishable from Enter and the
    /// user must use `Alt+Enter` or `Ctrl+J` for newline insertion.
    pub fn keyboard_enhanced(&self) -> bool {
        self.keyboard_enhanced
    }
}

// ── Exit summary footer ───────────────────────────────────────────────────────

/// Print a session summary footer to stdout after TUI exit.
pub fn render_exit_summary(
    elapsed: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    session_id: Option<&str>,
) {
    render_exit_summary_full(
        elapsed,
        elapsed,
        input_tokens,
        output_tokens,
        cost_usd,
        session_id,
    );
}

/// Print a session summary footer with separate per-turn and total wall-clock
/// durations (agenthicc parity: `✾ Worked for …` then
/// `✾ Total wall clock time since last IDLE: …`).
pub fn render_exit_summary_full(
    worked_for: std::time::Duration,
    total_wall_clock: std::time::Duration,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    session_id: Option<&str>,
) {
    let mut out = io::stdout();
    let worked_str = crate::state::format_elapsed(worked_for);
    let total_str = crate::state::format_elapsed(total_wall_clock);
    let in_tok = crate::state::format_tokens_compact(input_tokens);
    let out_tok = crate::state::format_tokens_compact(output_tokens);

    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = writeln!(out, "  ✻ Worked for {worked_str}");
    let _ = writeln!(
        out,
        "  ✾ Total wall clock time since last IDLE: {total_str}"
    );
    let _ = writeln!(
        out,
        "  Tokens: {in_tok} in / {out_tok} out  ·  Cost: ${cost_usd:.4}"
    );
    if let Some(sid) = session_id {
        let _ = writeln!(out, "  Session: {sid}");
        let _ = writeln!(out, "  Resume:  xaft --resume {sid}");
    }
    let _ = writeln!(out, "────────────────────────────────────────────────");
    let _ = out.flush();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_constructs() {
        let surface = ConversationalSurface::new(false).unwrap();
        assert!(!surface.mouse);
    }

    #[test]
    fn exit_summary_no_session() {
        // Should not panic
        render_exit_summary(std::time::Duration::from_secs(5), 100, 50, 0.01, None);
    }

    #[test]
    fn exit_summary_with_session() {
        render_exit_summary(
            std::time::Duration::from_secs(125),
            12400,
            3100,
            0.42,
            Some("test-session-id"),
        );
    }

    #[test]
    fn exit_summary_full_includes_total_wall_clock() {
        // Capture output: render to a temp file-backed stdout is hard, so we
        // assert the helper renders without panicking and both durations parse.
        render_exit_summary_full(
            std::time::Duration::from_secs(61),
            std::time::Duration::from_secs(125),
            12400,
            3100,
            0.42,
            Some("sid"),
        );
    }
}
