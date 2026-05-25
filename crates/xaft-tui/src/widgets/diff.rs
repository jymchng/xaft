//! Full-featured diff viewer widget.
//!
//! Renders unified diffs with:
//! - Syntax-coloured `+`/`−`/`@@` lines
//! - Hunk-by-hunk navigation (n/N)
//! - Scroll within a file (j/k / PgUp/PgDn)
//! - Unified and side-by-side display modes (Tab)
//! - File index cycling (→/← arrow keys)
//!
//! The widget is driven by [`DiffViewerState`] owned by `AppState`.
//! State mutations happen through `DiffViewerState` methods; the widget
//! is read-only.

use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Widget, Wrap},
};

use crate::theme::Theme;

// ── Diff data model ────────────────────────────────────────────────────────────

/// A single line in a unified diff, classified by type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLineKind {
    /// A line added in the new version (`+`).
    Added,
    /// A line removed from the old version (`−`).
    Removed,
    /// A context line present in both versions (` `).
    Context,
    /// A hunk header (`@@`).
    HunkHeader,
    /// A file header (`+++`/`---`).
    FileHeader,
}

/// A parsed line from a unified diff.
#[derive(Debug, Clone)]
pub struct ParsedDiffLine {
    pub kind: DiffLineKind,
    /// The raw line content (including the `+`/`-`/` ` prefix).
    pub content: String,
    /// Old-file line number (None for added lines).
    pub old_line: Option<u32>,
    /// New-file line number (None for removed lines).
    pub new_line: Option<u32>,
}

/// A parsed hunk from a unified diff (`@@ ... @@` through next hunk/EOF).
#[derive(Debug, Clone)]
pub struct ParsedHunk {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<ParsedDiffLine>,
}

/// A fully parsed file diff.
#[derive(Debug, Clone)]
pub struct ParsedFileDiff {
    pub path: String,
    pub hunks: Vec<ParsedHunk>,
    /// Raw unified diff text (kept for display when parsing fails).
    pub raw: String,
}

// ── State ─────────────────────────────────────────────────────────────────────

/// Display mode for the diff viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// Standard unified view (left-to-right, `+`/`−`).
    Unified,
    /// Side-by-side view (old left, new right).
    SideBySide,
}

impl DiffMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Unified => Self::SideBySide,
            Self::SideBySide => Self::Unified,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unified => "unified",
            Self::SideBySide => "side-by-side",
        }
    }
}

/// Full state for the diff viewer pane.
///
/// Lives in `AppState` and is mutated by keyboard events.
/// Read by `DiffWidget` each frame.
#[derive(Debug, Clone)]
pub struct DiffViewerState {
    /// Parsed diffs, ordered by receipt time (newest last).
    pub diffs: Vec<ParsedFileDiff>,
    /// Index into `diffs` of the currently displayed file.
    pub current_file: usize,
    /// Index into the current file's hunk list.
    pub current_hunk: usize,
    /// Scroll offset within the displayed lines (0 = top).
    pub scroll: u16,
    /// Display mode.
    pub mode: DiffMode,
    /// Whether to show diff pane at all.
    pub visible: bool,
    /// Total lines added across all diffs this session.
    pub total_added: i64,
    /// Total lines removed across all diffs this session.
    pub total_removed: i64,
}

impl Default for DiffViewerState {
    fn default() -> Self {
        Self {
            diffs: Vec::new(),
            current_file: 0,
            current_hunk: 0,
            scroll: 0,
            mode: DiffMode::Unified,
            visible: false,
            total_added: 0,
            total_removed: 0,
        }
    }
}

