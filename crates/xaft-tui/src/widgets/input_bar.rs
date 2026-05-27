//! Input bar pane widget.
//!
//! Renders the user's submitted task as a read-only single-line (or
//! wrapped few-line) prompt bar at the bottom of the Chat column.
//! Matches the Claude Code visual style: borderless, inline, single-column.

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
        // Fill entire area with base background.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(self.theme.base());
            }
        }

        if area.height == 0 || area.width < 4 {
            return;
        }

        // ── Two horizontal border lines — demarcate the input zone ────────────
        let sep_style = Style::default()
            .fg(self.theme.border)
            .bg(self.theme.bg);

        // Top separator (row 0)
        for x in area.left()..area.right() {
            buf[(x, area.top())].set_symbol("─").set_style(sep_style);
        }

        // Bottom separator (last row, only if height allows)
        if area.height >= 2 {
            let bot_y = area.bottom() - 1;
            for x in area.left()..area.right() {
                buf[(x, bot_y)].set_symbol("─").set_style(sep_style);
            }
        }

        // Content area is between the two separator rows
        if area.height < 3 {
            return;
        }
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height - 2,
        );

        let content = if self.focused {
            let buf_text = &self.state.input_buffer;
            let cursor = if self.state.tick % 60 < 30 { "▌" } else { " " };
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
            // Working indicator when unfocused: phase verb + elapsed + tokens.
            let hint: String = if self.state.phase.is_active() {
                self.state.working_indicator()
            } else if self.state.task_done {
                "· done".to_string()
            } else {
                "·".to_string()
            };
            Line::from(Span::styled(hint, self.theme.dim()))
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
        // Should show a placeholder hint — working indicator or idle dot
        assert!(
            content.contains('·') || content.contains('✢') || content.contains('✣')
                || content.contains('✤') || content.contains('✥'),
            "should show placeholder hint"
        );
    }

    #[test]
    fn renders_empty_task_as_placeholder() {
        let state = make_state("");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(content.contains('·'), "empty state shows · hint");
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let state = make_state("task");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 1));
        widget.render(Rect::new(0, 0, 5, 1), &mut buf);
    }

    #[test]
    fn working_indicator_shows_icon_and_verb_when_active() {
        use crate::bridge::TuiEvent;
        let mut state = make_state("refactor auth");
        // Start a planning phase
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "planner".into(),
            call_index: 0,
        });
        let indicator = state.working_indicator();
        assert!(
            indicator.contains("Planning"),
            "planning phase must show 'Planning' verb, got: {indicator:?}"
        );
        assert!(
            indicator.contains('✢') || indicator.contains('✣')
                || indicator.contains('✤') || indicator.contains('✥'),
            "must contain ✢/✣/✤/✥ icon, got: {indicator:?}"
        );
    }

    #[test]
    fn working_indicator_includes_elapsed_and_output_tokens() {
        use crate::bridge::TuiEvent;
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        // Simulate output tokens arriving
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 1000,
            output_tokens: 5000,
            cost_usd: 0.01,
            duration_ms: 200.0,
        });
        // Start new LLM call (keeps agent_start_time from the original LlmCallStarting)
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 1,
        });
        let indicator = state.working_indicator();
        // Should include elapsed (may be "0s") and token direction arrow
        assert!(
            indicator.contains('↓'),
            "must contain ↓ arrow for output tokens, got: {indicator:?}"
        );
        assert!(
            indicator.contains("tokens"),
            "must contain 'tokens' label, got: {indicator:?}"
        );
    }

    #[test]
    fn active_agent_thinking_not_overwritten_by_tick() {
        use crate::bridge::TuiEvent;
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        // Set thinking to a text excerpt (as AgentOutput does)
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "I am analyzing the codebase structure".into(),
        });
        let thinking_before = state.active_agent_thinking.clone();
        // Fire many Tick events — they must NOT overwrite the excerpt
        for _ in 0..60 {
            state.handle_event(TuiEvent::Tick);
        }
        assert_eq!(
            state.active_agent_thinking, thinking_before,
            "Tick must not overwrite active_agent_thinking set by AgentOutput"
        );
    }
}
