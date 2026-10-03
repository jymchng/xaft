//! `DropdownWidget` — a `MenuWidget` adapter over the PRD-59 trigger system.
//!
//! Bridges the existing `ActiveTrigger` + `TriggerHandler` system to the
//! `MenuWidget` protocol introduced by PRD-61, so trigger dropdowns can
//! optionally be managed by `MenuDriver` in the future.

use std::io::{self, Write};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};

use crate::menu::{MenuPayload, MenuResult, MenuWidget};
use crate::trigger::{ActiveTrigger, MatchItem, TriggerContext, TriggerHandler};

// ── DropdownWidget ────────────────────────────────────────────────────────────

/// A `MenuWidget` adapter that wraps an `ActiveTrigger` + `TriggerHandler`.
///
/// This is a migration bridge: existing trigger dropdowns (`@`-mention, `/` slash)
/// can be driven via `MenuDriver` without changes to the underlying handler.
pub struct DropdownWidget {
    trigger: ActiveTrigger,
    handler: Arc<dyn TriggerHandler>,
    title_str: &'static str,
}

impl DropdownWidget {
    /// Create a `DropdownWidget` from an active trigger and its handler.
    pub fn new(trigger: ActiveTrigger, handler: Arc<dyn TriggerHandler>) -> Self {
        Self {
            trigger,
            handler,
            title_str: "completion",
        }
    }

    /// Override the title shown in the overlay header.
    pub fn with_title(mut self, title: &'static str) -> Self {
        self.title_str = title;
        self
    }
}

impl MenuWidget for DropdownWidget {
    fn title(&self) -> &str {
        self.title_str
    }

