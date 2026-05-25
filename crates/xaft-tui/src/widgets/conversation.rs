//! Streaming conversation / output pane widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::state::{AppState, OutputKind, WorkflowPhase};
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
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        // Title shows phase + spinner if active
        let title = if self.state.phase.is_active() {
            format!(
                " {} {} ",
                self.state.spinner_char(),
                self.state.phase.label()
            )
        } else if self.state.phase == WorkflowPhase::Done {
            " ✓ Done ".to_string()
        } else {
            " Output ".to_string()
        };

        let block = Block::default()
            .title(title)
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

        let height = inner.height as usize;
        if height == 0 {
            return;
        }

        // Collect visible lines
        let visible = self.state.visible_output(height);
        let lines: Vec<Line> = visible
            .iter()
            .map(|ol| render_output_line(ol, self.theme))
            .collect();

        // Pad with empty lines if fewer than height
        let mut all_lines = lines;
        while all_lines.len() < height {
            all_lines.push(Line::default());
        }

        Paragraph::new(all_lines)
            .style(self.theme.base())
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        // Scroll indicator
        if self.state.output_scroll > 0 {
            let indicator = format!(" ↑ {} ", self.state.output_scroll);
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
        OutputKind::AgentText => theme.base(),
        OutputKind::ToolResult => theme.dim(),
        OutputKind::System => Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
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