impl DiffViewerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a raw unified diff for a file path.
    pub fn push_diff(&mut self, path: impl Into<String>, raw: impl Into<String>) {
        let path = path.into();
        let raw = raw.into();
        let parsed = parse_unified_diff(&path, &raw);
        // Update or append (overwrite same path if already present)
        if let Some(existing) = self.diffs.iter_mut().find(|d| d.path == path) {
            *existing = parsed;
        } else {
            self.diffs.push(parsed);
        }
        self.visible = !self.diffs.is_empty();
    }

    /// Ingest from a HashMap of path → raw diff.
    pub fn push_diffs(&mut self, diffs: &HashMap<String, String>, added: i64, removed: i64) {
        for (path, raw) in diffs {
            self.push_diff(path.clone(), raw.clone());
        }
        self.total_added += added;
        self.total_removed += removed;
        self.visible = !self.diffs.is_empty();
    }

    /// Whether any diffs are loaded.
    pub fn has_diffs(&self) -> bool {
        !self.diffs.is_empty()
    }

    /// Current file (if any).
    pub fn current_file_diff(&self) -> Option<&ParsedFileDiff> {
        self.diffs.get(self.current_file)
    }

    /// Navigate to the next file.
    pub fn next_file(&mut self) {
        if !self.diffs.is_empty() {
            self.current_file = (self.current_file + 1) % self.diffs.len();
            self.current_hunk = 0;
            self.scroll = 0;
        }
    }

    /// Navigate to the previous file.
    pub fn prev_file(&mut self) {
        if !self.diffs.is_empty() {
            self.current_file = self.current_file.saturating_sub(1);
            self.current_hunk = 0;
            self.scroll = 0;
        }
    }

    /// Navigate to the next hunk within the current file.
    pub fn next_hunk(&mut self) {
        if let Some(diff) = self.diffs.get(self.current_file) {
            if !diff.hunks.is_empty() {
                self.current_hunk = (self.current_hunk + 1) % diff.hunks.len();
                self.scroll = 0;
            }
        }
    }

    /// Navigate to the previous hunk within the current file.
    pub fn prev_hunk(&mut self) {
        if let Some(diff) = self.diffs.get(self.current_file) {
            if !diff.hunks.is_empty() {
                self.current_hunk = if self.current_hunk == 0 {
                    diff.hunks.len() - 1
                } else {
                    self.current_hunk - 1
                };
                self.scroll = 0;
            }
        }
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Toggle unified ↔ side-by-side.
    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggle();
        self.scroll = 0;
    }

    /// Summary string: "3 files +42/−18".
    pub fn summary(&self) -> String {
        if self.diffs.is_empty() {
            return "no changes".to_string();
        }
        format!(
            "{} file(s) +{}/−{}",
            self.diffs.len(),
            self.total_added,
            self.total_removed
        )
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a unified diff string into structured hunks.
pub fn parse_unified_diff(path: &str, raw: &str) -> ParsedFileDiff {
    let mut hunks: Vec<ParsedHunk> = Vec::new();
    let mut current_hunk: Option<ParsedHunk> = None;
    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for line in raw.lines() {
        if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                hunks.push(h);
            }
            let (os, ns) = parse_hunk_header(line);
            old_line = os;
            new_line = ns;
            current_hunk = Some(ParsedHunk {
                header: line.to_string(),
                old_start: os,
                new_start: ns,
                lines: Vec::new(),
            });
        } else if line.starts_with("+++") || line.starts_with("---") {
            if let Some(ref mut h) = current_hunk {
                h.lines.push(ParsedDiffLine {
                    kind: DiffLineKind::FileHeader,
                    content: line.to_string(),
                    old_line: None,
                    new_line: None,
                });
            }
        } else if let Some(ref mut hunk) = current_hunk {
            if line.starts_with('+') {
                hunk.lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Added,
                    content: line[1..].to_string(),
                    old_line: None,
                    new_line: Some(new_line),
                });
                new_line += 1;
            } else if line.starts_with('-') {
                hunk.lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Removed,
                    content: line[1..].to_string(),
                    old_line: Some(old_line),
                    new_line: None,
                });
                old_line += 1;
            } else {
                // Context line (` ` prefix or bare)
                let content = if line.starts_with(' ') {
                    line[1..].to_string()
                } else {
                    line.to_string()
                };
                hunk.lines.push(ParsedDiffLine {
                    kind: DiffLineKind::Context,
                    content,
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                });
                old_line += 1;
                new_line += 1;
            }
        }
    }
    if let Some(h) = current_hunk {
        hunks.push(h);
    }

    ParsedFileDiff {
        path: path.to_string(),
        hunks,
        raw: raw.to_string(),
    }
}

/// Parse `@@ -a,b +c,d @@` and return (old_start, new_start).
fn parse_hunk_header(header: &str) -> (u32, u32) {
    // Find numbers after `@@ -` and `+`
    let inner = header.trim_start_matches('@').trim().trim_start_matches('-');
    let mut parts = inner.split_whitespace();
    let old = parts
        .next()
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim_start_matches('-').parse().ok())
        .unwrap_or(1);
    let new_part = parts
        .next()
        .map(|s| s.trim_start_matches('+'))
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    (old, new_part)
}

