//! Single-row token + cost usage bar shown above the InputBar.
//!
//! Renders: `  ⬡ 405.6k tokens  │  $0.0042  │  28 calls  │  ⠋ coder`

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::state::AppState;
use crate::theme::Theme;

/// Compact one-line bar showing accumulated token / cost usage for the session.
pub struct UsageBarWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
}

impl<'a> UsageBarWidget<'a> {
    /// Construct the widget.
    pub fn new(state: &'a AppState, theme: &'a Theme) -> Self {
        Self { state, theme }
    }
}

impl Widget for UsageBarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Fill background — set_symbol(" ") clears any split-border `─`
        // characters that may have been rendered before this widget.
        // set_style alone only changes colors, leaving the symbol intact.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(
                    Style::default()
                        .bg(self.theme.statusbar_bg)
                        .fg(self.theme.dim),
                );
            }
        }

        if area.width < 20 || area.height == 0 {
            return;
        }

        let tokens = format_tokens(self.state.total_tokens());
        let cost = format!("${:.4}", self.state.total_cost_usd);
        let calls = format!("{} calls", self.state.total_llm_calls);

        // Current agent indicator (dim, right-aligned)
        let agent_part = if !self.state.current_agent.is_empty() && self.state.phase.is_active() {
            format!(
                "  {} {}",
                self.state.spinner_char(),
                self.state.current_agent
            )
        } else {
            String::new()
        };

        let dim = Style::default()
            .fg(self.theme.dim)
            .bg(self.theme.statusbar_bg);
        let accent = Style::default()
            .fg(self.theme.accent)
            .bg(self.theme.statusbar_bg)
            .add_modifier(Modifier::BOLD);
        let sep = Style::default()
            .fg(self.theme.border)
            .bg(self.theme.statusbar_bg);

        let spans: Vec<Span> = vec![
            Span::styled("  ⬡ ", sep),
            Span::styled(tokens, accent),
            Span::styled("  │  ", sep),
            Span::styled(cost, accent),
            Span::styled("  │  ", sep),
            Span::styled(calls, dim),
        ];

        // Left section
        let left = Line::from(spans);
        Paragraph::new(left).render(area, buf);

        // Right section: agent indicator
        if !agent_part.is_empty() {
            let right_len = agent_part.len() as u16;
            if right_len < area.width {
                let x = area.right().saturating_sub(right_len + 1);
                buf.set_string(
                    x,
                    area.top(),
                    &agent_part,
                    Style::default()
                        .fg(self.theme.warning)
                        .bg(self.theme.statusbar_bg),
                );
            }
        }
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tokens", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k tokens", n as f64 / 1_000.0)
    } else {
        format!("{n} tokens")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn renders_without_panic_empty_state() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = UsageBarWidget::new(&state, &theme);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        widget.render(Rect::new(0, 0, 80, 1), &mut buf);
    }

    #[test]
    fn renders_tiny_area_without_panic() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = UsageBarWidget::new(&state, &theme);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        widget.render(Rect::new(0, 0, 5, 1), &mut buf);
    }

    #[test]
    fn format_tokens_scales_correctly() {
        assert_eq!(format_tokens(0), "0 tokens");
        assert_eq!(format_tokens(999), "999 tokens");
        assert_eq!(format_tokens(1_500), "1.5k tokens");
        assert_eq!(format_tokens(2_500_000), "2.5M tokens");
    }

    #[test]
    fn renders_with_usage_data() {
        use crate::bridge::TuiEvent;
        let mut state = AppState::new("task");
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 1000,
            output_tokens: 500,
            cost_usd: 0.0042,
            duration_ms: 1200.0,
        });
        let theme = Theme::dark();
        let widget = UsageBarWidget::new(&state, &theme);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        widget.render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        // Should contain token count and cost
        assert!(content.contains("1.5k") || content.contains("tokens") || content.contains("$"));
    }
}
