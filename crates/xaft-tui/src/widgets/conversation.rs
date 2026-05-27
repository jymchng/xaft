//! Streaming conversation / output pane widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::state::{AppState, OutputKind};
use crate::theme::Theme;

/// Main conversation output pane.
pub struct ConversationWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> ConversationWidget<'a> {
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for ConversationWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Transparent background — clear cells without imposing a background color,
        // letting the terminal's natural background show through.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(Style::default());
            }
        }

        if area.height == 0 {
            return;
        }

        // Content area: use full area with 1-col left/right padding
        let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
        let height = inner.height as usize;
        if height == 0 {
            return;
        }

        // Transient bottom area: agent thinking shown below history.
        // Hidden when user has scrolled up to read history.
        let at_bottom = self.state.output_scroll == 0;
        let thinking_lines: Vec<&str> = if at_bottom {
            self.state
                .active_agent_thinking
                .as_deref()
                .map(|s| s.lines().collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let thinking_rows = (thinking_lines.len() as u16).min((height as u16) / 2);
        let history_height = height.saturating_sub(thinking_rows as usize);

        // Feed the current inner width back to AppState so handle_key can
        // compute correct wrap-aware scroll boundaries.
        // If the pane is too narrow to render (< 2 cols), fall back to the
        // last known good width rather than passing 0 to visible_output_scrolled
        // (which would return an empty slice and show a blank pane).
        let inner_width = inner.width as usize;
        let effective_width = if inner_width > 0 {
            self.state.last_chat_inner_width.set(inner_width);
            inner_width
        } else {
            self.state.last_chat_inner_width.get().max(1)
        };

        // Wrap-aware selection for both auto-scroll and manual-scroll cases.
        // scroll_rows=0 pins to bottom; larger values slide the window upward
        // one visual row at a time, correctly handling wrapped lines.
        let visible = self
            .state
            .visible_output_scrolled(history_height, effective_width, self.state.output_scroll);

        // Compute true visual rows occupied by visible content BEFORE rendering
        // to Line objects. Used for bottom-anchor padding so wrapped lines don't
        // overflow the pane height and clip the newest content off-screen.
        let visible_vrows: usize = visible
            .iter()
            .map(|ol| AppState::visual_row_count_for(&ol.text, effective_width).max(1))
            .sum();

        let mut all_lines: Vec<Line> = visible
            .iter()
            .filter(|ol| {
                // Section 5: hide inline diff lines when show_diff_inline=false.
                // New format: diff body lines start with 6 spaces (indented line-number prefix).
                // Old format (legacy): lines starting with "  - " or "  + ".
                if !self.state.show_diff_inline {
                    let is_diff_body = matches!(
                        ol.kind,
                        OutputKind::Error | OutputKind::Success | OutputKind::ToolResult
                    ) && (ol.text.starts_with("      ")   // new format: 6-space indent
                        || ol.text.starts_with("  - ")   // legacy format
                        || ol.text.starts_with("  + ")); // legacy format
                    !is_diff_body
                } else {
                    true
                }
            })
            .map(|ol| render_output_line(ol, self.theme))
            .collect();

        // Append transient thinking indicator at bottom (single line).
        if !thinking_lines.is_empty() {
            let max_w = inner.width.saturating_sub(2) as usize;
            let display: String = thinking_lines[0].chars().take(max_w).collect();
            all_lines.push(Line::from(Span::styled(
                display.to_string(),
                Style::default()
                    .fg(self.theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        }

        // Bottom-anchor: pad with empty lines at the TOP.
        // Padding is based on VISUAL rows (not logical line count) so wrapped
        // lines don't overflow pane height and clip the newest content.
        let content_vrows = visible_vrows + thinking_rows as usize;
        let pad = height.saturating_sub(content_vrows);
        if pad > 0 {
            let mut padded = vec![Line::default(); pad];
            padded.extend(all_lines);
            all_lines = padded;
        }

        Paragraph::new(all_lines)
            .style(Style::default())
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        // Section 1.4: scroll position indicator at top-right showing % from bottom.
        // Denominator is total visual rows so the percentage is accurate for wrapped lines.
        if self.state.output_scroll > 0 {
            let total_vrows = self.state.total_visual_rows(effective_width);
            let pct = (self.state.output_scroll * 100 / total_vrows.max(1)).min(100);
            let indicator = format!(" ↑{}% ", pct);
            let x = inner.right().saturating_sub(indicator.len() as u16 + 1);
            let y = inner.top();
            if x < inner.right() {
                buf.set_string(x, y, &indicator, self.theme.dim());
            }
        }
    }
}

fn render_output_line<'a>(line: &'a crate::state::OutputLine, theme: &'a Theme) -> Line<'a> {
    let text_style = match line.kind {
        OutputKind::AgentText => Style::default().fg(theme.fg),
        OutputKind::ToolCall => Style::default().fg(theme.dim),
        OutputKind::ToolResult => Style::default().fg(theme.dim),
        OutputKind::System => Style::default().fg(theme.dim),
        OutputKind::Error => theme.error(),
        OutputKind::Success => theme.success(),
        OutputKind::AgentMarker => theme.agent(),
        OutputKind::UserMessage => Style::default(),
    };
    Line::from(Span::styled(line.text.clone(), text_style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{OutputKind, OutputLine};
    use std::time::Instant;

    fn make_line(kind: OutputKind, text: &str) -> OutputLine {
        OutputLine {
            kind,
            text: text.to_string(),
            agent: None,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn render_output_line_agent_text() {
        let theme = Theme::dark();
        let line = make_line(OutputKind::AgentText, "hello");
        let rendered = render_output_line(&line, &theme);
        let content: String = rendered.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains("hello"));
    }

    #[test]
    fn render_output_line_error_colored() {
        let theme = Theme::dark();
        let line = make_line(OutputKind::Error, "boom");
        let rendered = render_output_line(&line, &theme);
        let span = rendered.spans.last().unwrap();
        assert_eq!(span.style.fg, Some(theme.error));
    }

    #[test]
    fn render_output_line_with_agent() {
        let theme = Theme::dark();
        let mut line = make_line(OutputKind::AgentText, "output");
        line.agent = Some("coder".into());
        let rendered = render_output_line(&line, &theme);
        let content: String = rendered.spans.iter().map(|s| s.content.as_ref()).collect();
        // Agent prefix not rendered — phase markers in stream identify agent
        assert!(!content.contains("[coder]"));
        assert!(content.contains("output"));
    }

    /// Verifies that wrapped lines don't cause the newest content to be clipped
    /// off the bottom of the pane.  Padding must be based on visual rows so that
    /// the total rendered rows (pad + content_vrows) == pane height.
    #[test]
    fn visual_padding_correct_for_wrapped_lines() {
        use crate::bridge::TuiEvent;
        use crate::state::AppState;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let mut state = AppState::new("test");
        let theme = crate::theme::Theme::dark();

        // Each line is 160 chars — wraps to 2 rows at width=78 (inner after 1-col padding each side).
        // 3 such lines occupy 6 visual rows. Pane height=20 → pad should be 14, not 17.
        let long_text: String = "A".repeat(160);
        for _ in 0..3 {
            state.handle_event(TuiEvent::AgentOutput {
                agent_name: "coder".into(),
                content: long_text.clone(),
            });
        }
        state.output_scroll = 0;
        // Tell state the inner width so visual_row_count_for gives correct counts
        state.last_chat_inner_width.set(78);

        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        let widget = ConversationWidget::new(&state, &theme, false);
        widget.render(area, &mut buf);

        // The bottom rows (rows 14..20) should contain 'A' characters (content), not blanks.
        // If padding was computed from logical line count (3) instead of visual rows (6),
        // content would start at row 17 and wrap would push the 3rd line off-screen.
        let bottom_row: String = (0..80u16)
            .map(|x| buf[(x, 19)].symbol().to_string())
            .collect();
        assert!(
            bottom_row.contains('A'),
            "bottom row must contain content — newest line clipped off-screen: {:?}",
            bottom_row
        );
    }

    /// After resize to a very narrow area (width < 2), the widget must not panic
    /// and must use the cached width rather than passing 0 to visible_output_scrolled.
    #[test]
    fn narrow_pane_does_not_blank_or_panic() {
        use crate::bridge::TuiEvent;
        use crate::state::AppState;
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let mut state = AppState::new("test");
        let theme = crate::theme::Theme::dark();
        state.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".into(),
            content: "hello".into(),
        });
        state.last_chat_inner_width.set(40); // simulate prior good render

        // Area of width=1 → inner.width=0 after saturation subtraction
        let area = Rect::new(0, 0, 1, 10);
        let mut buf = Buffer::empty(area);
        // Should not panic
        ConversationWidget::new(&state, &theme, false).render(area, &mut buf);
    }
}
