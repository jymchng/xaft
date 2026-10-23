//! The interactive background-sessions manager behind `xaft agents`.
//!
//! agenthicc's `agents` (with no flags) opens `run_background_manager`
//! (`tui/workspace/background_manager.py`): a keyboard-driven list of durable
//! background sessions with a detail panel, where Enter attaches the selected
//! session to the foreground TUI. `agents --run RUN` opens the goal-run
//! variant, which lists the run's agents and attaches the run's main session.
//!
//! This is the same control surface over xaft's stores:
//!
//! | key | action |
//! | --- | --- |
//! | `↑`/`k`, `↓`/`j` | select |
//! | `Enter` | attach the selection (the run's main session in run mode) |
//! | `c` | cancel (two-step, as `xaft jobs cancel`) |
//! | `r` | resume (as `xaft jobs resume`) |
//! | `a` | archive |
//! | `d` | move to trash |
//! | `q`, `Esc` | close |
//!
//! The list refreshes every second, as agenthicc's does (`refresh_s=1.0`), so
//! a session that changes state while the manager is open is shown as it is.
//!
//! The state machine ([`ManagerState`]) is pure and fully tested: every key
//! goes through [`ManagerState::handle_key`] and every frame through
//! [`ManagerState::render`]. [`run_manager`] is the only terminal-facing code;
//! it draws those frames with crossterm and feeds it keys.

use std::io::Write as _;
use std::time::Duration;

use crate::jobs::{Job, JobStatus, JobStore};

/// A key the manager understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Move the selection up.
    Up,
    /// Move the selection down.
    Down,
    /// Attach the selection.
    Enter,
    /// Close the manager.
    Escape,
    /// Any other character.
    Char(char),
}

/// What the manager asks its caller to do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerAction {
    /// Keep running.
    Continue,
    /// Close without attaching.
    Quit,
    /// Close and attach this background session.
    Attach(String),
    /// Close and attach this goal run's main session.
    AttachRun(String),
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Background session id.
    pub session_id: String,
    /// Wire status.
    pub status: String,
    /// Title, or the agent role in run mode.
    pub title: String,
    /// Workflow name, `direct`, or the task id in run mode.
    pub flow: String,
    /// Latest activity line.
    pub activity: String,
    /// Working directory.
    pub cwd: String,
}

impl Row {
    /// The row for one background job.
    pub fn from_job(job: &Job) -> Self {
        Self {
            session_id: job.session_id.clone(),
            status: job.status.as_str().to_string(),
            title: job.title.clone(),
            flow: if job.workflow_name.is_empty() {
                "direct".into()
            } else {
                job.workflow_name.clone()
            },
            activity: job.latest_activity.clone(),
            cwd: job.cwd.clone(),
        }
    }
}

/// Which list the manager is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Every current background session.
    Jobs,
    /// The agents of one goal run.
    Run {
        /// The run.
        run_id: String,
        /// Header line describing the run.
        header: String,
    },
}

/// The manager's whole state.
#[derive(Debug, Clone)]
pub struct ManagerState {
    mode: Mode,
    rows: Vec<Row>,
    selected: usize,
    message: String,
}

impl ManagerState {
    /// A manager over the current (non-archived, non-trashed) jobs.
    pub fn for_jobs(store: &JobStore) -> Self {
        Self {
            mode: Mode::Jobs,
            rows: store.list(false, false).iter().map(Row::from_job).collect(),
            selected: 0,
            message: String::new(),
        }
    }

    /// A manager over one goal run's agents.
    pub fn for_run(run_id: &str, header: String, rows: Vec<Row>) -> Self {
        Self {
            mode: Mode::Run {
                run_id: run_id.to_string(),
                header,
            },
            rows,
            selected: 0,
            message: String::new(),
        }
    }

    /// The rows shown.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The selected row index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The status line from the last action.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Re-read the list, keeping the selection on the same session.
    pub fn reload(&mut self, store: &JobStore) {
        if self.mode != Mode::Jobs {
            return;
        }
        let keep = self.rows.get(self.selected).map(|r| r.session_id.clone());
        self.rows = store.list(false, false).iter().map(Row::from_job).collect();
        self.selected = keep
            .and_then(|id| self.rows.iter().position(|r| r.session_id == id))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
    }

