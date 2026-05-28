//! Conversational terminal surface — raw mode only, no alternate screen.

use std::io::{self, Write};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
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
}

impl ConversationalSurface {
    /// Create the surface (does NOT yet enter raw mode).
    pub fn new(mouse: bool) -> Result<Self, TuiError> {
        Ok(Self { mouse })
    }

    /// Enter raw mode and optionally enable mouse capture.
    pub fn init(&mut self) -> Result<(), TuiError> {
        tracing::info!(mouse = self.mouse, "xaft: conversational surface init");
        enable_raw_mode()?;
        if self.mouse {
            execute!(io::stdout(), EnableMouseCapture)?;
        }
        Ok(())
    }

    /// Disable raw mode and restore terminal state.
    ///
    /// Transcript content is already in the terminal scrollback — nothing to replay.
    pub fn shutdown(&mut self) -> Result<(), TuiError> {
        tracing::info!("xaft: conversational surface shutdown");
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        // disable_raw_mode is also called by IncrementalRenderer::shutdown(),
        // but we call it here as a safety net.
        let _ = disable_raw_mode();
        Ok(())
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
    let mut out = io::stdout();
    let elapsed_str = crate::state::format_elapsed(elapsed);
    let in_tok = crate::state::format_tokens_compact(input_tokens);
    let out_tok = crate::state::format_tokens_compact(output_tokens);

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
}
