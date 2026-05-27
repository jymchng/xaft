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
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(self.theme.statusbar());
            }
        }

        if area.width < 20 || area.height == 0 {
            return;
        }

        // Section 6: separator at top of status bar, embedding git branch name.
        let sep_style = Style::default().fg(self.theme.dim).bg(self.theme.statusbar_bg);
        let branch = self.state.git_branch.as_deref().unwrap_or("");
        if !branch.is_empty() && area.width > (branch.len() + 6) as u16 {
            // Format: ──────── branch-name ──>
            let branch_part = format!(" {} ──>", branch);
            let dash_count = (area.width as usize).saturating_sub(branch_part.len());
            let sep: String = "─".repeat(dash_count) + &branch_part;
            let sep_display: String = sep.chars().take(area.width as usize).collect();
            buf.set_string(area.left(), area.top(), &sep_display, sep_style);
        } else {
            for x in area.left()..area.right() {
                buf[(x, area.top())].set_symbol("─").set_style(sep_style);
            }
        }

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

        // Section 4: append context usage % when >= 70%.
        const CONTEXT_WINDOW_TOKENS: u64 = 262_112;
        let tok_used = self.state.total_tokens();
        let ctx_pct = (tok_used * 100) / CONTEXT_WINDOW_TOKENS;
        let ctx_str = if ctx_pct >= 70 {
            format!("  {}% context", ctx_pct)
        } else {
            String::new()
        };
        let right = format!("{keys}{ctx_str}  ");

        // When height >= 2: separator on row 0, text on row 1.
        // When height == 1: text overlays the separator row (compact mode).
        let text_y = if area.height >= 2 { area.top() + 1 } else { area.top() };
        let text_area = Rect::new(area.x, text_y, area.width, 1);
        Paragraph::new(Line::from(Span::styled(left, self.theme.statusbar())))
            .render(text_area, buf);
        let right_len = right.chars().count() as u16;
        if right_len < area.width {
            buf.set_string(
                area.right().saturating_sub(right_len),
                text_y,
                &right,
                self.theme.statusbar(),
            );
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
    fn renders_separator_at_top() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = StatusBarWidget::new(&state, &theme);
        let mut buf = make_buf(80, 2);
        widget.render(Rect::new(0, 0, 80, 2), &mut buf);
        // Top row should contain separator character
        let top_row: String = (0..80u16)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect();
        assert!(top_row.contains('─'), "separator row must contain ─");
    }
}