    /// Apply one key.
    pub fn handle_key(&mut self, key: Key, store: &mut JobStore) -> ManagerAction {
        match key {
            Key::Up | Key::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                ManagerAction::Continue
            }
            Key::Down | Key::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                ManagerAction::Continue
            }
            Key::Escape | Key::Char('q') => ManagerAction::Quit,
            Key::Enter => match &self.mode {
                Mode::Run { run_id, .. } => ManagerAction::AttachRun(run_id.clone()),
                Mode::Jobs => match self.rows.get(self.selected) {
                    Some(row) => ManagerAction::Attach(row.session_id.clone()),
                    None => {
                        self.message = "No background session is selected.".into();
                        ManagerAction::Continue
                    }
                },
            },
            Key::Char(c @ ('c' | 'r' | 'a' | 'd')) if self.mode == Mode::Jobs => {
                let Some(id) = self.rows.get(self.selected).map(|r| r.session_id.clone()) else {
                    return ManagerAction::Continue;
                };
                let (verb, outcome) = match c {
                    'c' => ("cancel", store.cancel(&id)),
                    'r' => ("resume", store.resume(&id)),
                    'a' => ("archive", store.transition(&id, JobStatus::Archived)),
                    _ => ("delete", store.delete(&id)),
                };
                self.message = match outcome {
                    Ok(job) => format!("{verb}: {} -> {}", job.session_id, job.status),
                    Err(e) => format!("Unable to {verb} {id}: {e}"),
                };
                self.reload(store);
                ManagerAction::Continue
            }
            Key::Char(_) => ManagerAction::Continue,
        }
    }

    /// The frame for a `width` × `height` terminal, one string per line.
    ///
    /// The selected row starts with `> `; [`run_manager`] also highlights it.
    pub fn render(&self, width: usize, height: usize) -> Vec<String> {
        let width = width.max(20);
        let mut lines = Vec::new();
        match &self.mode {
            Mode::Jobs => lines.push(format!("Background sessions ({})", self.rows.len())),
            Mode::Run { header, .. } => lines.push(header.clone()),
        }
        lines.push(format!(
            "  {:<16}  {:<16}  {:<28}  {}",
            "ID", "STATUS", "TITLE", "FLOW"
        ));
        // Header, column titles, two separators, two detail lines, footer and
        // message: everything else is list.
        let budget = height.saturating_sub(8).max(1);
        let start = if self.selected >= budget {
            self.selected + 1 - budget
        } else {
            0
        };
        if self.rows.is_empty() {
            lines.push(match self.mode {
                Mode::Jobs => "  No background sessions.".into(),
                Mode::Run { .. } => "  No agents are linked to this run.".into(),
            });
        }
        for (index, row) in self.rows.iter().enumerate().skip(start).take(budget) {
            let marker = if index == self.selected { "> " } else { "  " };
            let id: String = row.session_id.chars().take(16).collect();
            let title: String = row.title.chars().take(28).collect();
            lines.push(format!(
                "{marker}{id:<16}  {status:<16}  {title:<28}  {flow}",
                status = row.status,
                flow = row.flow
            ));
        }
        lines.push("─".repeat(width.min(80)));
        match self.rows.get(self.selected) {
            Some(row) => {
                lines.push(format!("Activity: {}", row.activity));
                lines.push(format!("Dir: {}", row.cwd));
            }
            None => {
                lines.push(String::new());
                lines.push(String::new());
            }
        }
        lines.push("─".repeat(width.min(80)));
        lines.push(match self.mode {
            Mode::Jobs => {
                "↑/↓ select · enter attach · c cancel · r resume · a archive · d delete · q quit"
                    .into()
            }
            Mode::Run { .. } => "↑/↓ select · enter attach the run's main session · q quit".into(),
        });
        lines.push(self.message.clone());
        lines
            .into_iter()
            .map(|line| line.chars().take(width).collect())
            .collect()
    }
}

/// Run the manager on the real terminal until it closes.
///
/// The terminal is restored on every exit path, including an error.
pub fn run_manager(
    state: &mut ManagerState,
    store: &mut JobStore,
) -> std::io::Result<ManagerAction> {
    use crossterm::event::{Event, KeyCode, KeyEventKind};
    use crossterm::{cursor, execute, terminal};

    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let outcome = (|| -> std::io::Result<ManagerAction> {
        loop {
            let (columns, rows) = terminal::size().unwrap_or((80, 24));
            draw(state, &mut stdout, columns as usize, rows as usize)?;
            if !crossterm::event::poll(Duration::from_secs(1))? {
                state.reload(store);
                continue;
            }
            let Event::Key(event) = crossterm::event::read()? else {
                continue;
            };
            if event.kind != KeyEventKind::Press {
                continue;
            }
            let key = match event.code {
                KeyCode::Up => Key::Up,
                KeyCode::Down => Key::Down,
                KeyCode::Enter => Key::Enter,
                KeyCode::Esc => Key::Escape,
                KeyCode::Char(c) => Key::Char(c),
                _ => continue,
            };
            match state.handle_key(key, store) {
                ManagerAction::Continue => {}
                other => return Ok(other),
            }
        }
    })();
    let _ = execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    outcome
}