    fn render(
        &self,
        out: &mut dyn Write,
        size: (u16, u16),
        _prev_rows: usize,
    ) -> io::Result<usize> {
        let cols = size.0 as usize;
        let max_visible = self.handler.max_visible();
        let items = &self.trigger.items;
        let selected = self.trigger.selected;
        let scroll_top = self.trigger.scroll_top;

        if items.is_empty() {
            return Ok(0);
        }

        let visible: Vec<&MatchItem> = items.iter().skip(scroll_top).take(max_visible).collect();

        let mut rows = 0usize;
        for (i, item) in visible.iter().enumerate() {
            let abs_idx = scroll_top + i;
            let is_selected = abs_idx == selected;
            let prefix = if is_selected { "▶ " } else { "  " };
            let display = &item.display;
            let row = format!(
                "  {prefix}{display:<width$}",
                width = cols.saturating_sub(4)
            );
            if is_selected {
                writeln!(out, "\x1b[7m{row}\x1b[0m\r")?;
            } else {
                writeln!(out, "{row}\r")?;
            }
            rows += 1;
        }

        // Show hint line for selected item.
        if let Some(item) = self.trigger.selected_item() {
            if let Some(ref hint) = item.hint {
                writeln!(out, "  \x1b[2m{hint}\x1b[0m\r")?;
                rows += 1;
            }
        }

        out.flush()?;
        Ok(rows)
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        let count = self.trigger.items.len();
        let max_visible = self.handler.max_visible();

        match key.code {
            KeyCode::Esc => {
                return MenuResult::Cancel;
            }
            KeyCode::Enter | KeyCode::Tab => {
                if let Some(item) = self.trigger.selected_item() {
                    let working_dir = std::path::Path::new(".");
                    let prefix = "";
                    let dir_prefix = "";
                    let ctx = TriggerContext {
                        prefix,
                        dir_prefix,
                        working_dir,
                        terminal_cols: 80,
                    };
                    let insert = self.handler.on_select(item, &ctx);
                    return MenuResult::Done(MenuPayload::Selected(insert));
                }
                return MenuResult::Cancel;
            }
            KeyCode::Up => {
                if count > 0 {
                    if self.trigger.selected == 0 {
                        self.trigger.selected = count - 1;
                    } else {
                        self.trigger.selected -= 1;
                    }
                    // Adjust scroll.
                    if self.trigger.selected < self.trigger.scroll_top {
                        self.trigger.scroll_top = self.trigger.selected;
                    }
                }
            }
            KeyCode::Down => {
                if count > 0 {
                    self.trigger.selected = (self.trigger.selected + 1) % count;
                    // Adjust scroll.
                    if self.trigger.selected >= self.trigger.scroll_top + max_visible {
                        self.trigger.scroll_top =
                            self.trigger.selected.saturating_sub(max_visible - 1);
                    }
                }
            }
            _ => {}
        }
        MenuResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::{ActiveTriggerScan, MatchKind};

    fn make_trigger(items: Vec<MatchItem>) -> ActiveTrigger {
        ActiveTrigger {
            scan: ActiveTriggerScan {
                trigger_char: '@',
                prefix: String::new(),
                dir_prefix: String::new(),
                trigger_byte_pos: 0,
            },
            items,
            selected: 0,
            scroll_top: 0,
        }
    }

    struct PassthroughHandler;
    impl TriggerHandler for PassthroughHandler {
        fn trigger_char(&self) -> char {
            '@'
        }
        fn matches(&self, _ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
            vec![]
        }
    }

    struct ShortWindowHandler;
    impl TriggerHandler for ShortWindowHandler {
        fn trigger_char(&self) -> char {
            '@'
        }
        fn matches(&self, _ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
            vec![]
        }
        fn max_visible(&self) -> usize {
            2
        }
    }

    fn make_item(display: &str, insert: &str) -> MatchItem {
        MatchItem {
            display: display.to_string(),
            insert: insert.to_string(),
            hint: None,
            kind: MatchKind::File,
        }
    }

    fn make_item_with_hint(display: &str, insert: &str, hint: &str) -> MatchItem {
        MatchItem {
            hint: Some(hint.to_string()),
            ..make_item(display, insert)
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    fn widget(items: Vec<MatchItem>) -> DropdownWidget {
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        DropdownWidget::new(make_trigger(items), handler)
    }

    fn widget_with(
        items: Vec<MatchItem>,
        selected: usize,
        scroll_top: usize,
        handler: Arc<dyn TriggerHandler>,
    ) -> DropdownWidget {
        let mut trigger = make_trigger(items);
        trigger.selected = selected;
        trigger.scroll_top = scroll_top;
        DropdownWidget::new(trigger, handler)
    }

    fn render_to_string(w: &DropdownWidget, cols: u16, rows: u16) -> (usize, String) {
        let mut buf: Vec<u8> = Vec::new();
        let written = w.render(&mut buf, (cols, rows), 0).expect("render");
        (written, String::from_utf8(buf).expect("utf-8"))
    }

    #[test]
    fn esc_returns_cancel() {
        let trigger = make_trigger(vec![make_item("foo.rs", "@foo.rs ")]);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut w = DropdownWidget::new(trigger, handler);
        let key = KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE);
        assert!(matches!(w.handle_key(key), MenuResult::Cancel));
    }

    #[test]
    fn enter_selects_item() {
        let trigger = make_trigger(vec![make_item("foo.rs", "@foo.rs ")]);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut w = DropdownWidget::new(trigger, handler);
        let key = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        match w.handle_key(key) {
            MenuResult::Done(MenuPayload::Selected(s)) => assert_eq!(s, "@foo.rs "),
            other => panic!("Expected Done(Selected), got {other:?}"),
        }
    }

    #[test]
    fn down_wraps_selection() {
        let trigger = make_trigger(vec![
            make_item("a.rs", "@a.rs "),
            make_item("b.rs", "@b.rs "),
        ]);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut w = DropdownWidget::new(trigger, handler);
        let down = KeyEvent::new(KeyCode::Down, crossterm::event::KeyModifiers::NONE);
        w.handle_key(down);
        assert_eq!(w.trigger.selected, 1);
        w.handle_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(w.trigger.selected, 0); // wraps
    }

    #[test]
    fn title_default_is_completion() {
        let trigger = make_trigger(vec![]);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let w = DropdownWidget::new(trigger, handler);
        assert_eq!(w.title(), "completion");
    }

    #[test]
    fn with_title_overrides_the_header_and_is_chainable() {
        let w = widget(vec![]).with_title("files");
        assert_eq!(w.title(), "files");
    }

    #[test]
    fn an_empty_item_list_renders_nothing() {
        let w = widget(vec![]);
        let (rows, text) = render_to_string(&w, 40, 10);
        assert_eq!(rows, 0, "no items means no rows");
        assert!(text.is_empty(), "no output at all, got {text:?}");
    }

    #[test]
    fn the_selected_row_is_highlighted_and_others_are_not() {
        let w = widget(vec![
            make_item("a.rs", "@a.rs "),
            make_item("b.rs", "@b.rs "),
        ]);
        let mut trigger_items = w.trigger.items.clone();
        // Render with the SECOND item selected by rebuilding the trigger.
        drop(w);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut trigger = make_trigger(std::mem::take(&mut trigger_items));
        trigger.selected = 1;
        let w = DropdownWidget::new(trigger, handler);
        let (rows, text) = render_to_string(&w, 40, 10);

        assert_eq!(rows, 2, "one row per visible item: {text:?}");
        // Reverse video is ESC [ 7 m; BEL (U+0007) is a different byte, so
        // match the full sequence.
        let highlighted: Vec<&str> = text
            .split("\r\n")
            .filter(|l| l.contains("\u{1b}[7m"))
            .collect();
        assert_eq!(highlighted.len(), 1, "exactly one reverse-video row");
        assert!(
            highlighted[0].contains("▶ b.rs"),
            "the selected row carries the caret and the display text: {:?}",
            highlighted[0]
        );
        assert!(
            !highlighted[0].contains("a.rs"),
            "only the selected item is highlighted: {:?}",
            highlighted[0]
        );
        assert!(
            text.contains("  a.rs"),
            "unselected rows are plain: {text:?}"
        );
    }

    #[test]
    fn render_pads_each_row_to_the_terminal_width_minus_four() {
        let w = widget(vec![make_item("x", "x")]);
        let (_, text) = render_to_string(&w, 20, 10);
        let row = text.split("\r\n").next().expect("a row");
        // Strip the reverse-video wrapper so only the printable row remains:
        // two leading spaces, the two-char prefix, then the display padded to
        // `cols - 4`. (Measured: the wrapper is ESC [ 7 m / ESC [ 0 m, so the
        // raw line is 8 bytes longer than the printable row.)
        let printable = row
            .replace("\u{1b}[7m", "")
            .replace("\u{1b}[0m", "")
            .replace('\r', "");
        assert_eq!(
            printable.chars().count(),
            4 + 16,
            "row is padded to cols-4 after the 4-char prefix: {printable:?}"
        );
        assert!(
            printable.starts_with("  \u{25b6} x"),
            "prefix is two spaces plus the caret: {printable:?}"
        );
    }

    #[test]
    fn render_only_shows_the_max_visible_window_from_the_scroll_top() {
        let items: Vec<MatchItem> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| make_item(s, s))
            .collect();
        let handler = Arc::new(ShortWindowHandler) as Arc<dyn TriggerHandler>;
        let w = widget_with(items, 4, 3, handler);
        let (rows, text) = render_to_string(&w, 40, 10);

        assert_eq!(rows, 2, "max_visible=2 caps the window: {text:?}");
        assert!(
            text.contains("d") && text.contains("e"),
            "window is 3..5: {text:?}"
        );
        assert!(
            !text.contains('a') && !text.contains('b'),
            "no rows above the scroll top"
        );
    }

    #[test]
    fn the_selected_items_hint_is_rendered_as_an_extra_dim_row() {
        let w = widget(vec![
            make_item("a.rs", "@a.rs "),
            make_item_with_hint("b.rs", "@b.rs ", "src/main.rs"),
        ]);
        let mut items = w.trigger.items.clone();
        drop(w);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut trigger = make_trigger(std::mem::take(&mut items));
        trigger.selected = 1;
        let w = DropdownWidget::new(trigger, handler);
        let (rows, text) = render_to_string(&w, 40, 10);

        assert_eq!(rows, 3, "two items plus the hint row: {text:?}");
        assert!(
            text.contains("\u{1b}[2msrc/main.rs\u{1b}[0m"),
            "hint is dimmed: {text:?}"
        );
    }

    #[test]
    fn a_hinted_item_that_is_not_selected_gets_no_hint_row() {
        // The hint belongs to the SELECTED item only. Item 0 carries a hint but
        // item 1 is selected, so no hint row is emitted even though the list
        // does contain a hinted item.
        let w = widget(vec![
            make_item_with_hint("a.rs", "@a.rs ", "unused hint"),
            make_item("b.rs", "@b.rs "),
        ]);
        let mut items = w.trigger.items.clone();
        drop(w);
        let handler = Arc::new(PassthroughHandler) as Arc<dyn TriggerHandler>;
        let mut trigger = make_trigger(std::mem::take(&mut items));
        trigger.selected = 1;
        let w = DropdownWidget::new(trigger, handler);
        let (rows, text) = render_to_string(&w, 40, 10);

        assert_eq!(rows, 2, "only the selected item shows a hint: {text:?}");
        assert!(!text.contains("unused hint"), "no hint row: {text:?}");
    }

    #[test]
    fn tab_selects_the_same_item_enter_does() {
        let w = widget(vec![make_item("foo.rs", "@foo.rs ")]);
        let mut w = w;
        match w.handle_key(key(KeyCode::Tab)) {
            MenuResult::Done(MenuPayload::Selected(s)) => assert_eq!(s, "@foo.rs "),
            other => panic!("Tab should select, got {other:?}"),
        }
    }

    #[test]
    fn enter_without_any_items_is_a_cancel() {
        let mut w = widget(vec![]);
        assert!(matches!(
            w.handle_key(key(KeyCode::Enter)),
            MenuResult::Cancel
        ));
        assert!(
            matches!(w.handle_key(key(KeyCode::Tab)), MenuResult::Cancel),
            "Tab behaves the same way with nothing to select"
        );
    }

    #[test]
    fn up_wraps_from_the_first_item_to_the_last() {
        let items: Vec<MatchItem> = ["a", "b", "c"].iter().map(|s| make_item(s, s)).collect();
        let mut w = widget(items);
        assert_eq!(w.trigger.selected, 0);
        assert!(matches!(
            w.handle_key(key(KeyCode::Up)),
            MenuResult::Continue
        ));
        assert_eq!(w.trigger.selected, 2, "Up from 0 wraps to the end");
    }

    #[test]
    fn up_steps_backwards_through_the_list() {
        let items: Vec<MatchItem> = ["a", "b", "c"].iter().map(|s| make_item(s, s)).collect();
        let mut w = widget_with(items, 2, 0, Arc::new(PassthroughHandler));
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.trigger.selected, 1);
        assert_eq!(w.trigger.scroll_top, 0, "scrolling up needs no adjustment");
    }