// ── Widget ────────────────────────────────────────────────────────────────────

/// Diff viewer widget — read-only view into `DiffViewerState`.
pub struct DiffWidget<'a> {
    pub state: &'a DiffViewerState,
    pub theme: &'a Theme,
    pub focused: bool,
}

impl<'a> DiffWidget<'a> {
    pub fn new(state: &'a DiffViewerState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }

    /// Whether there are any diffs to display.
    pub fn has_diffs(state: &DiffViewerState) -> bool {
        state.has_diffs()
    }
}

impl Widget for DiffWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };

        if !self.state.has_diffs() {
            let block = Block::default()
                .title(" Diffs ")
                .borders(Borders::ALL)
                .border_style(border_style)
                .style(self.theme.base());
            let inner = block.inner(area);
            block.render(area, buf);
            Paragraph::new(Line::from(Span::styled(
                "No file changes yet.",
                self.theme.dim(),
            )))
            .render(inner, buf);
            return;
        }

        let diff = match self.state.current_file_diff() {
            Some(d) => d,
            None => return,
        };

        let n_files = self.state.diffs.len();
        let n_hunks = diff.hunks.len();
        let hunk_idx = self.state.current_hunk;

        // Title: "Diffs (2/5) src/main.rs  hunk 1/3  [unified]"
        let mode_label = self.state.mode.label();
        let title = format!(
            " Diffs ({}/{}) {}  h:{}/{}  [{}] ",
            self.state.current_file + 1,
            n_files,
            shorten_path(&diff.path, (area.width as usize).saturating_sub(35)),
            if n_hunks > 0 { hunk_idx + 1 } else { 0 },
            n_hunks,
            mode_label,
        );

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

        if inner.height < 3 || inner.width < 10 {
            return;
        }

        // Top: file list (2 lines) + hunk navigation hint (1 line) + divider
        let header_height = 2u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_height), Constraint::Min(2)])
            .split(inner);

        // File navigation list (compact)
        let file_items: Vec<ListItem> = self
            .state
            .diffs
            .iter()
            .enumerate()
            .take(header_height as usize)
            .map(|(i, d)| {
                let short = shorten_path(&d.path, (inner.width as usize).saturating_sub(4));
                if i == self.state.current_file {
                    ListItem::new(Line::from(Span::styled(
                        format!("▶ {short}"),
                        self.theme
                            .accent()
                            .add_modifier(Modifier::BOLD),
                    )))
                } else {
                    ListItem::new(Line::from(Span::styled(
                        format!("  {short}"),
                        self.theme.dim(),
                    )))
                }
            })
            .collect();
        ratatui::widgets::List::new(file_items)
            .style(self.theme.base())
            .render(chunks[0], buf);

        let diff_area = chunks[1];
        if diff_area.height == 0 {
            return;
        }

        match self.state.mode {
            DiffMode::Unified => {
                render_unified(diff, hunk_idx, self.state.scroll, diff_area, buf, self.theme);
            }
            DiffMode::SideBySide => {
                render_side_by_side(diff, hunk_idx, self.state.scroll, diff_area, buf, self.theme);
            }
        }

        // Footer hint bar
        let hint = format!(
            " n/N hunk  ←/→ file  j/k scroll  Tab {}→{} ",
            self.state.mode.label(),
            self.state.mode.toggle().label()
        );
        if diff_area.height >= 2 {
            let hint_y = diff_area.bottom().saturating_sub(1);
            let hint_style = self.theme.dim();
            let x = diff_area.left();
            let w = hint.len().min(diff_area.width as usize);
            buf.set_string(x, hint_y, &hint[..w], hint_style);
        }
    }
}

// ── Rendering helpers ─────────────────────────────────────────────────────────

