//! The background-terminal overlay opened by `/ps` (parity rows 20/22, keybinding 11).
//!
//! A port of agenthicc's `TerminalListOverlay`
//! (`agenthicc/tui/workspace/overlays/terminals.py`). The manager owns the
//! records; this widget owns only a bounded presentation projection and a
//! selection. The selected **terminal id** is authoritative, not the row index,
//! so a refresh that reorders or drops records can never make `s` stop a
//! different terminal from the one the user was looking at.
//!
//! # Keys (agenthicc's, unchanged)
//!
//! | key | action |
//! |---|---|
//! | `↑`/`k`, `↓`/`j` | move the selection |
//! | `PgUp`/`PgDn`, `Home`/`End` | page / jump |
//! | `Enter` | details view for the selected terminal |
//! | `s`, `Ctrl+X` | stop the selected terminal (`request_stop`) |
//! | `Esc` | leave the details view, else close the overlay |
//!
//! `s` is keybinding row 11. The matrix labels it "suspend"; what agenthicc's
//! overlay actually does on `s` is `manager.request_stop(selected)` — a
//! graceful stop that escalates to `SIGKILL` after the grace period — and that
//! is what this widget does. Every other key is swallowed so a stray letter
//! cannot leak into the input bar while the overlay owns the keyboard.

use std::io::{self, Write};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use xaft_tools::terminals::{TerminalManager, TerminalRecord};

use crate::menu::{MenuResult, MenuWidget, OverlayLine, OverlayStyle};

/// Fixed rows around the list: title, two detail rows, message, footer.
const LIST_OVERHEAD_ROWS: usize = 5;

/// Longest output tail shown in the overlay, in characters.
const MAX_OUTPUT_CHARS: usize = 240;

/// Current wall-clock time in epoch seconds, as the records store it.
fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Collapse whitespace and cut `value` to `limit` characters with an ellipsis.
///
/// agenthicc's `_shorten`: an empty value renders as `—` so a column never
/// collapses into an unreadable gap.
pub fn shorten(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "—".to_string();
    }
    if compact.chars().count() <= limit {
        return compact;
    }
    let keep = limit.saturating_sub(1).max(1);
    let mut cut: String = compact.chars().take(keep).collect();
    cut = cut.trim_end().to_string();
    cut.push('…');
    cut
}

/// The last non-empty output line of a terminal, bounded.
fn output_preview(record: &TerminalRecord) -> String {
    let raw = if !record.stdout.is_empty() {
        record.stdout.as_str()
    } else if !record.stderr.is_empty() {
        record.stderr.as_str()
    } else {
        "(no output)"
    };
    let last = raw
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .next_back()
        .unwrap_or_else(|| "(no output)".to_string());
    shorten(&last, MAX_OUTPUT_CHARS)
}

