//! Full-featured approval dialog widget.
//!
//! Renders three distinct views based on `ApprovalQueue` state:
//!
//! 1. **Single modal** — one pending approval, with risk gauge and tool-specific preview
//! 2. **Batch list** — multiple pending approvals in a compact list
//! 3. **History** — session approval log with stats
//!
//! All views are centered overlays drawn on top of the main layout.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget, Wrap},
};

use crate::approval::{ApprovalDecision, ApprovalQueue, RiskLevel, tool_preview_lines};
use crate::theme::Theme;

// ── Overlay geometry ──────────────────────────────────────────────────────────

/// Compute a centered overlay rect for the approval dialog.
pub fn approval_overlay_rect(terminal: Rect, content_height: u16) -> Rect {
    // Width: prefer 80 cols but never exceed terminal width.
    let width = terminal
        .width
        .saturating_sub(4)
        .min(80)
        .max(1)
        .min(terminal.width);
    // Height: never exceed terminal height.
    let height = content_height
        .min(terminal.height.saturating_sub(4).max(1))
        .min(terminal.height);
    let x = (terminal.width.saturating_sub(width)) / 2;
    let y = (terminal.height.saturating_sub(height)) / 2;
    Rect::new(x + terminal.x, y + terminal.y, width, height)
}

// ── Main widget ───────────────────────────────────────────────────────────────

/// Approval overlay — reads `ApprovalQueue` from `AppState`.
pub struct ApprovalWidget<'a> {
    pub queue: &'a ApprovalQueue,
    pub theme: &'a Theme,
}

impl<'a> ApprovalWidget<'a> {
    pub fn new(queue: &'a ApprovalQueue, theme: &'a Theme) -> Self {
        Self { queue, theme }
    }

    /// True if any overlay should be shown.
    pub fn is_visible(queue: &ApprovalQueue) -> bool {
        queue.has_pending() || queue.show_history
    }
}

impl Widget for ApprovalWidget<'_> {
    fn render(self, terminal_area: Rect, buf: &mut Buffer) {
        if self.queue.show_history {
            render_history(self.queue, terminal_area, buf, self.theme);
            return;
        }

        if !self.queue.has_pending() {
            return;
        }

        if self.queue.pending.len() > 1 {
            render_batch_list(self.queue, terminal_area, buf, self.theme);
        } else {
            render_single_modal(self.queue, terminal_area, buf, self.theme);
        }
    }
}

// ── Single modal ──────────────────────────────────────────────────────────────

fn render_single_modal(queue: &ApprovalQueue, terminal: Rect, buf: &mut Buffer, theme: &Theme) {
    let item = match queue.focused() {
        Some(i) => i,
        None => return,
    };

    let preview_lines = tool_preview_lines(&item.tool_name, &item.input);
    let content_height = 4 // header + risk + blank + footer
        + preview_lines.len() as u16
        + 3; // padding

    let area = approval_overlay_rect(terminal, content_height.max(12));

    // Clear background
    Clear.render(area, buf);

    let risk_color = risk_style_color(item.risk);
    let title = format!(
        " ⚠  {} — {} ",
        if item.risk >= RiskLevel::High {
            "⚡ APPROVAL REQUIRED"
        } else {
            "Approval Required"
        },
        item.tool_name
    );

    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .title_style(Style::default().fg(risk_color).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(risk_color))
        .style(Style::default().fg(theme.fg).bg(theme.modal_bg));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height < 4 {
        return;
    }

    // Split: body + footer hint
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1), // risk gauge
            Constraint::Length(1), // blank
            Constraint::Length(1), // keybindings
        ])
        .split(inner);

    // Body: preview lines
    let preview: Vec<Line> = preview_lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(theme.fg))))
        .collect();
    Paragraph::new(preview)
        .wrap(Wrap { trim: false })
        .render(chunks[0], buf);

    // Risk gauge
    render_risk_gauge(item.risk, chunks[1], buf, theme);

    // Keybindings footer
    let hint = " [a]pprove  [r]eject  [s]kip  [A]ll  [R]ej.all  [h]istory ";
    let hint_style = Style::default()
        .fg(theme.dim)
        .add_modifier(Modifier::ITALIC);
    Paragraph::new(hint)
        .alignment(Alignment::Center)
        .style(hint_style)
        .render(chunks[3], buf);
}

// ── Risk gauge ────────────────────────────────────────────────────────────────

