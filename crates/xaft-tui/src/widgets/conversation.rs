//! Streaming conversation / output pane widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
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
        // Borderless — fill background and let content flow directly.
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ").set_style(self.theme.base());
            }
        }

        if area.height == 0 {
            return;
        }

        // One-row header: phase icon + label (dim, left-aligned).
        // Keeps visual context without a heavy border box.
        let (phase_icon, phase_label, phase_style) = match &self.state.phase {
            WorkflowPhase::Idle => ("○", "xaft", Style::default().fg(self.theme.dim)),
            WorkflowPhase::Planning => (
                "◈",
                "Planning",
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            WorkflowPhase::Coding => (
                "◉",
                "Coding",
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            WorkflowPhase::QaReview => (
                "◎",
                "QA Review",
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            WorkflowPhase::Fixing => (
                "◌",
                "Fixing",
                Style::default()
                    .fg(self.theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            WorkflowPhase::Done => ("●", "Done", Style::default().fg(self.theme.success)),
            WorkflowPhase::Error => ("◍", "Error", Style::default().fg(self.theme.error)),
        };

        // Header row
        if area.height >= 1 {
            let spinner_str = if self.state.phase.is_active() {
                format!("{} ", self.state.spinner_char())
            } else {
                String::new()
            };
            let header = Line::from(vec![
                Span::styled(spinner_str, phase_style),
                Span::styled(format!("{phase_icon} {phase_label}"), phase_style),
            ]);
            Paragraph::new(header).render(
                Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
                buf,
            );
        }

        // Content area: everything below the header row
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(1),
        );
        let height = inner.height as usize;
        if height == 0 {
            return;
        }

        // Transient bottom area: tool status takes priority over agent thinking.
        // Hidden when user has scrolled up to read history.
        let at_bottom = self.state.output_scroll == 0;
        let tool_status = if at_bottom {
            self.state.active_tool_status.as_deref()
        } else {
            None
        };
        // Agent thinking: multi-line, shown below history when no tool is active.
        let thinking_lines: Vec<&str> = if at_bottom && tool_status.is_none() {
            self.state
                .active_agent_thinking
                .as_deref()
                .map(|s| s.lines().collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let thinking_rows = thinking_lines.len() as u16;
        // Total transient rows wanted (1 for tool status OR N for thinking).
        // Cap to at most height/2 so history always occupies ≥ half the pane.
        // This prevents errors and agent output from being hidden when the
        // terminal is very small or heavily zoomed.
        let raw_transient = if tool_status.is_some() {
            1u16
        } else {
            thinking_rows
        };
        let transient_rows = raw_transient.min((height as u16) / 2);
        let history_height = height.saturating_sub(transient_rows as usize);

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
            .map(|ol| render_output_line(ol, self.theme))
            .collect();

        // Append transient indicator(s) at bottom.
        if let Some(status) = tool_status {
            // Tool in flight — animated spinner with tool name
            let spinner = ['⣾', '⣽', '⣻', '⣷', '⣯', '⣟', '⡿', '⢿'];
            let i = (self.state.tick as usize / 3) % spinner.len();
            let line = format!("{} {status}", spinner[i]);
            let max_w = inner.width.saturating_sub(2) as usize;
            let display: String = line.chars().take(max_w).collect();
            all_lines.push(Line::from(Span::styled(
                display,
                Style::default()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else if !thinking_lines.is_empty() {
            // Agent thinking — dim italic, each line separate
            let max_w = inner.width.saturating_sub(4) as usize;
            for tline in &thinking_lines {
                let display: String = tline.chars().take(max_w).collect();
                all_lines.push(Line::from(Span::styled(
                    format!("  {display}"),
                    Style::default()
                        .fg(self.theme.dim)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }

        // Pad with empty lines if fewer than height
        while all_lines.len() < height {
            all_lines.push(Line::default());
        }

        Paragraph::new(all_lines)
            .style(self.theme.base())
            .wrap(Wrap { trim: false })
            .render(inner, buf);

        // Scroll position indicator at top-right
        if self.state.output_scroll > 0 {
            let indicator = format!(" ↑{} ", self.state.output_scroll);
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
        // Agent response: normal foreground for readability
        OutputKind::AgentText => Style::default().fg(theme.fg),
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