/// A return code as agenthicc renders it: `—` until the process has one.
pub fn returncode_cell(record: &TerminalRecord) -> String {
    record
        .returncode
        .map(|c| c.to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// Rows per page for an overlay allowed `max_rows` rows.
fn page_size(max_rows: usize) -> usize {
    max_rows.saturating_sub(LIST_OVERHEAD_ROWS).max(1)
}

/// The `/ps` overlay over the session's one terminal manager.
pub struct TerminalListMenu {
    manager: Arc<TerminalManager>,
    /// The terminal the selection is pinned to.
    selected_id: Option<String>,
    /// Last known index of the selection, used when its record disappears.
    index: usize,
    /// `true` while the details view is open.
    details: bool,
    /// Feedback from the last action (`s` / `Ctrl+X`).
    message: String,
}

impl TerminalListMenu {
    /// Open the overlay, optionally pre-selecting `selected_id`
    /// (agenthicc's `/ps <terminal-id>`).
    pub fn new(manager: Arc<TerminalManager>, selected_id: Option<String>) -> Self {
        let mut menu = Self {
            manager,
            selected_id: selected_id.filter(|s| !s.is_empty()),
            index: 0,
            details: false,
            message: String::new(),
        };
        let records = menu.manager.list_records();
        menu.select_requested(&records);
        menu
    }

    /// The terminal id the selection is pinned to, if any.
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// Whether the details view is open.
    pub fn in_details(&self) -> bool {
        self.details
    }

    /// The feedback line from the last action.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Re-pin the selection against a fresh record list.
    ///
    /// agenthicc's `_select_requested`: keep the requested id when it is still
    /// present; otherwise clamp to a neighbouring row, and drop out of the
    /// details view rather than silently showing a replacement terminal.
    fn select_requested(&mut self, records: &[TerminalRecord]) {
        if let Some(id) = self.selected_id.clone() {
            if let Some(pos) = records.iter().position(|r| r.terminal_id == id) {
                self.index = pos;
                return;
            }
        }
        if records.is_empty() {
            self.index = 0;
            self.selected_id = None;
            self.details = false;
            return;
        }
        let requested = self.selected_id.clone();
        self.index = self.index.min(records.len() - 1);
        self.selected_id = Some(records[self.index].terminal_id.clone());
        if requested.is_some() && requested != self.selected_id {
            self.details = false;
        }
    }

    /// Index of the pinned selection in `records`, without mutating.
    fn display_index(&self, records: &[TerminalRecord]) -> usize {
        self.selected_id
            .as_ref()
            .and_then(|id| records.iter().position(|r| &r.terminal_id == id))
            .unwrap_or_else(|| self.index.min(records.len().saturating_sub(1)))
    }

    fn set_index(&mut self, records: &[TerminalRecord], index: usize) {
        if records.is_empty() {
            self.index = 0;
            self.selected_id = None;
            return;
        }
        self.index = index.min(records.len() - 1);
        self.selected_id = Some(records[self.index].terminal_id.clone());
    }

    fn move_by(&mut self, records: &[TerminalRecord], delta: isize) {
        let next = (self.index as isize + delta).max(0) as usize;
        self.set_index(records, next);
    }

    /// Ask the manager to stop the selected terminal.
    fn stop_selected(&mut self) {
        let Some(id) = self.selected_id.clone() else {
            self.message = "No owned background terminals.".to_string();
            return;
        };
        self.message = if self.manager.request_stop(&id, false) {
            format!("Stop requested for {id}.")
        } else {
            format!("No running owned terminal found for {id}.")
        };
    }

    fn footer(&self, cols: u16) -> OverlayLine {
        let hint = if self.details {
            "Esc back   s stop   Ctrl+X stop"
        } else if cols < 48 {
            "↑↓ PgUp/PgDn   s   Esc close"
        } else {
            "↑↓/j/k select   PgUp/PgDn page   Home/End   Enter details   s stop   Ctrl+X stop   Esc close"
        };
        OverlayLine::new(hint, OverlayStyle::Dim)
    }

    fn details_lines(&self, record: &TerminalRecord, cols: u16) -> Vec<OverlayLine> {
        let mut lines = vec![
            OverlayLine::new(
                format!("Terminal Details · {}", shorten(&record.terminal_id, 32)),
                OverlayStyle::Title,
            ),
            OverlayLine::new(
                format!("State: {}", shorten(record.state.as_str(), 24)),
                OverlayStyle::Normal,
            ),
            OverlayLine::new(
                format!("Command: {}", shorten(&record.command, 120)),
                OverlayStyle::Normal,
            ),
            OverlayLine::new(
                format!(
                    "PID: {}  Duration: {:.1}s",
                    record
                        .pid
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "—".into()),
                    record.elapsed_s(now_epoch())
                ),
                OverlayStyle::Normal,
            ),
            OverlayLine::new(
                format!("Output: {}", output_preview(record)),
                OverlayStyle::Normal,
            ),
        ];
        if !self.message.is_empty() {
            lines.push(OverlayLine::new(self.message.clone(), OverlayStyle::Dim));
        }
        lines.push(self.footer(cols));
        lines
    }
}

impl MenuWidget for TerminalListMenu {
    fn title(&self) -> &str {
        "Background Terminals"
    }

    /// The overlay draws through [`MenuWidget::overlay_lines`]; this raw-ANSI
    /// path only exists to satisfy the trait and writes the same rows.
    fn render(
        &self,
        out: &mut dyn Write,
        size: (u16, u16),
        _prev_rows: usize,
    ) -> io::Result<usize> {
        let rows = self
            .overlay_lines(size.0, crate::menu::OVERLAY_MAX_ROWS)
            .unwrap_or_default();
        for row in &rows {
            write!(out, "{}\r\n", row.text)?;
        }
        Ok(rows.len())
    }

    fn handle_key(&mut self, key: KeyEvent) -> MenuResult {
        let records = self.manager.list_records();
        if key.code == KeyCode::Esc {
            if self.details {
                self.details = false;
                return MenuResult::Continue;
            }
            return MenuResult::Cancel;
        }
        self.select_requested(&records);
        let page = page_size(crate::menu::OVERLAY_MAX_ROWS) as isize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.move_by(&records, -1),
            KeyCode::Down => self.move_by(&records, 1),
            KeyCode::Char('k') | KeyCode::Char('K') if !ctrl => self.move_by(&records, -1),
            KeyCode::Char('j') | KeyCode::Char('J') if !ctrl => self.move_by(&records, 1),
            KeyCode::PageUp => self.move_by(&records, -page),
            KeyCode::PageDown => self.move_by(&records, page),
            KeyCode::Home => self.set_index(&records, 0),
            KeyCode::End => self.set_index(&records, records.len().saturating_sub(1)),
            KeyCode::Enter if self.selected_id.is_some() => self.details = true,
            // Keybinding row 11: `s` stops the selected terminal.
            KeyCode::Char('s') | KeyCode::Char('S') if !ctrl => self.stop_selected(),
            // Ctrl+X, shared with agenthicc's background-session manager.
            KeyCode::Char('x') | KeyCode::Char('X') if ctrl => self.stop_selected(),
            _ => {}
        }
        MenuResult::Continue
    }

    fn overlay_lines(&self, cols: u16, max_rows: usize) -> Option<Vec<OverlayLine>> {
        let records = self.manager.list_records();
        let index = self.display_index(&records);
        let selected = records.get(index);

        if self.details {
            if let Some(record) =
                selected.filter(|r| Some(&r.terminal_id) == self.selected_id.as_ref())
            {
                let mut lines = self.details_lines(record, cols);
                lines.truncate(max_rows);
                return Some(lines);
            }
        }

        let size = page_size(max_rows);
        let page = if records.is_empty() { 0 } else { index / size };
        let start = page * size;
        let end = (start + size).min(records.len());
        let pages = records.len().div_ceil(size).max(1);
        let title = if records.is_empty() {
            "Background Terminals · page 1/1 · showing 0 of 0".to_string()
        } else {
            format!(
                "Background Terminals · page {}/{} · showing {}–{} of {}",
                page + 1,
                pages,
                start + 1,
                end,
                records.len()
            )
        };

        let mut lines = vec![OverlayLine::new(title, OverlayStyle::Title)];
        if records.is_empty() {
            lines.push(OverlayLine::new(
                "  —  —  No owned terminals  —",
                OverlayStyle::Dim,
            ));
        }
        for (offset, record) in records[start..end].iter().enumerate() {
            let at = start + offset;
            let marker = if at == index { "▶" } else { " " };
            let row = format!(
                "{marker} {:<14} {:<10} {:<36} {}",
                shorten(&record.terminal_id, 14),
                shorten(record.state.as_str(), 10),
                shorten(&record.label, 36),
                returncode_cell(record)
            );
            let style = if at == index {
                OverlayStyle::Selected
            } else {
                OverlayStyle::Normal
            };
            lines.push(OverlayLine::new(row, style));
        }
        match selected {
            Some(record) => {
                lines.push(OverlayLine::new(
                    format!(
                        "State: {}  ·  Command: {}",
                        record.state.as_str(),
                        shorten(&record.command, 120)
                    ),
                    OverlayStyle::Dim,
                ));
                lines.push(OverlayLine::new(
                    format!(
                        "PID: {}  Duration: {:.1}s  ·  Output: {}",
                        record
                            .pid
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "—".into()),
                        record.elapsed_s(now_epoch()),
                        output_preview(record)
                    ),
                    OverlayStyle::Dim,
                ));
            }
            None => lines.push(OverlayLine::new(
                "No owned background terminals.",
                OverlayStyle::Dim,
            )),
        }
        if !self.message.is_empty() {
            lines.push(OverlayLine::new(self.message.clone(), OverlayStyle::Dim));
        }
        lines.push(self.footer(cols));
        lines.truncate(max_rows);
        Some(lines)
    }
}

