//! Input bar pane widget.
//!
//! Visual structure when a phase is active (height ≥ 3):
//!
//! ```text
//! Row 0:  ✤ Coding…  (0s · ↓ 1.2k tokens)   ← working indicator, bold-sweep
//! Row 1:  ─────────────────────────────────    ← separator
//! Row 2:  · done  /  >▌                        ← content
//! ```
//!
//! When idle (no active phase) row 0 is also a separator so the two borders
//! visually frame the content row.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::state::{AppState, format_elapsed, format_tokens_compact};
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
        // Transparent background: clear cells without imposing a bg color.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(Style::default());
            }
        }

        if area.height == 0 || area.width < 4 {
            return;
        }

        // Yellow separator lines always frame the input zone — both top AND bottom.
        let yellow = ratatui::style::Color::Rgb(220, 180, 40);
        let sep_style = Style::default().fg(yellow);

        // ── Top separator (always yellow) ─────────────────────────────────────
        for x in area.left()..area.right() {
            buf[(x, area.top())].set_symbol("─").set_style(sep_style);
        }

        // ── Bottom separator (always yellow, fixes "white border" after send) ──
        if area.height >= 2 {
            let bot_y = area.bottom() - 1;
            for x in area.left()..area.right() {
                buf[(x, bot_y)].set_symbol("─").set_style(sep_style);
            }
        }

        // ── Content between separators (cursor or idle hint only) ──────────────
        // Working indicator is rendered in the conversation pane above, not here.
        if area.height >= 3 {
            let content_area = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
            let content = build_content_line(self.state, self.theme, self.focused);
            Paragraph::new(content)
                .wrap(Wrap { trim: false })
                .style(Style::default())
                .render(content_area, buf);
        }
    }
}

/// Build the working indicator `Line` with a left-to-right bold sweep on the verb.
///
/// The sweep advances one character every 8 ticks (~130ms at 60fps).  After all
/// characters are bold the counter wraps back to 0, creating a repeating pulse.
fn build_indicator_line<'a>(state: &'a AppState, theme: &'a Theme) -> Line<'a> {
    let icon = state.indicator_icon();
    let verb = state.phase_verb();
    let verb_chars: Vec<char> = verb.chars().collect();
    let total = verb_chars.len();

    // Bold sweep: one extra char bolded per 8 ticks; pause at end before reset.
    // The +4 in the modulus gives 4 ticks of "all bold" before the reset.
    let bold_count = ((state.tick as usize / 8) % (total + 4)).min(total);

    let yellow = ratatui::style::Color::Rgb(220, 180, 40);
    let dim_base = Style::default().fg(yellow);
    let bold_style = dim_base.add_modifier(Modifier::BOLD);

    let elapsed_suffix = if let Some(start) = state.agent_start_time {
        let tok = format_tokens_compact(state.total_output_tokens);
        format!("… ({} · ↓ {tok} tokens)", format_elapsed(start.elapsed()))
    } else {
        "…".to_string()
    };

    let mut spans: Vec<Span> = Vec::with_capacity(5);
    spans.push(Span::styled(format!("{icon} "), dim_base));

    if bold_count > 0 {
        let bold_part: String = verb_chars[..bold_count].iter().collect();
        spans.push(Span::styled(bold_part, bold_style));
    }
    let normal_part: String = verb_chars[bold_count..].iter().collect();
    if !normal_part.is_empty() {
        spans.push(Span::styled(normal_part, dim_base));
    }

    spans.push(Span::styled(elapsed_suffix, dim_base));

    Line::from(spans)
}

