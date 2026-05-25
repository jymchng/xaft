//! Agent activity widget — shows active tool spinner + recent tool log entries.
//!
//! This is the primary widget for the `AgentActivity` pane, replacing
//! `ToolLogWidget` for that specific pane slot while keeping `ToolLogWidget`
//! available for backward compatibility.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Widget},
};

use crate::state::{AppState, ToolEntryState};
use crate::theme::Theme;

/// Pane showing active tool spinner and recent tool call history.
///
/// Designed to be compact and glanceable: each row is one tool call with
/// status icon, name, and elapsed duration.
pub struct AgentActivityWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> AgentActivityWidget<'a> {
    /// Construct the widget for the given state and theme.
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for AgentActivityWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        // Title shows active tool name if any
        let title = if let Some(ref at) = self.state.active_tool {
            let elapsed = at.started_at.elapsed().as_millis();
            format!(" {} {} {}ms ", self.state.spinner_char(), at.name, elapsed)
        } else {
            " Tools ".to_string()
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

        if inner.height < 1 || inner.width < 4 {
            return;
        }

        let available = inner.height as usize;

        // Show tool entries newest-first (reversed), up to available lines
        let tool_count = self.state.tool_log.len();
        let items_to_show = available.min(tool_count);
        let start = tool_count.saturating_sub(items_to_show);

        // Collect in reverse order (newest first)
        let entries: Vec<_> = self.state.tool_log.range(start..).rev().collect();

        let mut items: Vec<ListItem> = Vec::with_capacity(items_to_show);

        for entry in entries.into_iter().take(available) {
            let (icon, icon_style) = match entry.state {
                ToolEntryState::Running => {
                    let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
                    let i = (self.state.tick as usize / 2) % frames.len();
                    (
                        frames[i].to_string(),
                        Style::default()
                            .fg(self.theme.warning)
                            .add_modifier(Modifier::BOLD),
                    )
                }
                ToolEntryState::Done => ("✓".to_string(), self.theme.success()),
                ToolEntryState::Failed => ("✗".to_string(), self.theme.error()),
            };

            let duration = entry
                .duration_ms
                .map(|d| {
                    if d >= 1000.0 {
                        format!(" {:.1}s", d / 1000.0)
                    } else {
                        format!(" {:.0}ms", d)
                    }
                })
                .unwrap_or_default();

            // Truncate tool name to fit, leaving room for icon + duration
            let dur_len = duration.len();
            let avail_name = (inner.width as usize).saturating_sub(3 + dur_len); // "X " prefix + duration
            let name = if entry.name.len() > avail_name && avail_name > 1 {
                format!("{}…", &entry.name[..avail_name.saturating_sub(1)])
            } else {
                entry.name.clone()
            };

            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(name, self.theme.tool()),
                Span::styled(duration, self.theme.dim()),
            ])));
        }

        // If nothing yet, show placeholder
        if items.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                "No tool calls yet",
                self.theme.dim(),
            ))));
        }

        List::new(items).style(self.theme.base()).render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, ToolEntry, ToolEntryState};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::time::Instant;

    fn make_state_with_tools() -> AppState {
        let mut state = AppState::new("test");
        state.tool_log.push_back(ToolEntry {
            name: "read_file".into(),
            tool_use_id: "t1".into(),
            input_preview: "src/main.rs".into(),
            state: ToolEntryState::Done,
            started_at: Instant::now(),
            duration_ms: Some(15.0),
        });
        state.tool_log.push_back(ToolEntry {
            name: "write_file".into(),
            tool_use_id: "t2".into(),
            input_preview: "src/lib.rs".into(),
            state: ToolEntryState::Running,
            started_at: Instant::now(),
            duration_ms: None,
        });
        state
    }

    #[test]
    fn renders_without_panic_empty() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        widget.render(Rect::new(0, 0, 40, 20), &mut buf);
    }

    #[test]
    fn renders_with_tools_without_panic() {
        let state = make_state_with_tools();
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        widget.render(Rect::new(0, 0, 40, 20), &mut buf);
    }

    #[test]
    fn renders_tiny_area_without_panic() {
        let state = make_state_with_tools();
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        widget.render(Rect::new(0, 0, 6, 3), &mut buf);
    }

    #[test]
    fn shows_done_checkmark() {
        let state = make_state_with_tools();
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        widget.render(Rect::new(0, 0, 40, 20), &mut buf);
        // Check that some cell contains '✓' (done icon)
        let has_checkmark = (0..40u16).any(|x| (0..20u16).any(|y| buf[(x, y)].symbol() == "✓"));
        assert!(has_checkmark, "should render ✓ for done tool");
    }
}
