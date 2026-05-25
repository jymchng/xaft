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

        if area.width < 20 {
            return;
        }

        // Left section: phase + current agent
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

        // Center: task (truncated)
        let task_max = (area.width as usize).saturating_sub(60);
        let task_display = if self.state.task.len() > task_max && task_max > 4 {
            format!("{}…", &self.state.task[..task_max.saturating_sub(1)])
        } else {
            self.state.task.clone()
        };

        // Right section: tokens + cost + keybindings
        let tokens_str = format_tokens(self.state.total_tokens());
        let cost_str = format!("${:.4}", self.state.total_cost_usd);
        let keys = if self.state.approval_queue.has_pending() {
            " [a]Approve [r]Reject [s]Skip [A]All [R]Rej.all "
        } else if self.state.layout_manager.focused_type()
            == Some(crate::layout::PaneType::InputBar)
        {
            " [Enter]Send [Esc]Cancel [Q]Quit "
        } else if self.state.task_done {
            " Done — [Tab]→Input to send next task  [Q]Quit "
        } else {
            " [Tab]Focus [↑↓]Scroll [Q]Quit "
        };

        let git_str = self
            .state
            .git_branch
            .as_deref()
            .map(|b| {
                let short = b.chars().take(20).collect::<String>();
                format!("  {short}")
            })
            .unwrap_or_default();

        let left = format!("{phase_text}{agent_text}");
        let right = format!("{tokens_str} {cost_str}{git_str}{keys}");
        let center = task_display;

        // Render left-aligned
        Paragraph::new(Line::from(vec![
            Span::styled(
                left,
                self.theme
                    .statusbar()
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(format!(" {center}"), self.theme.statusbar()),
        ]))
        .render(area, buf);

        // Render right-aligned by writing from the right edge
        let right_len = right.len() as u16;
        if right_len < area.width {
            let x = area.right().saturating_sub(right_len);
            buf.set_string(x, area.top(), &right, self.theme.statusbar());
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
