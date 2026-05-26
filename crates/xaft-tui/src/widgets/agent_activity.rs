//! Agent activity widget — visualises per-agent status, current tool, and recent
//! tool-call history using data from `AgentTracker`.
//!
//! Rendered layout (example):
//!
//! ```text
//! ┌─ Agent Activity ─────────────────────────────────────────────┐
//! │ ⏳ planner      Thinking  2.1s                               │
//! │ ✓  coder        Done      8.4s                               │
//! │    └─ ✓ write_file   45ms                                    │
//! │    └─ ✓ edit_file    12ms                                     │
//! │ ⏳ qa           Thinking  0.8s                               │
//! │    └─ ⚙ read_file    in progress...                          │
//! │ ○  fixer        Idle                                         │
//! │ ─────────────────────────────────────────────────────────────│
//! │ 3 agents │ 2 active │ 1 done │ 18.4s                        │
//! └──────────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Widget},
};

use crate::state::AppState;
use crate::theme::Theme;

/// Pane showing per-agent status and recent tool-call history.
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

    /// Returns the braille spinner frame for `tick`.
    fn spinner(tick: u64) -> char {
        const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        FRAMES[(tick as usize / 3) % FRAMES.len()]
    }

    /// Format a duration in seconds as a compact string: `"2.1s"` or `"42ms"`.
    fn fmt_secs(secs: f64) -> String {
        if secs >= 1.0 {
            format!("{secs:.1}s")
        } else {
            format!("{:.0}ms", secs * 1000.0)
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

        let tracker = &self.state.agent_tracker;

        // Dynamic title: "Agent Activity" or show active count when busy
        let active = tracker.active_count();
        let title = if active > 0 {
            format!(" {} Agent Activity ", Self::spinner(self.state.tick))
        } else {
            " Agent Activity ".to_string()
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

        // ── Build list items ──────────────────────────────────────────────────

        let mut items: Vec<ListItem> = Vec::new();

        if tracker.nodes.is_empty() {
            // Placeholder when no agents have appeared yet
            items.push(ListItem::new(Line::from(Span::styled(
                "No agents yet",
                self.theme.dim(),
            ))));
        } else {
            // Footer takes 2 rows (separator + stats); cap agent rows to remainder
            let footer_rows = 2usize;
            let agent_rows_available = available.saturating_sub(footer_rows).max(1);
            let mut rows_used = 0usize;

            'agents: for node in tracker.agents_in_order() {
                if rows_used >= agent_rows_available {
                    break;
                }

                // ── Agent status row ──────────────────────────────────────────
                let status_dur = node.status_duration().as_secs_f64();
                let dur_str = Self::fmt_secs(status_dur);

                let icon = node.status.icon();
                let icon_color = node.status.color();
                let status_label = node.status.label();

                // Pad name to 12 chars for alignment
                let name_padded = format!("{:<12}", node.name);
                // Pad status label to 10 chars
                let status_padded = format!("{:<10}", status_label);

                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{icon} "),
                        Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(name_padded, self.theme.agent()),
                    Span::styled(status_padded, self.theme.dim()),
                    Span::styled(dur_str, self.theme.dim()),
                ])));
                rows_used += 1;

                if rows_used >= agent_rows_available {
                    break 'agents;
                }

                // ── Tool history sub-rows (most recent 3, newest last) ────────
                // Show at most 3 sub-rows, but only if we have space
                let history: Vec<_> = node.tool_history.iter().rev().take(3).collect();
                // Reverse to show oldest-of-three first (chronological within agent)
                let history_chrono: Vec<_> = history.into_iter().rev().collect();

                for tool in history_chrono {
                    if rows_used >= agent_rows_available {
                        break;
                    }

                    let (tool_icon, tool_icon_style) = if tool.success.is_none() {
                        // Still running — animated spinner
                        let c = Self::spinner(self.state.tick);
                        (
                            c.to_string(),
                            Style::default()
                                .fg(self.theme.warning)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else if tool.success == Some(true) {
                        ("✓".to_string(), self.theme.success())
                    } else {
                        ("✗".to_string(), self.theme.error())
                    };

                    let duration_part = if tool.success.is_none() {
                        "in progress...".to_string()
                    } else {
                        tool.elapsed_str()
                    };

                    // Truncate tool name to fit
                    let avail_name = (inner.width as usize).saturating_sub(20);
                    let tool_name = if tool.tool_name.len() > avail_name && avail_name > 1 {
                        format!("{}…", &tool.tool_name[..avail_name.saturating_sub(1)])
                    } else {
                        tool.tool_name.clone()
                    };

                    items.push(ListItem::new(Line::from(vec![
                        Span::styled("   └─ ", self.theme.dim()),
                        Span::styled(tool_icon, tool_icon_style),
                        Span::raw(" "),
                        Span::styled(tool_name, self.theme.tool()),
                        Span::raw("   "),
                        Span::styled(duration_part, self.theme.dim()),
                    ])));
                    rows_used += 1;
                }
            }

            // ── Footer separator + stats ──────────────────────────────────────
            if available > rows_used + 1 {
                // Separator line
                let sep = "─".repeat(inner.width as usize);
                items.push(ListItem::new(Line::from(Span::styled(
                    sep,
                    self.theme.dim(),
                ))));

                // Stats line
                let total_agents = tracker.nodes.len();
                let done = tracker.done_count();
                let elapsed = tracker.total_elapsed().as_secs_f64();
                let elapsed_str = Self::fmt_secs(elapsed);

                let stats = format!(
                    "{total_agents} agents │ {active} active │ {done} done │ {elapsed_str}"
                );
                items.push(ListItem::new(Line::from(Span::styled(
                    stats,
                    self.theme.dim(),
                ))));
            }
        }

        // Render visible rows (trim to available height)
        let visible: Vec<ListItem> = items.into_iter().take(available).collect();
        List::new(visible)
            .style(self.theme.base())
            .render(inner, buf);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn make_state() -> AppState {
        AppState::new("test")
    }

    #[test]
    fn renders_without_panic_empty() {
        let state = make_state();
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        widget.render(Rect::new(0, 0, 40, 20), &mut buf);
    }

    #[test]
    fn renders_with_active_agents_without_panic() {
        let mut state = make_state();
        state.agent_tracker.on_llm_start("coder");
        state
            .agent_tracker
            .on_tool_start("coder", "read_file", "t1", "src/main.rs");
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        widget.render(Rect::new(0, 0, 60, 20), &mut buf);
    }

    #[test]
    fn renders_tiny_area_without_panic() {
        let mut state = make_state();
        state.agent_tracker.on_llm_start("coder");
        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        // Minimum 3 rows
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        widget.render(Rect::new(0, 0, 6, 3), &mut buf);
    }

    #[test]
    fn renders_done_agent_with_checkmark() {
        let mut state = make_state();
        state.agent_tracker.on_llm_start("coder");
        state
            .agent_tracker
            .on_tool_start("coder", "write_file", "t2", "");
        state.agent_tracker.on_tool_complete("coder", "t2", true);
        state.agent_tracker.on_run_complete("coder");

        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        widget.render(Rect::new(0, 0, 60, 20), &mut buf);

        // Should render ✓ somewhere
        let has_check = (0..60u16).any(|x| (0..20u16).any(|y| buf[(x, y)].symbol() == "✓"));
        assert!(has_check, "should render ✓ for done agent or done tool");
    }

    #[test]
    fn renders_multiple_agents_without_panic() {
        let mut state = make_state();
        state.agent_tracker.on_llm_start("planner");
        state.agent_tracker.on_llm_start("coder");
        state.agent_tracker.on_run_complete("planner");
        state
            .agent_tracker
            .on_tool_start("coder", "write_file", "t1", "");

        let theme = Theme::dark();
        let widget = AgentActivityWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 25));
        widget.render(Rect::new(0, 0, 80, 25), &mut buf);
    }

    #[test]
    fn spinner_rotates() {
        // Verify spinner doesn't panic and cycles through characters
        for i in 0..30u64 {
            let c = AgentActivityWidget::spinner(i);
            assert!(['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'].contains(&c));
        }
    }

    #[test]
    fn fmt_secs_formats_correctly() {
        assert_eq!(AgentActivityWidget::fmt_secs(0.042), "42ms");
        assert_eq!(AgentActivityWidget::fmt_secs(1.5), "1.5s");
        assert_eq!(AgentActivityWidget::fmt_secs(0.0), "0ms");
    }
}
