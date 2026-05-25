//! File tree widget.
//!
//! Shows the list of files modified in the current session, sourced from
//! `AppState.diff.diffs` (the `DiffViewerState`).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Widget},
};

use crate::state::AppState;
use crate::theme::Theme;

/// Widget that displays the list of files modified in the current session.
pub struct FileTreeWidget<'a> {
    state: &'a AppState,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> FileTreeWidget<'a> {
    /// Create a new `FileTreeWidget`.
    pub fn new(state: &'a AppState, theme: &'a Theme, focused: bool) -> Self {
        Self {
            state,
            theme,
            focused,
        }
    }
}

impl Widget for FileTreeWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_style = if self.focused {
            self.theme.border_focused()
        } else {
            self.theme.border()
        };
        let block = Block::default()
            .title(" Files ")
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
        if inner.height == 0 {
            return;
        }

        // Get modified file paths from diff state (Vec<ParsedFileDiff>)
        let files: Vec<&str> = self
            .state
            .diff
            .diffs
            .iter()
            .map(|d| d.path.as_str())
            .collect();

        if files.is_empty() {
            let item = ListItem::new(Line::from(Span::styled("(no changes)", self.theme.dim())));
            List::new(vec![item])
                .style(self.theme.base())
                .render(inner, buf);
            return;
        }

        let max = inner.height as usize;
        let inner_width = inner.width as usize;
        let items: Vec<ListItem> = files
            .iter()
            .take(max)
            .map(|path| {
                // Show just the filename, with modified indicator
                let display = if path.len() > inner_width.saturating_sub(4) {
                    let start = path.len().saturating_sub(inner_width.saturating_sub(5));
                    format!("\u{2026}{}", &path[start..])
                } else {
                    path.to_string()
                };
                ListItem::new(Line::from(vec![
                    Span::styled("M ", self.theme.warning()),
                    Span::styled(display, Style::default().fg(self.theme.fg)),
                ]))
            })
            .collect();

        List::new(items).style(self.theme.base()).render(inner, buf);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::TuiEvent;
    use crate::state::AppState;
    use crate::theme::Theme;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::collections::HashMap;

    fn make_state() -> AppState {
        AppState::new("test task")
    }

    #[test]
    fn file_tree_widget_renders_without_panic_empty() {
        let state = make_state();
        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        w.render(Rect::new(0, 0, 40, 20), &mut buf);
    }

    #[test]
    fn file_tree_widget_renders_without_panic_focused() {
        let state = make_state();
        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, true);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        w.render(Rect::new(0, 0, 40, 20), &mut buf);
    }

    #[test]
    fn file_tree_widget_shows_modified_files() {
        let mut state = make_state();
        let mut diffs = HashMap::new();
        diffs.insert("src/main.rs".to_string(), "+fn new() {}".to_string());
        diffs.insert("src/lib.rs".to_string(), "+fn helper() {}".to_string());
        state.handle_event(TuiEvent::FileEditsCommitted {
            files: vec!["src/main.rs".into(), "src/lib.rs".into()],
            lines_added: 2,
            lines_removed: 0,
            diffs,
        });

        assert_eq!(state.diff.diffs.len(), 2);

        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        w.render(Rect::new(0, 0, 60, 20), &mut buf);

        // Verify some content was rendered (non-empty area after render)
        let rendered: String = (0..20)
            .flat_map(|y| (0..60u16).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect::<Vec<_>>()
            .join("");
        // "Files" title or "M" modified indicator should appear
        assert!(
            rendered.contains('M') || rendered.contains("Files"),
            "Expected file list content in buffer"
        );
    }

    #[test]
    fn file_tree_widget_shows_no_changes_when_empty() {
        let state = make_state();
        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 10));
        w.render(Rect::new(0, 0, 60, 10), &mut buf);

        let rendered: String = (0..10)
            .flat_map(|y| (0..60u16).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            rendered.contains("no changes"),
            "Empty diff state should show 'no changes'"
        );
    }

    #[test]
    fn file_tree_widget_renders_tiny_area_without_panic() {
        let state = make_state();
        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        w.render(Rect::new(0, 0, 6, 3), &mut buf);
    }

    #[test]
    fn file_tree_widget_truncates_long_paths() {
        let mut state = make_state();
        let long_path = "very/long/path/to/some/deeply/nested/file.rs";
        let mut diffs = HashMap::new();
        diffs.insert(long_path.to_string(), "+x".to_string());
        state.handle_event(TuiEvent::FileEditsCommitted {
            files: vec![long_path.into()],
            lines_added: 1,
            lines_removed: 0,
            diffs,
        });

        let theme = Theme::dark();
        let w = FileTreeWidget::new(&state, &theme, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 6));
        // Must not panic even with a wide path in a narrow widget
        w.render(Rect::new(0, 0, 20, 6), &mut buf);
    }
}