    #[test]
    fn up_pulls_the_scroll_top_down_so_the_selection_stays_visible() {
        let items: Vec<MatchItem> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| make_item(s, s))
            .collect();
        let handler = Arc::new(ShortWindowHandler) as Arc<dyn TriggerHandler>;
        let mut w = widget_with(items, 2, 1, handler);
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.trigger.selected, 1);
        assert_eq!(
            w.trigger.scroll_top, 1,
            "selection 1 is at the top edge, so the window follows it"
        );

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.trigger.selected, 0);
        assert_eq!(w.trigger.scroll_top, 0, "scroll_top tracks the selection");
    }

    #[test]
    fn down_scrolls_the_window_when_the_selection_leaves_it() {
        let items: Vec<MatchItem> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| make_item(s, s))
            .collect();
        let handler = Arc::new(ShortWindowHandler) as Arc<dyn TriggerHandler>;
        let mut w = widget_with(items, 0, 0, handler);

        w.handle_key(key(KeyCode::Down));
        assert_eq!((w.trigger.selected, w.trigger.scroll_top), (1, 0));

        // Selection 2 is one past the 2-row window, so it scrolls by one.
        w.handle_key(key(KeyCode::Down));
        assert_eq!(w.trigger.selected, 2);
        assert_eq!(w.trigger.scroll_top, 1);

        w.handle_key(key(KeyCode::Down));
        assert_eq!((w.trigger.selected, w.trigger.scroll_top), (3, 2));
    }

    #[test]
    fn arrow_keys_do_nothing_when_there_are_no_items() {
        let mut w = widget(vec![]);
        for code in [KeyCode::Up, KeyCode::Down] {
            assert!(
                matches!(w.handle_key(key(code)), MenuResult::Continue),
                "{code:?} on an empty list keeps the dropdown open"
            );
            assert_eq!(w.trigger.selected, 0, "selection is untouched");
            assert_eq!(w.trigger.scroll_top, 0, "scroll is untouched");
        }
    }

    #[test]
    fn an_unrelated_key_keeps_the_dropdown_open() {
        let mut w = widget(vec![make_item("a", "a"), make_item("b", "b")]);
        let result = w.handle_key(key(KeyCode::Char('c')));
        assert!(matches!(result, MenuResult::Continue));
        assert_eq!(w.trigger.selected, 0, "a printable key is not navigation");
        assert_eq!(w.trigger.scroll_top, 0);
    }
}
