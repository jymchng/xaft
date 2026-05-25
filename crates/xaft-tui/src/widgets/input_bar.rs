//! Input bar pane widget.
//!
//! Renders the user's submitted task as a read-only single-line (or
//! wrapped few-line) prompt bar at the bottom of the Chat column.
//! Matches the PRD layout: Chat 78% / InputBar 22% vertical split.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::state::{AppState, WorkflowPhase};
use crate::theme::Theme;

/// Read-only display of the submitted task / prompt.
pub struct InputBarWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> InputBarWidget<'a> {
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for InputBarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        // Phase-aware prefix icon
        let prefix = match self.state.phase {
            WorkflowPhase::Done => "✓ ",
            WorkflowPhase::Error => "✗ ",
            _ if self.state.phase.is_active() => "> ",
            _ => "> ",
        };

        let prefix_style = match self.state.phase {
            WorkflowPhase::Done => self.theme.success(),
            WorkflowPhase::Error => self.theme.error(),
            _ => Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD),
        };

        let block = Block::default()
            .title(Line::from(vec![Span::styled(
                " Task ",
                Style::default().fg(self.theme.dim),
            )]))
            .borders(Borders::ALL)
            .border_style(border_style)
            .style(self.theme.base());

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width < 4 {
            return;
        }

        let task = &self.state.task;
        let content = if task.is_empty() {
            Line::from(Span::styled("(no task)", self.theme.dim()))
        } else {
            Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(task.as_str(), Style::default().fg(self.theme.fg)),
            ])
        };

        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .style(self.theme.base())
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    fn make_state(task: &str) -> AppState {
        AppState::new(task)
    }

    #[test]
    fn renders_task_without_panic() {
        let state = make_state("Fix the auth bug");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(content.contains("Fix the auth bug"));
    }

    #[test]
    fn renders_empty_task_as_placeholder() {
        let state = make_state("");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(content.contains("no task"));
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let state = make_state("task");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        widget.render(Rect::new(0, 0, 5, 1), &mut buf);
    }
}