/// Render records as agenthicc's `Background Terminals` table (ID / State /
/// Command / Return), for a `/ps` with no overlay host.
pub fn terminal_table_lines(records: &[TerminalRecord]) -> Vec<String> {
    let mut lines = vec![
        "Background Terminals".to_string(),
        format!(
            "{:<14} {:<12} {:<40} {}",
            "ID", "State", "Command", "Return"
        ),
    ];
    for record in records {
        lines.push(format!(
            "{:<14} {:<12} {:<40} {}",
            record.terminal_id,
            record.state.as_str(),
            shorten(&record.label, 40),
            returncode_cell(record)
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use xaft_tools::terminals::TerminalManagerConfig;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn manager(dir: &std::path::Path) -> Arc<TerminalManager> {
        let mut config = TerminalManagerConfig::new("sess-overlay", dir);
        config.store_root = dir.join("store");
        config.cancel_grace_s = 1.0;
        Arc::new(TerminalManager::new(config))
    }

    async fn start(m: &TerminalManager, label: &str) -> String {
        let argv = vec!["/bin/sleep".to_string(), "30".to_string()];
        let out = m
            .start(
                None,
                Some(&argv),
                None,
                0.0,
                None,
                false,
                label,
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(out["ok"], serde_json::json!(true), "{out}");
        out["terminal_id"].as_str().unwrap().to_string()
    }

    fn texts(lines: &[OverlayLine]) -> Vec<String> {
        lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn shorten_collapses_whitespace_and_marks_truncation() {
        assert_eq!(shorten("  a   b  ", 10), "a b");
        assert_eq!(shorten("", 10), "—");
        assert_eq!(shorten("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn an_empty_manager_renders_agenthiccs_empty_page() {
        let dir = tempfile::tempdir().unwrap();
        let menu = TerminalListMenu::new(manager(dir.path()), None);
        let lines = menu
            .overlay_lines(120, crate::menu::OVERLAY_MAX_ROWS)
            .unwrap();
        let text = texts(&lines).join("\n");
        assert!(
            text.contains("Background Terminals · page 1/1 · showing 0 of 0"),
            "{text}"
        );
        assert!(text.contains("No owned terminals"), "{text}");
        assert!(lines.len() <= crate::menu::OVERLAY_MAX_ROWS);
    }

    /// Keybinding row 11: `s` in the terminal overlay stops the selected
    /// terminal through the shared manager — the process actually exits.
    #[tokio::test]
    async fn s_stops_the_selected_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m, "sleeper").await;
        let mut menu = TerminalListMenu::new(Arc::clone(&m), None);
        assert_eq!(menu.selected_id(), Some(id.as_str()));

        assert!(matches!(
            menu.handle_key(key(KeyCode::Char('s'))),
            MenuResult::Continue
        ));
        assert_eq!(menu.message(), format!("Stop requested for {id}."));
        let waited = m.wait(&id, 5.0).await;
        assert_eq!(waited["state"], serde_json::json!("stopped"), "{waited}");

        // A second `s` has nothing live left to signal and says so.
        menu.handle_key(key(KeyCode::Char('s')));
        assert_eq!(
            menu.message(),
            format!("No running owned terminal found for {id}.")
        );
    }

    #[tokio::test]
    async fn ctrl_x_is_the_same_stop_as_s() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m, "sleeper").await;
        let mut menu = TerminalListMenu::new(Arc::clone(&m), Some(id.clone()));
        let mut ctrl_x = key(KeyCode::Char('x'));
        ctrl_x.modifiers = KeyModifiers::CONTROL;
        menu.handle_key(ctrl_x);
        assert_eq!(menu.message(), format!("Stop requested for {id}."));
        let waited = m.wait(&id, 5.0).await;
        assert_eq!(waited["state"], serde_json::json!("stopped"), "{waited}");
    }

    /// The selection is pinned to an id, and `/ps <id>` pre-selects it.
    #[tokio::test]
    async fn navigation_and_preselection_follow_terminal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let first = start(&m, "first").await;
        let second = start(&m, "second").await;
        // Records are newest first.
        let menu = TerminalListMenu::new(Arc::clone(&m), Some(first.clone()));
        assert_eq!(menu.selected_id(), Some(first.as_str()));

        let mut menu = TerminalListMenu::new(Arc::clone(&m), None);
        assert_eq!(menu.selected_id(), Some(second.as_str()), "newest first");
        menu.handle_key(key(KeyCode::Char('j')));
        assert_eq!(menu.selected_id(), Some(first.as_str()));
        menu.handle_key(key(KeyCode::Down));
        assert_eq!(
            menu.selected_id(),
            Some(first.as_str()),
            "clamped at the end"
        );
        menu.handle_key(key(KeyCode::Home));
        assert_eq!(menu.selected_id(), Some(second.as_str()));

        let lines = menu
            .overlay_lines(120, crate::menu::OVERLAY_MAX_ROWS)
            .unwrap();
        let selected: Vec<_> = lines
            .iter()
            .filter(|l| l.style == OverlayStyle::Selected)
            .collect();
        assert_eq!(selected.len(), 1);
        assert!(selected[0].text.contains(&second), "{selected:?}");

        assert_eq!(m.request_stop_all(true), 2);
        for id in [&first, &second] {
            m.wait(id, 5.0).await;
        }
    }

    #[tokio::test]
    async fn enter_opens_details_and_esc_backs_out_before_closing() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let id = start(&m, "detail-me").await;
        let mut menu = TerminalListMenu::new(Arc::clone(&m), None);

        menu.handle_key(key(KeyCode::Enter));
        assert!(menu.in_details());
        let text = texts(
            &menu
                .overlay_lines(120, crate::menu::OVERLAY_MAX_ROWS)
                .unwrap(),
        )
        .join("\n");
        assert!(text.contains(&format!("Terminal Details · {id}")), "{text}");
        assert!(text.contains("Esc back"), "{text}");

        assert!(matches!(
            menu.handle_key(key(KeyCode::Esc)),
            MenuResult::Continue
        ));
        assert!(!menu.in_details());
        assert!(matches!(
            menu.handle_key(key(KeyCode::Esc)),
            MenuResult::Cancel
        ));

        assert!(m.request_stop(&id, true));
        m.wait(&id, 5.0).await;
    }

    #[test]
    fn unrelated_keys_are_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let mut menu = TerminalListMenu::new(manager(dir.path()), None);
        assert!(matches!(
            menu.handle_key(key(KeyCode::Char('q'))),
            MenuResult::Continue
        ));
        assert!(menu.message().is_empty());
    }
}
