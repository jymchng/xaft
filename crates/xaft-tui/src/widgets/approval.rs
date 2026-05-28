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

use crate::approval::{ApprovalDecision, ApprovalQueue, RiskLevel, ToolPreview};
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

    /// True if the approval overlay should be shown.
    ///
    /// Single pending approvals use inline text + key bindings (Section 7).
    /// The modal overlay is only shown for batch approvals (2+ pending) or
    /// the history view.
    pub fn is_visible(queue: &ApprovalQueue) -> bool {
        queue.pending.len() > 1 || queue.show_history
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

    let preview = ToolPreview::from_input(&item.tool_name, &item.input);
    let content_height = 3 // title spacing
        + preview.content_height()
        + 3; // gauge + blank + footer

    let area = approval_overlay_rect(terminal, content_height.max(14));
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

    if inner.height < 5 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // tool preview body
            Constraint::Length(1), // risk gauge
            Constraint::Length(1), // blank
            Constraint::Length(1), // keybindings footer
        ])
        .split(inner);

    // Rich tool-specific preview
    render_tool_preview(&preview, chunks[0], buf, theme);

    // Risk gauge
    render_risk_gauge(item.risk, chunks[1], buf, theme);

    // Keybindings footer — includes [e]dit and [v]iew per PRD
    let hint = " [a]pprove  [r]eject  [e]dit  [v]iew  [s]kip  [A]ll  [h]istory ";
    Paragraph::new(hint)
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )
        .render(chunks[3], buf);
}

// ── Tool preview renderers ────────────────────────────────────────────────────

/// Dispatch to the appropriate tool-specific preview renderer.
fn render_tool_preview(preview: &ToolPreview, area: Rect, buf: &mut Buffer, theme: &Theme) {
    match preview {
        ToolPreview::Bash {
            command,
            working_dir,
            timeout_secs,
        } => render_bash_preview(command, working_dir, *timeout_secs, area, buf, theme),
        ToolPreview::FileEdit {
            path,
            old_lines,
            new_lines,
        } => render_file_edit_preview(path, old_lines, new_lines, area, buf, theme),
        ToolPreview::FileWrite {
            path,
            content_preview,
            total_bytes,
            is_new,
        } => render_file_write_preview(
            path,
            content_preview,
            *total_bytes,
            *is_new,
            area,
            buf,
            theme,
        ),
        ToolPreview::FileRead { path } => render_file_read_preview(path, area, buf, theme),
        ToolPreview::WebFetch { url, method } => {
            render_web_fetch_preview(url, method, area, buf, theme)
        }
        ToolPreview::Generic { lines } => render_generic_preview(lines, area, buf, theme),
    }
}

/// Bash preview: boxed command + working dir + timeout.
fn render_bash_preview(
    command: &str,
    working_dir: &str,
    timeout_secs: Option<u64>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    if area.height < 2 {
        return;
    }
    let label_style = Style::default().fg(theme.dim);
    let val_style = Style::default().fg(theme.fg);
    let cmd_style = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);

    // Command label
    Paragraph::new(Line::from(Span::styled("  Command:", label_style)))
        .render(Rect::new(area.x, area.y, area.width, 1), buf);

    if area.height > 2 {
        // Boxed command
        let cmd_box = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 3);
        let cmd_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.modal_bg));
        let cmd_inner = cmd_block.inner(cmd_box);
        cmd_block.render(cmd_box, buf);
        Paragraph::new(Span::styled(command, cmd_style))
            .wrap(Wrap { trim: false })
            .render(cmd_inner, buf);
    }

    let y_off = 4u16;
    if area.height > y_off {
        Paragraph::new(Line::from(vec![
            Span::styled("  Working Dir: ", label_style),
            Span::styled(working_dir, val_style),
        ]))
        .render(Rect::new(area.x, area.y + y_off, area.width, 1), buf);
    }
    if area.height > y_off + 1 {
        if let Some(t) = timeout_secs {
            Paragraph::new(Line::from(vec![
                Span::styled("  Timeout:     ", label_style),
                Span::styled(format!("{t}s"), val_style),
            ]))
            .render(Rect::new(area.x, area.y + y_off + 1, area.width, 1), buf);
        }
    }
}