/// Build the content line (cursor prompt when focused, idle hint when not).
fn build_content_line<'a>(state: &'a AppState, theme: &'a Theme, focused: bool) -> Line<'a> {
    if focused {
        let yellow = ratatui::style::Color::Rgb(220, 180, 40);
        let buf_text = &state.input_buffer;
        let cursor = if state.tick % 60 < 30 { "▌" } else { " " };
        if buf_text.is_empty() {
            Line::from(Span::styled(
                format!("❯ {cursor}"),
                Style::default().fg(yellow).add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(vec![
                Span::styled(
                    "❯ ",
                    Style::default().fg(yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(buf_text.as_str(), Style::default().fg(yellow)),
                Span::styled(cursor, Style::default().fg(yellow)),
            ])
        }
    } else if state.task_done {
        Line::from(Span::styled("· done", Style::default().fg(theme.dim)))
    } else {
        Line::from(Span::styled("·", Style::default().fg(theme.dim)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::TuiEvent;
    use crate::state::AppState;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    fn make_state(task: &str) -> AppState {
        AppState::new(task)
    }

    #[test]
    fn renders_placeholder_when_unfocused() {
        let state = make_state("Fix the auth bug");
        let theme = Theme::dark();
        let widget = InputBarWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        widget.render(Rect::new(0, 0, 80, 3), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(
            !content.contains("Fix the auth bug"),
            "submitted task must not persist in input bar"
        );
        assert!(
            content.contains('·')
                || content.contains('✢')
                || content.contains('✣')
                || content.contains('✤')
                || content.contains('✥'),
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
        let mut state = make_state("refactor auth");
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
            indicator.contains('✢')
                || indicator.contains('✣')
                || indicator.contains('✤')
                || indicator.contains('✥'),
            "must contain ✢/✣/✤/✥ icon, got: {indicator:?}"
        );
    }

    #[test]
    fn working_indicator_includes_elapsed_and_output_tokens() {
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        state.handle_event(TuiEvent::LlmCallComplete {
            agent_name: "coder".into(),
            input_tokens: 1000,
            output_tokens: 5000,
            cost_usd: 0.01,
            duration_ms: 200.0,
        });
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 1,
        });
        let indicator = state.working_indicator();
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
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "I am analyzing the codebase structure".into(),
        });
        let thinking_before = state.active_agent_thinking.clone();
        for _ in 0..60 {
            state.handle_event(TuiEvent::Tick);
        }
        assert_eq!(
            state.active_agent_thinking, thinking_before,
            "Tick must not overwrite active_agent_thinking set by AgentOutput"
        );
    }

    /// When an agent phase is active, the indicator row (row 0) contains the
    /// verb with increasing bold characters as tick advances.
    #[test]
    fn bold_sweep_advances_with_tick() {
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        let theme = Theme::dark();

        // At tick=0: bold_count = 0 (no bold chars yet)
        state.tick = 0;
        let bold0 = {
            let line = build_indicator_line(&state, &theme);
            line.spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .count()
        };
        // At tick=8: bold_count = 1 (first char bold)
        state.tick = 8;
        let bold1 = {
            let line = build_indicator_line(&state, &theme);
            line.spans
                .iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .count()
        };
        assert!(
            bold1 >= bold0,
            "More chars must be bold at higher tick: bold0={bold0}, bold1={bold1}"
        );
    }

    /// Input bar always shows yellow borders + idle content when phase active.
    /// Working indicator moved to conversation pane; InputBar shows ·.
    #[test]
    fn input_bar_shows_idle_dot_when_active_not_focused() {
        let mut state = make_state("task");
        state.handle_event(TuiEvent::LlmCallStarting {
            agent_name: "coder".into(),
            call_index: 0,
        });
        state.tick = 16;
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        InputBarWidget::new(&state, &theme, false).render(Rect::new(0, 0, 80, 3), &mut buf);

        let row0: String = (0..80u16)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect();
        let row1: String = (0..80u16)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect();
        let row2: String = (0..80u16)
            .map(|x| buf[(x, 2)].symbol().to_string())
            .collect();

        assert!(row0.contains('─'), "row 0 must be top separator");
        // Working indicator is now in the conversation pane — InputBar shows idle ·
        assert!(
            row1.contains('·'),
            "row 1 must show idle dot when not focused and indicator moved to chat pane, got: {row1:?}"
        );
        assert!(row2.contains('─'), "row 2 must be bottom separator");
    }

    /// Background cells must have no bg color (transparent).
    #[test]
    fn background_is_transparent() {
        use ratatui::style::Color;
        let state = make_state("task");
        let theme = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 3));
        InputBarWidget::new(&state, &theme, false).render(Rect::new(0, 0, 80, 3), &mut buf);
        // All cells should have Color::Reset (no explicit bg) — not the theme bg color.
        let has_theme_bg = buf.content.iter().any(|c| c.bg == Color::Rgb(16, 17, 19));
        assert!(
            !has_theme_bg,
            "InputBar background must be transparent (no explicit bg color)"
        );
    }
}
