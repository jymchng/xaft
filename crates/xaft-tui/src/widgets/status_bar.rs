//! Bottom status bar widget.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
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
        // Fill entire area with statusbar background
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_style(self.theme.statusbar());
            }
        }

        if area.width < 20 || area.height == 0 {
            return;
        }

        // ── Row 0: stats (tokens + cost + phase + agent + git) ───────────────
        let phase_text = if self.state.phase.is_active() {
            format!(
                " {} {} ",
                self.state.spinner_char(),
                self.state.phase.label()
            )
        } else {
            format!(" {} ", self.state.phase.label())
        };

        let agent_text = if !self.state.current_agent.is_empty() {
            format!("│ {} ", self.state.current_agent)
        } else {
            String::new()
        };

        let tokens_str = format_tokens(self.state.total_tokens());
        let cost_str = format!("${:.4}", self.state.total_cost_usd);
        let calls_str = format!("{} calls", self.state.total_llm_calls);

        let git_str = self
            .state
            .git_branch
            .as_deref()
            .map(|b| format!("  {}", b.chars().take(20).collect::<String>()))
            .unwrap_or_default();

        let stats_left = format!("{phase_text}{agent_text}");
        let stats_right = format!(" {tokens_str}  {cost_str}  {calls_str}{git_str} ");

        let stats_area = if area.height >= 2 {
            Rect::new(area.x, area.y, area.width, 1)
        } else {
            area
        };

        Paragraph::new(Line::from(Span::styled(
            stats_left.clone(),
            self.theme
                .statusbar()
                .add_modifier(ratatui::style::Modifier::BOLD),
        )))
        .render(stats_area, buf);

        let stats_right_len = stats_right.len() as u16;
        if stats_right_len < area.width {
            let x = area.right().saturating_sub(stats_right_len);
            buf.set_string(x, stats_area.top(), &stats_right, self.theme.statusbar());
        }

        // ── Row 1: keybinding help (only if 2+ rows available) ───────────────
        if area.height < 2 {
            return;
        }

        let help_area = Rect::new(area.x, area.y + 1, area.width, 1);

        let keys: &str = if self.state.approval_queue.has_pending() {
            " [a]Approve  [r]Reject  [s]Skip  [A]All≤Med  [R]Rej.all  [↑↓]nav  [h]history"
        } else if self.state.layout_manager.focused_type()
            == Some(crate::layout::PaneType::InputBar)
        {
            " [Enter]Send task  [Esc]Cancel  [Tab]Chat  [↑↓]Scroll  [q]Quit"
        } else if self.state.task_done {
            " Task done — [Tab]→Input for next task  [↑↓]Scroll history  [q]Quit"
        } else if self.state.phase.is_active() {
            " [↑↓]Scroll  [Tab]→Input  [q]Quit  │  Agents running — wait or Ctrl+C to cancel"
        } else {
            " [Tab]→Input  [↑↓]Scroll  [q]Quit  │  Type a task in the Input bar, then [Enter]"
        };

        let keys_len = keys.len() as u16;
        if keys_len <= area.width {
            buf.set_string(area.x, help_area.top(), keys, self.theme.statusbar());
        } else {
            // Truncate to fit
            let display: String = keys.chars().take(area.width as usize).collect();
            buf.set_string(area.x, help_area.top(), &display, self.theme.statusbar());
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
}