/// FileEdit preview: file path + inline diff (old=red, new=green).
fn render_file_edit_preview(
    path: &str,
    old_lines: &[String],
    new_lines: &[String],
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    if area.height < 2 {
        return;
    }
    let label_style = Style::default().fg(theme.dim);

    Paragraph::new(Line::from(vec![
        Span::styled("  File: ", label_style),
        Span::styled(path, Style::default().fg(theme.fg)),
    ]))
    .render(Rect::new(area.x, area.y, area.width, 1), buf);

    if area.height < 3 {
        return;
    }

    // Diff box
    let box_h = area.height.saturating_sub(2).min(12);
    let diff_box = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), box_h);
    let diff_block = Block::default()
        .title(Span::styled(" diff ", Style::default().fg(theme.dim)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.modal_bg));
    let diff_inner = diff_block.inner(diff_box);
    diff_block.render(diff_box, buf);

    let avail_lines = diff_inner.height as usize;
    let mut rows: Vec<Line> = Vec::new();

    // Show removed lines first, then added
    for (i, line) in old_lines.iter().take(avail_lines / 2 + 1).enumerate() {
        let ln = format!("{:>3} │- {}", i + 1, line);
        rows.push(Line::from(Span::styled(
            ln,
            Style::default().fg(theme.error),
        )));
    }
    for (i, line) in new_lines
        .iter()
        .take(avail_lines.saturating_sub(rows.len()))
        .enumerate()
    {
        let ln = format!("{:>3} │+ {}", i + 1, line);
        rows.push(Line::from(Span::styled(
            ln,
            Style::default().fg(theme.success),
        )));
    }

    Paragraph::new(rows)
        .wrap(Wrap { trim: false })
        .render(diff_inner, buf);
}

/// WriteFile preview: file path + first N lines with numbers + size.
fn render_file_write_preview(
    path: &str,
    content_preview: &[String],
    total_bytes: usize,
    is_new: bool,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    if area.height < 2 {
        return;
    }
    let label_style = Style::default().fg(theme.dim);
    let new_tag = if is_new { " (new file)" } else { "" };

    Paragraph::new(Line::from(vec![
        Span::styled("  File: ", label_style),
        Span::styled(path, Style::default().fg(theme.fg)),
        Span::styled(
            format!("  {total_bytes} bytes{new_tag}"),
            Style::default().fg(theme.dim),
        ),
    ]))
    .render(Rect::new(area.x, area.y, area.width, 1), buf);

    if area.height < 3 || content_preview.is_empty() {
        return;
    }

    let box_h = area.height.saturating_sub(2).min(10);
    let prev_box = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), box_h);
    let prev_block = Block::default()
        .title(Span::styled(" preview ", Style::default().fg(theme.dim)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.modal_bg));
    let prev_inner = prev_block.inner(prev_box);
    prev_block.render(prev_box, buf);

    let avail = prev_inner.height as usize;
    let rows: Vec<Line> = content_preview
        .iter()
        .take(avail)
        .enumerate()
        .map(|(i, l)| {
            Line::from(vec![
                Span::styled(format!("{:>3} │ ", i + 1), Style::default().fg(theme.dim)),
                Span::styled(l.clone(), Style::default().fg(theme.fg)),
            ])
        })
        .collect();

    Paragraph::new(rows).render(prev_inner, buf);
}

/// ReadFile preview: just the path.
fn render_file_read_preview(path: &str, area: Rect, buf: &mut Buffer, theme: &Theme) {
    Paragraph::new(Line::from(vec![
        Span::styled("  File: ", Style::default().fg(theme.dim)),
        Span::styled(path, Style::default().fg(theme.fg)),
    ]))
    .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

/// WebFetch preview: METHOD + URL.
fn render_web_fetch_preview(url: &str, method: &str, area: Rect, buf: &mut Buffer, theme: &Theme) {
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!("  {method} "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(url, Style::default().fg(theme.fg)),
    ]))
    .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

