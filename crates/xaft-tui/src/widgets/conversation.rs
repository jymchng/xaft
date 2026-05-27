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
        // Borderless — fill background and let content flow directly.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(self.theme.base());
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

        // Collect visible history lines using wrap-aware selection so that
        // lines wider than the pane don't push newer lines off the bottom.
        let inner_width = inner.width as usize;
        let visible = if self.state.output_scroll == 0 && inner_width > 0 {
            // Auto-scroll: select exactly the lines that fill the pane
            // accounting for text wrapping at the current pane width.
            self.state
                .visible_output_wrapped(history_height, inner_width)
        } else {
            // Manual scroll: use logical-line count (scroll unit = 1 line)
            self.state.visible_output(history_height)
        };
        let mut all_lines: Vec<Line> = visible
            .iter()
            .filter(|ol| {
                // Section 5: hide inline diff lines when show_diff_inline=false.
                if !self.state.show_diff_inline {
                    let is_diff_line = matches!(ol.kind, OutputKind::Error | OutputKind::Success)
                        && (ol.text.starts_with("  - ") || ol.text.starts_with("  + "));
                    !is_diff_line
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

        // Bottom-align: pad with empty lines at the TOP so newest content
        // sits directly above the InputBar, pushed up by each new message.
        let content_rows = all_lines.len();
        if content_rows < height {
            let pad = height - content_rows;
            let mut padded = vec![Line::default(); pad];
            padded.extend(all_lines.into_iter());
            all_lines = padded;
        }

        Paragraph::new(all_lines)
            .style(self.theme.base())
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        // Section 1.4: scroll position indicator at top-right showing % from bottom.
        if self.state.output_scroll > 0 {
            let n = self.state.output_lines.len();
            let pct = (self.state.output_scroll * 100) / n.max(1);
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
    let agent_span = if let Some(ref agent) = line.agent {
        Some(Span::styled(format!("[{agent}] "), theme.agent()))
    } else {
        None
    };

    let text_style = match line.kind {
        OutputKind::AgentText => Style::default().fg(theme.fg),
        OutputKind::ToolCall => Style::default().fg(theme.dim),
        OutputKind::ToolResult => Style::default().fg(theme.dim),
        OutputKind::System => Style::default().fg(theme.dim),
        OutputKind::Error => theme.error(),
        OutputKind::Success => theme.success(),
    };

    let mut spans: Vec<Span> = Vec::new();
    if let Some(agent) = agent_span {
        spans.push(agent);
    }
    spans.push(Span::styled(line.text.clone(), text_style));

    Line::from(spans)
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
        assert!(content.contains("[coder]"));
        assert!(content.contains("output"));
    }
}