fn render_unified(
    diff: &ParsedFileDiff,
    hunk_idx: usize,
    scroll: u16,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    // Collect all lines: current hunk (if any), then rest
    let lines: Vec<Line> = if diff.hunks.is_empty() {
        diff.raw
            .lines()
            .map(|l| raw_diff_line(l, theme))
            .collect()
    } else {
        let hunk = &diff.hunks[hunk_idx.min(diff.hunks.len() - 1)];
        let mut out = Vec::new();
        // Hunk header
        out.push(Line::from(Span::styled(
            hunk.header.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::ITALIC | Modifier::BOLD),
        )));
        // Lines
        for dl in &hunk.lines {
            out.push(format_diff_line(dl, theme));
        }
        out
    };

    let visible_height = area.height.saturating_sub(1) as usize; // reserve footer
    let scroll_usize = scroll as usize;
    let display: Vec<Line> = lines
        .into_iter()
        .skip(scroll_usize)
        .take(visible_height)
        .collect();

    Paragraph::new(display)
        .style(theme.base())
        .render(Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1)), buf);
}

fn render_side_by_side(
    diff: &ParsedFileDiff,
    hunk_idx: usize,
    scroll: u16,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    let half_w = area.width / 2;
    let left_area = Rect::new(area.x, area.y, half_w, area.height.saturating_sub(1));
    let right_area = Rect::new(area.x + half_w, area.y, half_w, area.height.saturating_sub(1));

    // Draw vertical divider
    for y in area.top()..area.bottom() {
        buf[(area.x + half_w, y)].set_symbol("│").set_style(theme.border());
    }

    if diff.hunks.is_empty() {
        Paragraph::new("(no hunks)")
            .style(theme.dim())
            .render(left_area, buf);
        return;
    }

    let hunk = &diff.hunks[hunk_idx.min(diff.hunks.len() - 1)];
    let scroll_usize = scroll as usize;

    // Split lines into old (context + removed) and new (context + added)
    let old_lines: Vec<Line> = hunk
        .lines
        .iter()
        .filter(|l| {
            matches!(
                l.kind,
                DiffLineKind::Context | DiffLineKind::Removed | DiffLineKind::FileHeader
            )
        })
        .skip(scroll_usize)
        .take(left_area.height as usize)
        .map(|l| format_diff_line(l, theme))
        .collect();

    let new_lines: Vec<Line> = hunk
        .lines
        .iter()
        .filter(|l| {
            matches!(
                l.kind,
                DiffLineKind::Context | DiffLineKind::Added | DiffLineKind::FileHeader
            )
        })
        .skip(scroll_usize)
        .take(right_area.height as usize)
        .map(|l| format_diff_line(l, theme))
        .collect();

    Paragraph::new(old_lines)
        .style(theme.base())
        .wrap(Wrap { trim: false })
        .render(left_area, buf);

    let right_inner = Rect::new(
        right_area.x + 1, // skip the divider char
        right_area.y,
        right_area.width.saturating_sub(1),
        right_area.height,
    );
    Paragraph::new(new_lines)
        .style(theme.base())
        .wrap(Wrap { trim: false })
        .render(right_inner, buf);
}

fn format_diff_line(dl: &ParsedDiffLine, theme: &Theme) -> Line<'static> {
    let (prefix, style) = match dl.kind {
        DiffLineKind::Added => (
            "+",
            Style::default().fg(Color::Rgb(
                theme.success.r(),
                theme.success.g(),
                theme.success.b(),
            )),
        ),
        DiffLineKind::Removed => (
            "−",
            Style::default().fg(Color::Rgb(
                theme.error.r(),
                theme.error.g(),
                theme.error.b(),
            )),
        ),
        DiffLineKind::Context => (" ", Style::default().fg(theme.fg)),
        DiffLineKind::HunkHeader => (
            "@@",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::ITALIC),
        ),
        DiffLineKind::FileHeader => (
            "~~",
            Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
        ),
    };

    let line_num = match (dl.old_line, dl.new_line) {
        (Some(o), Some(n)) => format!("{:4}│{:4} ", o, n),
        (Some(o), None) => format!("{:4}│     ", o),
        (None, Some(n)) => format!("    │{:4} ", n),
        (None, None) => "         ".to_string(),
    };

    Line::from(vec![
        Span::styled(line_num, Style::default().fg(theme.dim)),
        Span::styled(format!("{prefix} "), style.add_modifier(Modifier::BOLD)),
        Span::styled(dl.content.clone(), style),
    ])
}

