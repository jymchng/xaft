//! Tool activity log sidebar widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Widget},
};

use crate::state::{AppState, ToolEntryState};
use crate::theme::Theme;

/// Sidebar showing recent tool activity + token/cost stats.
pub struct ToolLogWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> ToolLogWidget<'a> {
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for ToolLogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        let block = Block::default()
            .title(" Tools ")
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

        if inner.height < 2 || inner.width < 4 {
            return;
        }

        // Show stats header + tool entries
        let max_tools = (inner.height as usize).saturating_sub(4); // reserve space for stats
        let tool_count = self.state.tool_log.len();
        let start = tool_count.saturating_sub(max_tools);

        let mut items: Vec<ListItem> = Vec::new();

        // Stats section
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Tokens: ", self.theme.dim()),
            Span::styled(format_tokens(self.state.total_tokens()), self.theme.base()),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Cost:   ", self.theme.dim()),
            Span::styled(
                format!("${:.4}", self.state.total_cost_usd),
                self.theme.base(),
            ),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Calls:  ", self.theme.dim()),
            Span::styled(self.state.total_llm_calls.to_string(), self.theme.base()),
        ])));
        items.push(ListItem::new(Line::from(Span::styled(
            "─".repeat(inner.width.saturating_sub(2) as usize),
            self.theme.border(),
        ))));

        // Tool entries
        for entry in self.state.tool_log.range(start..) {
            let icon = match entry.state {
                ToolEntryState::Running => {
                    let frames = ['⠋', '⠙', '⠹', '⠸'];
                    let i = (self.state.tick as usize / 2) % frames.len();
                    frames[i].to_string()
                }
                ToolEntryState::Done => "✓".to_string(),
                ToolEntryState::Failed => "✗".to_string(),
            };

            let icon_style = match entry.state {
                ToolEntryState::Running => Style::default().fg(self.theme.warning),
                ToolEntryState::Done => self.theme.success(),
                ToolEntryState::Failed => self.theme.error(),
            };

            let duration = entry
                .duration_ms
                .map(|d| format!(" {d:.0}ms"))
                .unwrap_or_default();

            // Truncate tool name + preview to fit width
            let avail = inner.width.saturating_sub(6) as usize;
            let name_preview = format!("{} {}", entry.name, entry.input_preview);
            let truncated = if name_preview.len() > avail {
                format!("{}…", &name_preview[..avail.saturating_sub(1)])
            } else {
                name_preview
            };

            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(truncated, self.theme.tool()),
                Span::styled(duration, self.theme.dim()),
            ])));
        }

        // Active tool spinner if present
        if let Some(ref at) = self.state.active_tool {
            let elapsed = at.started_at.elapsed().as_millis();
            let avail = inner.width.saturating_sub(6) as usize;
            let name = if at.name.len() > avail {
                format!("{}…", &at.name[..avail.saturating_sub(1)])
            } else {
                at.name.clone()
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", self.state.spinner_char()),
                    Style::default().fg(self.theme.accent),
                ),
                Span::styled(name, self.theme.tool()),
                Span::styled(format!(" {elapsed}ms"), self.theme.dim()),
            ])));
        }

        List::new(items).style(self.theme.base()).render(inner, buf);
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1500), "1.5k");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }
}
