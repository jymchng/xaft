//! Token / cost dashboard widget.
//!
//! Shows total token count, cost, LLM call count, current model, and a
//! per-agent breakdown sorted by cost. Designed to be compact and dense.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Widget},
};

use crate::state::AppState;
use crate::theme::Theme;

/// Dense token/cost statistics dashboard.
pub struct TokenDashboardWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> TokenDashboardWidget<'a> {
    /// Construct the widget.
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for TokenDashboardWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        let block = Block::default()
            .title(" Stats ")
            .title_style(
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(border_style)
            .style(self.theme.base());

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 1 || inner.width < 4 {
            return;
        }

        let available = inner.height as usize;
        let mut items: Vec<ListItem> = Vec::with_capacity(available);

        // ── Summary rows ───────────────────────────────────────────────────────

        let tokens_str = format_tokens(self.state.total_tokens());
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Tokens ", self.theme.dim()),
            Span::styled(tokens_str, self.theme.base().add_modifier(Modifier::BOLD)),
        ])));

        let cost_str = format_cost(self.state.total_cost_usd);
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Cost   ", self.theme.dim()),
            Span::styled(
                cost_str,
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
        ])));

        items.push(ListItem::new(Line::from(vec![
            Span::styled("Calls  ", self.theme.dim()),
            Span::styled(self.state.total_llm_calls.to_string(), self.theme.base()),
        ])));

        if !self.state.current_agent.is_empty() {
            let agent = shorten(
                &self.state.current_agent,
                (inner.width as usize).saturating_sub(8),
            );
            items.push(ListItem::new(Line::from(vec![
                Span::styled("Agent  ", self.theme.dim()),
                Span::styled(agent, Style::default().fg(self.theme.agent)),
            ])));
        }

        // ── Divider ────────────────────────────────────────────────────────────

        if items.len() < available {
            items.push(ListItem::new(Line::from(Span::styled(
                "─".repeat(inner.width.saturating_sub(2) as usize),
                self.theme.border(),
            ))));
        }

        // ── Per-agent breakdown ────────────────────────────────────────────────

        let remaining = available.saturating_sub(items.len());
        if remaining > 0 {
            let breakdown = self.state.top_agents_by_cost();
            for (agent, cost) in breakdown.iter().take(remaining) {
                let cost_s = format_cost(*cost);
                let agent_short = shorten(
                    agent,
                    (inner.width as usize).saturating_sub(cost_s.len() + 1),
                );
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(agent_short, Style::default().fg(self.theme.agent)),
                    Span::styled(format!(" {cost_s}"), self.theme.dim()),
                ])));
            }
        }

        // Hard-clamp to available height
        items.truncate(available);
        List::new(items).style(self.theme.base()).render(inner, buf);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_cost(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${:.2}", cost)
    } else if cost >= 0.01 {
        format!("${:.4}", cost)
    } else {
        format!("${:.6}", cost)
    }
}

fn shorten(s: &str, max: usize) -> String {
    if max < 2 {
        return s.chars().take(max).collect();
    }
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::TuiEvent;
    use crate::state::AppState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn make_state_with_stats() -> AppState {
        let mut state = AppState::new("test");
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 500,
            output_tokens: 1000,
            cost_usd: 0.0025,
            duration_ms: 800.0,
        });
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "qa".into(),
            input_tokens: 200,
            output_tokens: 300,
            cost_usd: 0.0008,
            duration_ms: 300.0,
        });
        state
    }

    #[test]
    fn renders_without_panic_empty() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = TokenDashboardWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 15));
        widget.render(Rect::new(0, 0, 30, 15), &mut buf);
    }

    #[test]
    fn renders_with_stats_without_panic() {
        let state = make_state_with_stats();
        let theme = Theme::dark();
        let widget = TokenDashboardWidget::new(&state, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 15));
        widget.render(Rect::new(0, 0, 30, 15), &mut buf);
    }

    #[test]
    fn renders_tiny_area_without_panic() {
        let state = make_state_with_stats();
        let theme = Theme::dark();
        let widget = TokenDashboardWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        widget.render(Rect::new(0, 0, 6, 3), &mut buf);
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1500), "1.5k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn format_cost_small() {
        assert!(format_cost(0.000001).starts_with('$'));
    }

    #[test]
    fn format_cost_large() {
        assert_eq!(format_cost(1.50), "$1.50");
    }
}