fn raw_diff_line(line: &str, theme: &Theme) -> Line<'static> {
    let style = if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(theme.success)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(theme.error)
    } else if line.starts_with("@@") {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(theme.dim)
    };
    Line::from(Span::styled(line.to_string(), style))
}

fn shorten_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len || max_len < 5 {
        return path.to_string();
    }
    format!("…{}", &path[path.len().saturating_sub(max_len - 1)..])
}

// ── Color channel helpers ─────────────────────────────────────────────────────
// ratatui Color doesn't expose r/g/b directly; we have to match on Rgb variant.

trait ColorChannels {
    fn r(&self) -> u8;
    fn g(&self) -> u8;
    fn b(&self) -> u8;
}

impl ColorChannels for Color {
    fn r(&self) -> u8 {
        if let Color::Rgb(r, _, _) = self {
            *r
        } else {
            200
        }
    }
    fn g(&self) -> u8 {
        if let Color::Rgb(_, g, _) = self {
            *g
        } else {
            200
        }
    }
    fn b(&self) -> u8 {
        if let Color::Rgb(_, _, b) = self {
            *b
        } else {
            200
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    const SAMPLE_DIFF: &str = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"extra\");
 }
@@ -10,3 +11,3 @@
 fn other() {
-    old_call();
+    new_call();
 }
";

    // ── Parser tests ──────────────────────────────────────────────────────────

    #[test]
    fn parse_hunk_count() {
        let diff = parse_unified_diff("src/main.rs", SAMPLE_DIFF);
        assert_eq!(diff.hunks.len(), 2);
    }

    #[test]
    fn parse_hunk_line_kinds() {
        let diff = parse_unified_diff("src/main.rs", SAMPLE_DIFF);
        let h0 = &diff.hunks[0];
        let has_added = h0.lines.iter().any(|l| l.kind == DiffLineKind::Added);
        let has_removed = h0.lines.iter().any(|l| l.kind == DiffLineKind::Removed);
        let has_context = h0.lines.iter().any(|l| l.kind == DiffLineKind::Context);
        assert!(has_added, "hunk should have Added lines");
        assert!(has_removed, "hunk should have Removed lines");
        assert!(has_context, "hunk should have Context lines");
    }

    #[test]
    fn parse_hunk_header_values() {
        let (old, new) = parse_hunk_header("@@ -1,5 +1,6 @@ fn main()");
        assert_eq!(old, 1);
        assert_eq!(new, 1);
    }

    #[test]
    fn parse_empty_diff_no_hunks() {
        let diff = parse_unified_diff("a.rs", "");
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn parse_no_context_lines_diff() {
        let d = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        let diff = parse_unified_diff("x", d);
        assert_eq!(diff.hunks.len(), 1);
        let h = &diff.hunks[0];
        assert!(h.lines.iter().any(|l| l.kind == DiffLineKind::Added));
        assert!(h.lines.iter().any(|l| l.kind == DiffLineKind::Removed));
    }

    // ── State tests ───────────────────────────────────────────────────────────

    #[test]
    fn state_starts_empty() {
        let s = DiffViewerState::new();
        assert!(!s.has_diffs());
        assert_eq!(s.mode, DiffMode::Unified);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn push_diff_makes_visible() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+fn new(){}");
        assert!(s.has_diffs());
        assert!(s.visible);
    }

    #[test]
    fn push_same_path_overwrites() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+first");
        s.push_diff("a.rs", "+second");
        assert_eq!(s.diffs.len(), 1);
        assert!(s.diffs[0].raw.contains("+second"));
    }

    #[test]
    fn push_diffs_accumulates_stats() {
        let mut s = DiffViewerState::new();
        let mut diffs = HashMap::new();
        diffs.insert("a.rs".to_string(), SAMPLE_DIFF.to_string());
        s.push_diffs(&diffs, 5, 3);
        assert_eq!(s.total_added, 5);
        assert_eq!(s.total_removed, 3);
        s.push_diffs(&diffs, 2, 1);
        assert_eq!(s.total_added, 7);
        assert_eq!(s.total_removed, 4);
    }

    #[test]
    fn next_file_wraps() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+a");
        s.push_diff("b.rs", "+b");
        s.push_diff("c.rs", "+c");
        s.current_file = 2;
        s.next_file();
        assert_eq!(s.current_file, 0); // wraps
    }