fn draw(
    state: &ManagerState,
    out: &mut std::io::Stdout,
    width: usize,
    height: usize,
) -> std::io::Result<()> {
    use crossterm::{cursor, queue, terminal};
    queue!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    for (index, line) in state.render(width, height).into_iter().enumerate() {
        queue!(out, cursor::MoveTo(0, index as u16))?;
        if line.starts_with("> ") {
            write!(out, "\x1b[7m{line}\x1b[0m")?;
        } else {
            write!(out, "{line}")?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(jobs: &[(&str, JobStatus)]) -> (tempfile::TempDir, JobStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path()).unwrap();
        for (id, status) in jobs {
            let mut job = Job::new(*id, "secret intent");
            job.title = format!("title {id}");
            job.cwd = format!("/work/{id}");
            job.latest_activity = format!("doing {id}");
            job.status = *status;
            store.create(job).unwrap();
        }
        (dir, store)
    }

    #[test]
    fn the_list_shows_current_jobs_and_moves_the_selection() {
        let (_dir, mut store) =
            store_with(&[("a1", JobStatus::Running), ("b2", JobStatus::Completed)]);
        let mut state = ManagerState::for_jobs(&store);
        let frame = state.render(100, 20);
        assert_eq!(frame[0], "Background sessions (2)");
        assert!(frame[2].starts_with("> a1"), "{frame:?}");
        assert!(frame.iter().any(|l| l == "Activity: doing a1"));
        assert!(
            !frame.iter().any(|l| l.contains("secret intent")),
            "intent must never render"
        );

        assert_eq!(
            state.handle_key(Key::Down, &mut store),
            ManagerAction::Continue
        );
        assert_eq!(state.selected(), 1);
        assert_eq!(
            state.handle_key(Key::Down, &mut store),
            ManagerAction::Continue
        );
        assert_eq!(state.selected(), 1, "the selection stops at the last row");
        assert_eq!(
            state.handle_key(Key::Enter, &mut store),
            ManagerAction::Attach("b2".into())
        );
        assert_eq!(
            state.handle_key(Key::Char('q'), &mut store),
            ManagerAction::Quit
        );
    }

    #[test]
    fn actions_go_through_the_store_and_report_like_jobs() {
        let (_dir, mut store) = store_with(&[("a1", JobStatus::Running)]);
        let mut state = ManagerState::for_jobs(&store);
        state.handle_key(Key::Char('c'), &mut store);
        assert_eq!(state.message(), "cancel: a1 -> cancelled");
        assert_eq!(store.get("a1", false).unwrap().status, JobStatus::Cancelled);

        state.handle_key(Key::Char('r'), &mut store);
        assert_eq!(state.message(), "resume: a1 -> starting");

        state.handle_key(Key::Char('a'), &mut store);
        assert!(
            state.message().starts_with("Unable to archive a1"),
            "{}",
            state.message()
        );

        state.handle_key(Key::Char('d'), &mut store);
        assert_eq!(state.message(), "delete: a1 -> deleted");
        assert!(
            state.rows().is_empty(),
            "a trashed job leaves the current list"
        );
    }

    #[test]
    fn an_empty_store_says_so_and_enter_does_not_attach() {
        let (_dir, mut store) = store_with(&[]);
        let mut state = ManagerState::for_jobs(&store);
        assert!(
            state
                .render(80, 20)
                .iter()
                .any(|l| l == "  No background sessions.")
        );
        assert_eq!(
            state.handle_key(Key::Enter, &mut store),
            ManagerAction::Continue
        );
        assert_eq!(state.message(), "No background session is selected.");
    }

    #[test]
    fn run_mode_attaches_the_run_and_ignores_job_actions() {
        let (_dir, mut store) = store_with(&[("a1", JobStatus::Running)]);
        let mut state = ManagerState::for_run(
            "run_1",
            "Goal run run_1 · running · ship it".into(),
            vec![Row::from_job(&store.get("a1", false).unwrap())],
        );
        assert_eq!(
            state.render(80, 20)[0],
            "Goal run run_1 · running · ship it"
        );
        state.handle_key(Key::Char('c'), &mut store);
        assert_eq!(store.get("a1", false).unwrap().status, JobStatus::Running);
        assert_eq!(
            state.handle_key(Key::Enter, &mut store),
            ManagerAction::AttachRun("run_1".into())
        );
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selection_visible() {
        let ids: Vec<String> = (0..30).map(|i| format!("job{i:02}")).collect();
        let jobs: Vec<(&str, JobStatus)> = ids
            .iter()
            .map(|id| (id.as_str(), JobStatus::Queued))
            .collect();
        let (_dir, mut store) = store_with(&jobs);
        let mut state = ManagerState::for_jobs(&store);
        for _ in 0..25 {
            state.handle_key(Key::Down, &mut store);
        }
        let frame = state.render(80, 12);
        assert!(frame.iter().any(|l| l.starts_with("> job25")), "{frame:?}");
        assert!(frame.len() <= 12 + 2);
    }

    #[test]
    fn reload_keeps_the_same_session_selected() {
        let (_dir, mut store) = store_with(&[("a1", JobStatus::Queued), ("b2", JobStatus::Queued)]);
        let mut state = ManagerState::for_jobs(&store);
        state.handle_key(Key::Down, &mut store);
        store.create(Job::new("a0", "x")).unwrap();
        state.reload(&store);
        assert_eq!(state.rows()[state.selected()].session_id, "b2");
    }
}