fn render_risk_gauge(risk: RiskLevel, area: Rect, buf: &mut Buffer, theme: &Theme) {
    if area.width < 20 {
        return;
    }
    let color = risk_style_color(risk);
    let filled = risk.gauge_blocks();
    let empty = 8usize.saturating_sub(filled);

    let mut spans = vec![
        Span::styled("  Risk: ", Style::default().fg(theme.dim)),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("  {}", risk.label()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];

    Paragraph::new(Line::from(spans.drain(..).collect::<Vec<_>>())).render(area, buf);
}

fn risk_style_color(risk: RiskLevel) -> Color {
    match risk {
        RiskLevel::Low => Color::Green,
        RiskLevel::Medium => Color::Yellow,
        RiskLevel::High => Color::Red,
        RiskLevel::Critical => Color::Rgb(255, 50, 50),
    }
}

// ── Batch list ────────────────────────────────────────────────────────────────

fn render_batch_list(queue: &ApprovalQueue, terminal: Rect, buf: &mut Buffer, theme: &Theme) {
    let n = queue.pending.len();
    let content_height = (n as u16 + 5).min(20);
    let area = approval_overlay_rect(terminal, content_height);

    Clear.render(area, buf);

    let title = format!(" ⚠  {} PENDING APPROVALS ", n);
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().fg(theme.fg).bg(theme.modal_bg));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    // Item list
    let items: Vec<ListItem> = queue
        .pending
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let focused = i == queue.focused_idx;
            let prefix = if focused { "▶ " } else { "  " };
            let risk_col = risk_style_color(a.risk);
            let line = Line::from(vec![
                Span::styled(
                    format!("{prefix}{:2}. ", i + 1),
                    Style::default().fg(if focused { theme.accent } else { theme.dim }),
                ),
                Span::styled(
                    format!("{:<14}", a.tool_name),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(
                    format!("{:>8} ", a.risk.label()),
                    Style::default().fg(risk_col),
                ),
                Span::styled(
                    a.input_preview.chars().take(25).collect::<String>(),
                    Style::default().fg(theme.dim),
                ),
            ]);
            if focused {
                ListItem::new(line).style(Style::default().bg(Color::Rgb(40, 40, 60)))
            } else {
                ListItem::new(line)
            }
        })
        .collect();

    List::new(items)
        .style(Style::default().fg(theme.fg))
        .render(chunks[0], buf);

    // Footer
    let hint = " [a]pprove  [r]eject  [A]ll≤Med  [R]ej.all  [↑↓]nav  [h]istory ";
    Paragraph::new(hint)
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )
        .render(chunks[1], buf);
}

// ── History view ──────────────────────────────────────────────────────────────

fn render_history(queue: &ApprovalQueue, terminal: Rect, buf: &mut Buffer, theme: &Theme) {
    let content_height = (queue.history.len() as u16 + 7).min(25);
    let area = approval_overlay_rect(terminal, content_height);

    Clear.render(area, buf);

    let total = queue.total_approved + queue.total_rejected + queue.total_auto;
    let title = format!(" Approval History ({} decisions) ", total);
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .style(Style::default().fg(theme.fg).bg(theme.modal_bg));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height < 4 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header row
            Constraint::Min(3),    // rows
            Constraint::Length(1), // summary
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Header
    Line::from(vec![
        Span::styled(" #   ", Style::default().fg(theme.dim)),
        Span::styled(
            format!("{:<12}", "Tool"),
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>8}", "Risk"),
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>8}", "Result"),
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
    ])
    .render(chunks[0], buf);

    // Rows (newest first in display)
    let rows: Vec<ListItem> = queue
        .history
        .iter()
        .rev()
        .enumerate()
        .take(chunks[1].height as usize)
        .map(|(i, rec)| {
            let (result_text, result_color) = match &rec.decision {
                ApprovalDecision::Approved => ("✓ User", Color::Green),
                ApprovalDecision::Rejected => ("✗ User", Color::Red),
                ApprovalDecision::AutoApproved { .. } => ("✓ Auto", Color::Cyan),
                ApprovalDecision::Skipped => ("○ Skip", Color::DarkGray),
            };
            let age_secs = rec.decided_at.elapsed().as_secs();
            let age = ApprovalRecord_format_age(age_secs);
            let row = Line::from(vec![
                Span::styled(
                    format!("{:>2}. ", queue.history.len() - i),
                    Style::default().fg(theme.dim),
                ),
                Span::styled(
                    format!("{:<12}", rec.tool_name),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(
                    format!("{:>8} ", rec.risk.label()),
                    Style::default().fg(risk_style_color(rec.risk)),
                ),
                Span::styled(
                    format!("{:<8}", result_text),
                    Style::default().fg(result_color),
                ),
                Span::styled(format!("{:>4}", age), Style::default().fg(theme.dim)),
            ]);
            ListItem::new(row)
        })
        .collect();

    List::new(rows)
        .style(Style::default().fg(theme.fg))
        .render(chunks[1], buf);

    // Summary
    let summary = format!(
        " Approved: {} ({} auto)  Rejected: {}  Pending: {}",
        queue.total_approved,
        queue.total_auto,
        queue.total_rejected,
        queue.pending.len()
    );
    Paragraph::new(summary)
        .style(Style::default().fg(theme.dim))
        .render(chunks[2], buf);

    // Hint
    Paragraph::new(" [Esc/h] Close  [u] Undo last  [c] Clear history ")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )
        .render(chunks[3], buf);
}

