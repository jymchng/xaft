//! The `/loops` overlay — every persisted loop job, with run-now and delete.
//!
//! A port of agenthicc's `LoopJobsOverlay`
//! (`tui/workspace/overlays/loops.py`). The overlay only presents records and
//! translates keys; the [`LoopManager`] owns scheduling and the one-owner
//! session rule, so `Enter` can never start a second execution path — it asks
//! the scheduler to make the job due, and the scheduler dispatches it through
//! the normal input path on the next idle tick.
//!
//! | key | action |
//! |---|---|
//! | `↑`/`k`, `↓`/`j`, `PgUp`/`PgDn`, `Home`/`End` | select |
//! | `Enter` | run the selected job now, then close |
//! | `d` | delete — confirm with `Enter`/`y`, cancel with `Esc`/`n` |
//! | `r` | refresh |
//! | `Esc` | close |

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::manager::LoopManager;
use super::model::{LoopRecord, format_loop_duration, redacted_preview};
use crate::menu::{MenuPayload, MenuResult, MenuWidget, OverlayLine, OverlayStyle};

/// Fixed rows around the list: title, three detail rows, message, footer.
const LIST_OVERHEAD_ROWS: usize = 6;

/// `YYYY-MM-DD HH:MM` in local time, as agenthicc's `_format_time` prints it.
fn format_time(epoch: f64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(epoch as i64, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}

fn page_size(max_rows: usize) -> usize {
    max_rows.saturating_sub(LIST_OVERHEAD_ROWS).max(1)
}

fn lock(manager: &Mutex<LoopManager>) -> std::sync::MutexGuard<'_, LoopManager> {
    manager.lock().unwrap_or_else(|e| e.into_inner())
}

/// The `/loops` overlay.
pub struct LoopJobsMenu {
    manager: Arc<Mutex<LoopManager>>,
    selected: usize,
    selected_id: Option<String>,
    confirm_delete: bool,
    message: String,
}

impl LoopJobsMenu {
    /// Open the overlay over the session's loop manager.
    pub fn new(manager: Arc<Mutex<LoopManager>>) -> Self {
        let mut menu = Self {
            manager,
            selected: 0,
            selected_id: None,
            confirm_delete: false,
            message: String::new(),
        };
        let records = menu.records();
        menu.reselect(&records);
        menu
    }

    /// The latest durable projection, newest first.
    pub fn records(&self) -> Vec<LoopRecord> {
        lock(&self.manager).list_jobs()
    }

    /// The loop id under the cursor, if any.
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// The feedback line from the last action.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether a delete is waiting for confirmation.
    pub fn confirming_delete(&self) -> bool {
        self.confirm_delete
    }

    /// Keep the selection on the same job across a refresh.
    fn reselect(&mut self, records: &[LoopRecord]) {
        if let Some(id) = &self.selected_id {
            if let Some(pos) = records.iter().position(|r| &r.loop_id == id) {
                self.selected = pos;
                return;
            }
        }
        self.selected = self.selected.min(records.len().saturating_sub(1));
        self.selected_id = records.get(self.selected).map(|r| r.loop_id.clone());
    }

    fn move_to(&mut self, records: &[LoopRecord], index: isize) {
        if records.is_empty() {
            self.selected = 0;
            self.selected_id = None;
            return;
        }
        let clamped = index.clamp(0, records.len() as isize - 1) as usize;
        self.selected = clamped;
        self.selected_id = Some(records[clamped].loop_id.clone());
    }

    fn selected_record(&self, records: &[LoopRecord]) -> Option<LoopRecord> {
        records.get(self.selected).cloned()
    }
}

