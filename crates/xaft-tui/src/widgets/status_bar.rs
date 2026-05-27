//! Bottom status bar widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::state::AppState;
use crate::theme::Theme;

/// Single-line status bar shown at the bottom of the terminal.
pub struct StatusBarWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
}

impl<'a> StatusBarWidget<'a> {
    pub fn new(state: &'a AppState, theme: &'a Theme) -> Self {
        Self { state, theme }
    }
}

impl Widget for StatusBarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Transparent background — no explicit bg color.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(Style::default());
            }
        }

        if area.width < 20 || area.height == 0 {
            return;
        }

        let orange = ratatui::style::Color::Rgb(230, 130, 40);
        let orange_style = Style::default().fg(orange);

        // Top separator removed per user request — text renders at row 0 directly.

        let tokens_str = format_tokens(self.state.total_tokens());
        let cost_str = format!("${:.4}", self.state.total_cost_usd);
        let phase_part = if self.state.phase.is_active() {
            format!("  ·  {}", self.state.phase.label())
        } else {
            String::new()
        };
        let left = format!("  xaft  ·  {tokens_str}  ·  {cost_str}{phase_part}");

        let keys: &str = if self.state.approval_queue.has_pending() {
            "[a] approve  [r] reject  [s] skip"
        } else if self.state.layout_manager.focused_type()
            == Some(crate::layout::PaneType::InputBar)
        {
            "[Enter] send  [Esc] cancel  [q] quit"
        } else if self.state.task_done {
            "[Tab] next task  [q] quit"
        } else if self.state.phase.is_active() {
            "[↑↓] scroll  [Esc] cancel  [q] quit"
        } else {
            "[Tab] focus  [↑↓] scroll  [q] quit"
        };

        // Section 4: context usage % shown when >= 70%; colored by urgency.
        const CONTEXT_WINDOW_TOKENS: u64 = 262_112;
        let tok_used = self.state.total_tokens();
        let ctx_pct = (tok_used * 100) / CONTEXT_WINDOW_TOKENS;
        let ctx_str = if ctx_pct >= 70 {
            format!("  {}% context used  ", ctx_pct)
        } else {
            String::new()
        };
        let ctx_style = if ctx_pct >= 90 {
            self.theme.error()
        } else {
            self.theme.warning()
        };
        let keys_str = format!("{keys}  ");

        // Text always at row 0 (no separator above it).
        let text_y = area.top();
        let text_area = Rect::new(area.x, text_y, area.width, 1);
        Paragraph::new(Line::from(Span::styled(left, orange_style))).render(text_area, buf);

        // Right side: keys + context% right-aligned, both in orange.
        let keys_len = keys_str.chars().count() as u16;
        let ctx_len = ctx_str.chars().count() as u16;
        let total_right_len = keys_len + ctx_len;
        if total_right_len < area.width {
            let start_x = area.right().saturating_sub(total_right_len);
            buf.set_string(start_x, text_y, &keys_str, orange_style);
            if !ctx_str.is_empty() {
                buf.set_string(start_x + keys_len, text_y, &ctx_str, ctx_style);
            }
        }
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k tok", n as f64 / 1_000.0)
    } else {
        format!("{n} tok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn make_buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    #[test]
    fn renders_without_panic() {
        let state = AppState::new("test task");
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(80, 1);
        widget.render(Rect::new(0, 0, 80, 1), &mut buf);
        // Should not panic
    }

    #[test]
    fn renders_with_tiny_area() {
        let state = AppState::new("t");
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(5, 1);
        widget.render(Rect::new(0, 0, 5, 1), &mut buf);
    }

    #[test]
    fn renders_text_at_row_zero_no_separator() {
        // Top separator removed — text renders directly at row 0.
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(80, 2);
        widget.render(Rect::new(0, 0, 80, 2), &mut buf);
        // Row 0 must contain text content ("xaft"), not a plain separator line.
        let top_row: String = (0..80u16)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            top_row.contains("xaft"),
            "row 0 must contain status text directly (no separator), got: {top_row:?}"
        );
    }

    #[test]
    fn context_indicator_hidden_below_threshold() {
        use crate::bridge::TuiEvent;
        let mut state = AppState::new("test");
        // 50% of 262_112 = 131_056 tokens — below 70% threshold, no indicator
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 131_056,
            output_tokens: 0,
            cost_usd: 0.0,
            duration_ms: 100.0,
        });
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(120, 2);
        widget.render(Rect::new(0, 0, 120, 2), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(
            !content.contains("context used"),
            "context indicator must not appear below 70%"
        );
    }

    #[test]
    fn context_indicator_shown_at_70_percent() {
        use crate::bridge::TuiEvent;
        let mut state = AppState::new("test");
        // 75% of 262_112 = ~196_584 tokens
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 196_584,
            output_tokens: 0,
            cost_usd: 0.0,
            duration_ms: 100.0,
        });
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(120, 2);
        widget.render(Rect::new(0, 0, 120, 2), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(
            content.contains("context used"),
            "context indicator must appear at ≥70%; content: {content:?}"
        );
    }

    #[test]
    fn context_indicator_uses_error_color_above_90_percent() {
        use crate::bridge::TuiEvent;
        use ratatui::style::Color;
        let mut state = AppState::new("test");
        // 95% of 262_112 = ~249_006 tokens
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 249_006,
            output_tokens: 0,
            cost_usd: 0.0,
            duration_ms: 100.0,
        });
        let theme = Theme::dark();
        let error_color = theme.error;
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(120, 2);
        widget.render(Rect::new(0, 0, 120, 2), &mut buf);
        // Find a cell containing '%' and check it has the error foreground color
        let pct_cell = buf.content.iter().find(|c| c.symbol() == "%");
        assert!(pct_cell.is_some(), "% character must be present at ≥90%");
        assert_eq!(
            pct_cell.unwrap().fg,
            Color::Rgb(185, 80, 75), // theme.error
            "context indicator must use error color above 90%: expected {:?}",
            error_color
        );
    }
}