fn ApprovalRecord_format_age(age_secs: u64) -> String {
    crate::approval::ApprovalRecord::format_age(age_secs)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalQueue, AutoApproveConfig, PendingApproval};
    use crate::theme::Theme;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn make_area() -> Rect {
        Rect::new(0, 0, 100, 30)
    }

    fn make_queue_with(tool: &str, risk_input: serde_json::Value) -> ApprovalQueue {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        q.pending
            .push_back(PendingApproval::new("tid", "rid", tool, risk_input));
        q
    }

    #[test]
    fn overlay_rect_centered() {
        let terminal = Rect::new(0, 0, 200, 60);
        let rect = approval_overlay_rect(terminal, 12);
        // Should be roughly centered
        assert!(rect.x > 0);
        assert!(rect.y > 0);
        assert!(rect.x < terminal.width / 2 + 5);
    }

    #[test]
    fn overlay_rect_clamps_to_terminal() {
        let terminal = Rect::new(0, 0, 20, 10);
        let rect = approval_overlay_rect(terminal, 30);
        assert!(rect.width <= terminal.width);
        assert!(rect.height <= terminal.height);
    }

    #[test]
    fn is_visible_no_pending() {
        let q = ApprovalQueue::new(AutoApproveConfig::default());
        assert!(!ApprovalWidget::is_visible(&q));
    }

    #[test]
    fn is_visible_with_pending() {
        let q = make_queue_with("write_file", serde_json::json!({"path": "a.rs"}));
        assert!(ApprovalWidget::is_visible(&q));
    }

    #[test]
    fn is_visible_history_mode() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        q.show_history = true;
        assert!(ApprovalWidget::is_visible(&q));
    }

    #[test]
    fn renders_single_modal_no_panic() {
        let q = make_queue_with(
            "bash_exec",
            serde_json::json!({"command": "cargo test", "working_dir": "/proj"}),
        );
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&q, &theme);
        let mut buf = Buffer::empty(make_area());
        widget.render(make_area(), &mut buf);
    }

    #[test]
    fn renders_batch_list_no_panic() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        for i in 0..4 {
            q.pending.push_back(PendingApproval::new(
                format!("t{i}"),
                "r1",
                "write_file",
                serde_json::json!({"path": format!("a{i}.rs")}),
            ));
        }
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&q, &theme);
        let mut buf = Buffer::empty(make_area());
        widget.render(make_area(), &mut buf);
    }

    #[test]
    fn renders_history_no_panic() {
        let mut q = ApprovalQueue::new(AutoApproveConfig::default());
        q.show_history = true;
        q.pending.push_back(PendingApproval::new(
            "t1",
            "r1",
            "read_file",
            serde_json::json!({"path": "a.rs"}),
        ));
        q.resolve_focused(ApprovalDecision::Approved);
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&q, &theme);
        let mut buf = Buffer::empty(make_area());
        widget.render(make_area(), &mut buf);
    }

    #[test]
    fn renders_tiny_area_no_panic() {
        let q = make_queue_with("write_file", serde_json::json!({"path": "a.rs"}));
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&q, &theme);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        widget.render(Rect::new(0, 0, 10, 4), &mut buf);
    }

    #[test]
    fn renders_empty_queue_no_panic() {
        let q = ApprovalQueue::new(AutoApproveConfig::default());
        let theme = Theme::dark();
        let widget = ApprovalWidget::new(&q, &theme);
        let mut buf = Buffer::empty(make_area());
        widget.render(make_area(), &mut buf);
    }

    #[test]
    fn risk_gauge_colors_differ() {
        // Low = Green, Critical = near-red — ensure they're different
        let low_color = risk_style_color(RiskLevel::Low);
        let crit_color = risk_style_color(RiskLevel::Critical);
        assert_ne!(low_color, crit_color);
    }
}