/// Generic JSON preview with simple syntax coloring.
fn render_generic_preview(lines: &[String], area: Rect, buf: &mut Buffer, theme: &Theme) {
    let rows: Vec<Line> = lines
        .iter()
        .take(area.height as usize)
        .map(|l| Line::from(highlight_json_line(l, theme)))
        .collect();
    Paragraph::new(rows)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

/// Very lightweight JSON line colorizer.
///
/// Applies heuristic coloring:
/// - Lines with `"key":` → key in cyan, value follows
/// - String values → green
/// - Numeric / bool / null tokens → yellow
fn highlight_json_line<'a>(line: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    // Fast path: empty or pure whitespace
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return vec![Span::raw(line)];
    }

    // Detect key: "key": pattern
    if let Some(colon_pos) = trimmed.find("\": ") {
        if trimmed.starts_with('"') {
            let indent_len = line.len() - trimmed.len();
            let key_end = colon_pos + 2; // includes `":`
            let rest = &trimmed[key_end + 1..]; // after `": `
            let val_style = if rest.starts_with('"') {
                Style::default().fg(Color::Green)
            } else if rest.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
                Style::default().fg(Color::Yellow)
            } else if matches!(rest, s if s.starts_with("true") || s.starts_with("false") || s.starts_with("null"))
            {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(theme.fg)
            };
            return vec![
                Span::raw(" ".repeat(indent_len)),
                Span::styled(&trimmed[..key_end], Style::default().fg(theme.accent)),
                Span::styled(format!(" {rest}"), val_style),
            ];
        }
    }

    // Fallback: dim for structural tokens, plain otherwise
    let style = if trimmed == "{"
        || trimmed == "}"
        || trimmed == "["
        || trimmed == "]"
        || trimmed == "{,"
        || trimmed == "},"
        || trimmed == "],"
    {
        Style::default().fg(theme.dim)
    } else {
        Style::default().fg(theme.fg)
    };
    vec![Span::styled(line, style)]
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
    fn is_visible_single_pending_hidden() {
        // Section 7: single pending uses inline text, not modal
        let q = make_queue_with("write_file", serde_json::json!({"path": "a.rs"}));
        assert!(!ApprovalWidget::is_visible(&q));
    }

    #[test]
    fn is_visible_batch_pending_shown() {
        let mut q = make_queue_with("bash_exec", serde_json::json!({"command": "ls"}));
        // Add a second pending item to trigger batch mode
        q.push(
            "tid-2",
            "run-2",
            "write_file",
            serde_json::json!({"path": "b.rs"}),
        );
        assert!(q.pending.len() > 1);
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

    // ── Tool preview rendering ─────────────────────────────────────────────────

    fn make_buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    fn buf_content(buf: &Buffer) -> String {
        buf.content.iter().map(|c| c.symbol().to_string()).collect()
    }

    #[test]
    fn render_bash_preview_shows_command() {
        use crate::approval::ToolPreview;
        use crate::theme::Theme;

        let preview = ToolPreview::Bash {
            command: "cargo build".to_string(),
            working_dir: "/project".to_string(),
            timeout_secs: Some(60),
        };
        let theme = Theme::dark();
        let mut buf = make_buf(60, 8);
        let area = Rect::new(0, 0, 60, 8);
        render_tool_preview(&preview, area, &mut buf, &theme);
        let content = buf_content(&buf);
        assert!(content.contains("cargo build"), "command not rendered");
    }

    #[test]
    fn render_file_edit_preview_shows_diff() {
        use crate::approval::ToolPreview;
        use crate::theme::Theme;

        let preview = ToolPreview::FileEdit {
            path: "src/main.rs".to_string(),
            old_lines: vec!["fn old() {}".to_string()],
            new_lines: vec!["fn new() {}".to_string()],
        };
        let theme = Theme::dark();
        let mut buf = make_buf(60, 10);
        let area = Rect::new(0, 0, 60, 10);
        render_tool_preview(&preview, area, &mut buf, &theme);
        let content = buf_content(&buf);
        assert!(content.contains("main.rs"), "path not rendered");
    }

    #[test]
    fn render_file_write_preview_shows_path_and_size() {
        use crate::approval::ToolPreview;
        use crate::theme::Theme;

        let preview = ToolPreview::FileWrite {
            path: "out.txt".to_string(),
            content_preview: vec!["line 1".to_string(), "line 2".to_string()],
            total_bytes: 100,
            is_new: true,
        };
        let theme = Theme::dark();
        let mut buf = make_buf(60, 8);
        let area = Rect::new(0, 0, 60, 8);
        render_tool_preview(&preview, area, &mut buf, &theme);
        let content = buf_content(&buf);
        assert!(content.contains("out.txt"), "path not rendered");
    }

    #[test]
    fn render_generic_preview_does_not_panic() {
        use crate::approval::ToolPreview;
        use crate::theme::Theme;

        let preview = ToolPreview::Generic {
            lines: vec![
                r#"{"key": "value","#.to_string(),
                r#" "num": 42"#.to_string(),
                r#"}"#.to_string(),
            ],
        };
        let theme = Theme::dark();
        let mut buf = make_buf(60, 6);
        let area = Rect::new(0, 0, 60, 6);
        render_tool_preview(&preview, area, &mut buf, &theme);
        // Just verify it doesn't panic
    }

    #[test]
    fn highlight_json_line_key_value_pair() {
        use crate::theme::Theme;
        let theme = Theme::dark();
        let spans = highlight_json_line(r#"  "command": "cargo test""#, &theme);
        assert!(!spans.is_empty());
        // At least one span should contain "command"
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("command"));
    }

    #[test]
    fn render_web_fetch_preview_shows_method_url() {
        use crate::approval::ToolPreview;
        use crate::theme::Theme;

        let preview = ToolPreview::WebFetch {
            url: "https://api.example.com/v1".to_string(),
            method: "POST".to_string(),
        };
        let theme = Theme::dark();
        let mut buf = make_buf(60, 4);
        let area = Rect::new(0, 0, 60, 4);
        render_tool_preview(&preview, area, &mut buf, &theme);
        let content = buf_content(&buf);
        assert!(content.contains("POST") || content.contains("api.example.com"));
    }
}
