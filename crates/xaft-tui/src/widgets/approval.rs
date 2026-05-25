//! Tool approval modal dialog widget.
//!
//! Shown as a centered overlay when `AppState::pending_approval` is `Some`.
//! Blocks all other input until the user approves or rejects.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use crate::state::{AppState, PendingApprovalState};
use crate::theme::Theme;

/// Centered approval modal dialog.
pub struct ApprovalWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
}

impl<'a> ApprovalWidget<'a> {
    pub fn new(state: &'a AppState, theme: &'a Theme) -> Self {
        Self { state, theme }
    }

    /// Returns true if the modal should be displayed.
    pub fn is_visible(state: &AppState) -> bool {
        state.pending_approval.is_some()
    }
}

impl Widget for ApprovalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let approval = match &self.state.pending_approval {
            Some(a) => a,
            None => return,
        };

        // Center the modal
        let modal_w = 60u16.min(area.width.saturating_sub(4));
        let modal_h = 12u16.min(area.height.saturating_sub(4));
        let x = area.x + (area.width.saturating_sub(modal_w)) / 2;
        let y = area.y + (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect::new(x, y, modal_w, modal_h);

        // Clear background
        Clear.render(modal_area, buf);

        let block = Block::default()
            .title(" ⚠  Approval Required ")
            .title_style(self.theme.warning().add_modifier(Modifier::BOLD))
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_style(self.theme.warning())
            .style(self.theme.modal());

        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        if inner.height < 4 || inner.width < 10 {
            return;
        }

        // Sections inside the modal
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // tool name
                Constraint::Length(1), // separator
                Constraint::Min(2),    // input preview
                Constraint::Length(1), // separator
                Constraint::Length(3), // buttons
            ])
            .split(inner);

        // Tool name line
        Paragraph::new(Line::from(vec![
            Span::styled("Tool: ", self.theme.dim()),
            Span::styled(
                approval.tool_name.clone(),
                self.theme.tool().add_modifier(Modifier::BOLD),
            ),
        ]))
        .render(sections[0], buf);

        // Separator
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            self.theme.border(),
        )))
        .render(sections[1], buf);

        // Input preview
        let preview = format_approval_input(approval, (inner.width as usize).saturating_sub(2));
        Paragraph::new(
            preview
                .lines()
                .map(|l| Line::from(Span::styled(l.to_string(), self.theme.modal())))
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: true })
        .render(sections[2], buf);

        // Separator
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            self.theme.border(),
        )))
        .render(sections[3], buf);

        // Buttons — render side by side
        let button_area = sections[4];
        let buttons = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(button_area);

        let approve_style = if self.state.approval_focused_approve {
            self.theme
                .success()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            self.theme.success()
        };
        let reject_style = if !self.state.approval_focused_approve {
            self.theme
                .error()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            self.theme.error()
        };

        Paragraph::new(Line::from(Span::styled("  [Y] Approve  ", approve_style)))
            .alignment(Alignment::Center)
            .render(buttons[0], buf);

        Paragraph::new(Line::from(Span::styled("  [N] Reject   ", reject_style)))
            .alignment(Alignment::Center)
            .render(buttons[1], buf);
    }
}

fn format_approval_input(approval: &PendingApprovalState, max_width: usize) -> String {
    let mut lines = Vec::new();

    match &approval.input {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter().take(5) {
                let val_str = match v {
                    serde_json::Value::String(s) => {
                        if s.len() > max_width.saturating_sub(k.len() + 3) {
                            format!("{}…", &s[..max_width.saturating_sub(k.len() + 4).max(4)])
                        } else {
                            s.clone()
                        }
                    }
                    other => other.to_string(),
                };
                lines.push(format!("{k}: {val_str}"));
            }
        }
        other => {
            let s = other.to_string();
            if s.len() > max_width {
                lines.push(format!("{}…", &s[..max_width.saturating_sub(1).max(1)]));
            } else {
                lines.push(s);
            }
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, PendingApprovalState};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::time::Instant;

    fn make_buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    fn make_approval() -> PendingApprovalState {
        PendingApprovalState {
            agent_run_id: "run-1".into(),
            tool_name: "bash_exec".into(),
            tool_use_id: "tid-1".into(),
            input: serde_json::json!({"command": "ls -la /etc"}),
            input_preview: "command=ls -la /etc".into(),
            arrived_at: Instant::now(),
        }
    }

    #[test]
    fn is_visible_without_approval() {
        let state = AppState::new("test");
        assert!(!ApprovalWidget::is_visible(&state));
    }

    #[test]
    fn is_visible_with_approval() {
        let mut state = AppState::new("test");
        state.pending_approval = Some(make_approval());
        assert!(ApprovalWidget::is_visible(&state));
    }

    #[test]
    fn renders_without_panic() {
        let mut state = AppState::new("test");
        state.pending_approval = Some(make_approval());
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&state, &theme);
        let mut buf = make_buf(80, 24);
        widget.render(Rect::new(0, 0, 80, 24), &mut buf);
    }

    #[test]
    fn renders_no_approval_no_crash() {
        let state = AppState::new("test");
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&state, &theme);
        let mut buf = make_buf(80, 24);
        widget.render(Rect::new(0, 0, 80, 24), &mut buf);
    }

    #[test]
    fn format_approval_input_object() {
        let approval = make_approval();
        let result = format_approval_input(&approval, 60);
        assert!(result.contains("command"));
        assert!(result.contains("ls -la"));
    }
}