    #[test]
    fn prev_file_clamps_at_zero() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+a");
        s.current_file = 0;
        s.prev_file();
        assert_eq!(s.current_file, 0);
    }

    #[test]
    fn hunk_navigation() {
        let mut s = DiffViewerState::new();
        s.push_diff("main.rs", SAMPLE_DIFF);
        assert_eq!(s.current_hunk, 0);
        s.next_hunk();
        assert_eq!(s.current_hunk, 1);
        s.next_hunk();
        assert_eq!(s.current_hunk, 0); // wraps
        s.prev_hunk();
        assert_eq!(s.current_hunk, 1); // wraps back
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut s = DiffViewerState::new();
        s.scroll = 5;
        s.scroll_up(10);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_down_increments() {
        let mut s = DiffViewerState::new();
        s.scroll_down(3);
        assert_eq!(s.scroll, 3);
        s.scroll_down(7);
        assert_eq!(s.scroll, 10);
    }

    #[test]
    fn toggle_mode_unified_to_side_by_side() {
        let mut s = DiffViewerState::new();
        assert_eq!(s.mode, DiffMode::Unified);
        s.toggle_mode();
        assert_eq!(s.mode, DiffMode::SideBySide);
        s.toggle_mode();
        assert_eq!(s.mode, DiffMode::Unified);
    }

    #[test]
    fn toggle_mode_resets_scroll() {
        let mut s = DiffViewerState::new();
        s.scroll = 20;
        s.toggle_mode();
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn next_hunk_resets_scroll() {
        let mut s = DiffViewerState::new();
        s.push_diff("main.rs", SAMPLE_DIFF);
        s.scroll = 15;
        s.next_hunk();
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn summary_string_format() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+x");
        s.push_diff("b.rs", "+y");
        s.total_added = 10;
        s.total_removed = 3;
        let summary = s.summary();
        assert!(summary.contains("2 file"), "got: {summary}");
        assert!(summary.contains("10"), "got: {summary}");
        assert!(summary.contains("3"), "got: {summary}");
    }

    #[test]
    fn has_diffs_predicate() {
        let s = DiffViewerState::new();
        assert!(!DiffWidget::has_diffs(&s));
        let mut s = s;
        s.push_diff("a.rs", "+x");
        assert!(DiffWidget::has_diffs(&s));
    }

    // ── Widget rendering ──────────────────────────────────────────────────────

    #[test]
    fn widget_renders_empty_without_panic() {
        let s = DiffViewerState::new();
        let theme = Theme::dark();
        let w = DiffWidget::new(&s, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        w.render(Rect::new(0, 0, 60, 20), &mut buf);
    }

    #[test]
    fn widget_renders_unified_without_panic() {
        let mut s = DiffViewerState::new();
        s.push_diff("main.rs", SAMPLE_DIFF);
        let theme = Theme::dark();
        let w = DiffWidget::new(&s, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        w.render(Rect::new(0, 0, 100, 30), &mut buf);
    }

    #[test]
    fn widget_renders_side_by_side_without_panic() {
        let mut s = DiffViewerState::new();
        s.push_diff("main.rs", SAMPLE_DIFF);
        s.mode = DiffMode::SideBySide;
        let theme = Theme::dark();
        let w = DiffWidget::new(&s, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        w.render(Rect::new(0, 0, 120, 30), &mut buf);
    }

    #[test]
    fn widget_renders_tiny_area_without_panic() {
        let mut s = DiffViewerState::new();
        s.push_diff("a.rs", "+x");
        let theme = Theme::dark();
        let w = DiffWidget::new(&s, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 4));
        w.render(Rect::new(0, 0, 8, 4), &mut buf);
    }

    #[test]
    fn widget_renders_second_hunk() {
        let mut s = DiffViewerState::new();
        s.push_diff("main.rs", SAMPLE_DIFF);
        s.current_hunk = 1;
        let theme = Theme::dark();
        let w = DiffWidget::new(&s, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        w.render(Rect::new(0, 0, 100, 30), &mut buf);
    }

    #[test]
    fn shorten_path_long() {
        let s = shorten_path("very/long/path/to/some/file.rs", 15);
        assert!(s.len() <= 15 || s.starts_with('…'));
    }

    #[test]
    fn shorten_path_short_unchanged() {
        assert_eq!(shorten_path("a.rs", 20), "a.rs");
    }
}