impl MenuWidget for LoopJobsMenu {
    fn title(&self) -> &str {
        "Scheduled Loops"
    }

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
        let records = self.records();
        self.reselect(&records);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if self.confirm_delete {
            match key.code {
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.confirm_delete = false;
                    self.message = "Delete cancelled.".to_string();
                }
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm_delete = false;
                    if let Some(record) = self.selected_record(&records) {
                        self.message = lock(&self.manager).delete_job(&record);
                        let refreshed = self.records();
                        self.selected_id = None;
                        self.reselect(&refreshed);
                    }
                }
                _ => {}
            }
            return MenuResult::Continue;
        }

        let page = page_size(crate::menu::OVERLAY_MAX_ROWS) as isize;
        let at = self.selected as isize;
        match key.code {
            KeyCode::Esc => return MenuResult::Cancel,
            KeyCode::Up => self.move_to(&records, at - 1),
            KeyCode::Down => self.move_to(&records, at + 1),
            KeyCode::Char('k') | KeyCode::Char('K') if !ctrl => self.move_to(&records, at - 1),
            KeyCode::Char('j') | KeyCode::Char('J') if !ctrl => self.move_to(&records, at + 1),
            KeyCode::PageUp => self.move_to(&records, at - page),
            KeyCode::PageDown => self.move_to(&records, at + page),
            KeyCode::Home => self.move_to(&records, 0),
            KeyCode::End => self.move_to(&records, records.len() as isize - 1),
            KeyCode::Char('d') | KeyCode::Char('D')
                if !ctrl && self.selected_record(&records).is_some() =>
            {
                self.confirm_delete = true;
                self.message.clear();
            }
            KeyCode::Char('r') | KeyCode::Char('R') if !ctrl => {
                // The projection is re-read on every key and every frame; an
                // explicit refresh only clears stale feedback.
                self.message.clear();
            }
            KeyCode::Enter => {
                if let Some(record) = self.selected_record(&records) {
                    let message = lock(&self.manager).run_job_now(&record);
                    // The job is now the scheduler's: close, and say what
                    // happened in the transcript where the user is looking.
                    return MenuResult::Done(MenuPayload::Notice(message));
                }
            }
            _ => {}
        }
        MenuResult::Continue
    }

    fn overlay_lines(&self, cols: u16, max_rows: usize) -> Option<Vec<OverlayLine>> {
        let _ = cols;
        let (records, session_id) = {
            let manager = lock(&self.manager);
            (manager.list_jobs(), manager.session_id().to_string())
        };
        let index = self
            .selected_id
            .as_ref()
            .and_then(|id| records.iter().position(|r| &r.loop_id == id))
            .unwrap_or_else(|| self.selected.min(records.len().saturating_sub(1)));
        let size = page_size(max_rows);
        let page = index / size;
        let pages = records.len().div_ceil(size).max(1);
        let start = (page * size).min(records.len());
        let end = (start + size).min(records.len());

        let mut lines = vec![OverlayLine::new(
            format!(
                "Scheduled Loops (page {}/{pages}; {})",
                page + 1,
                records.len()
            ),
            OverlayStyle::Title,
        )];
        if records.is_empty() {
            lines.push(OverlayLine::new(
                "  —  —  —  —  —  No scheduled loop jobs",
                OverlayStyle::Dim,
            ));
        }
        for (offset, record) in records[start..end].iter().enumerate() {
            let at = start + offset;
            let next = if record.pending {
                "now".to_string()
            } else {
                format_time(record.next_due_at)
            };
            let session = if record.session_id == session_id {
                "current".to_string()
            } else {
                record.session_id.chars().take(12).collect()
            };
            let row = format!(
                "{} {:<12} {:<16} {:<5} {:<16} {:<12} {}",
                if at == index { "▶" } else { " " },
                record.short_id(),
                record.state.as_str(),
                format_loop_duration(record.interval_s as f64),
                next,
                session,
                redacted_preview(&record.payload, 72)
            );
            lines.push(OverlayLine::new(
                row,
                if at == index {
                    OverlayStyle::Selected
                } else {
                    OverlayStyle::Normal
                },
            ));
        }
        match records.get(index) {
            Some(selected) => {
                lines.push(OverlayLine::new(
                    format!(
                        "Job {}  ·  Session {}",
                        selected.loop_id, selected.session_id
                    ),
                    OverlayStyle::Dim,
                ));
                lines.push(OverlayLine::new(
                    format!(
                        "State {}  ·  Runs {}  ·  Failures {}",
                        selected.state.as_str(),
                        selected.runs,
                        selected.failures
                    ),
                    OverlayStyle::Dim,
                ));
                lines.push(OverlayLine::new(
                    format!("Payload {}", redacted_preview(&selected.payload, 220)),
                    OverlayStyle::Dim,
                ));
            }
            None => lines.push(OverlayLine::new(
                "No scheduled loop jobs are available.",
                OverlayStyle::Dim,
            )),
        }
        if self.confirm_delete {
            lines.push(OverlayLine::new(
                "Delete this job permanently? Press Enter/y to confirm or Esc/n to cancel.",
                OverlayStyle::Warning,
            ));
        } else if !self.message.is_empty() {
            lines.push(OverlayLine::new(self.message.clone(), OverlayStyle::Dim));
        }
        lines.push(OverlayLine::new(
            "↑↓/j/k select   PgUp/PgDn page   Home/End   Enter run now   d delete   r refresh   Esc close",
            OverlayStyle::Dim,
        ));
        lines.truncate(max_rows);
        Some(lines)
    }
}
