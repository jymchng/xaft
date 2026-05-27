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
    widgets::{Paragraph, Widget, Wrap},
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
        // Borderless — fill with a slightly distinct background so the input
        // area is visually separated from the chat above it.
        let input_bg = Style::default()
            .bg(self.theme.statusbar_bg)
            .fg(self.theme.fg);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(input_bg);
            }
        }

        if area.height == 0 || area.width < 4 {
            return;
        }

        // Use full area with 1-col left/right padding
        let inner = Rect::new(
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            area.height,
        );

        let content = if self.focused {
            // Show the live input buffer with a blinking cursor
            let buf_text = &self.state.input_buffer;
            let cursor = if self.state.tick % 60 < 30 {
                "▌"
            } else {
                " "
            };
            if buf_text.is_empty() {
                Line::from(Span::styled(
                    format!("> {cursor}"),
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(self.theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(buf_text.as_str(), Style::default().fg(self.theme.fg)),
                    Span::styled(cursor, Style::default().fg(self.theme.accent)),
                ])
            }
        } else {
            // When not focused, always show a clean placeholder — the submitted
            // task text must NOT persist in the input pane after the user sends it.
            let hint = if self.state.phase.is_active() {
                "(working… Tab to send another task)"
            } else if self.state.task_done {
                "(done — Tab to send next task)"
            } else {
                "(Tab to focus and enter a task)"
            };
            Line::from(Span::styled(hint, self.theme.dim()))
        };

        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .style(input_bg)
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
    fn renders_placeholder_when_unfocused() {
        // When unfocused, input bar shows a hint — NOT the submitted task text.
        let state = make_state("Fix the auth bug");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        // The submitted task must NOT appear in the input bar when unfocused
        assert!(
            !content.contains("Fix the auth bug"),
            "submitted task must not persist in input bar"
        );
        // Should show a placeholder hint instead
        assert!(content.contains("Tab"), "should show Tab hint");
    }

    #[test]
    fn renders_empty_task_as_placeholder() {
        let state = make_state("");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(content.contains("Tab"), "empty state shows Tab hint");
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
