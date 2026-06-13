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

    fn make_item(display: &str, insert: &str) -> MatchItem {
        MatchItem {
            display: display.to_string(),
            insert: insert.to_string(),
            hint: None,
            kind: MatchKind::File,
        }
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
}
