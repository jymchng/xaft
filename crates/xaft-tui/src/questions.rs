//! `ask_user` in the TUI: the questions overlay and the question gate behind
//! it — agenthicc's `QuestionsOverlay` (PRD-100).
//!
//! An agent calls `ask_user` with `{id, text, options}` questions. The
//! [`TuiQuestionGate`] posts them to the event loop as
//! [`TuiEvent::QuestionsAsked`](crate::bridge::TuiEvent) and waits, up to
//! `tools.question_timeout_s`, for the answers the overlay sends back through
//! [`TuiQuestionGate::respond`].
//!
//! In the overlay ←/→ move between questions and ↑/↓ between options (which
//! wrap); every question also offers "Other — type your answer". Enter
//! confirms the highlighted option (or opens the free-text editor for
//! "Other") and moves to the next unanswered question; Enter on the last one
//! submits every answer at once. Esc cancels the whole round — or, while
//! typing, returns to the options.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::{Mutex, mpsc, oneshot};
use xaft_runtime::phased::question::{QuestionOutcome, UserQuestion, UserQuestionGate};

use crate::bridge::TuiEvent;
use crate::menu::{OverlayLine, OverlayStyle};

/// The free-text choice appended to every question's options.
pub const OTHER_LABEL: &str = "Other — type your answer";

/// agenthicc's `DEFAULT_QUESTION_TIMEOUT_S`.
pub const DEFAULT_QUESTION_TIMEOUT_S: f64 = 300.0;

/// Rows the question text may take in the overlay.
const QUESTION_ROWS: usize = 3;

// ── Gate ─────────────────────────────────────────────────────────────────────

/// The TUI's [`UserQuestionGate`]: asks through the questions overlay.
pub struct TuiQuestionGate {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Option<Vec<String>>>>>>,
    events: mpsc::UnboundedSender<TuiEvent>,
    timeout: Duration,
    next_id: AtomicU64,
}

impl TuiQuestionGate {
    /// A gate posting to the event loop behind `events`, waiting at most
    /// `timeout_s` seconds (`tools.question_timeout_s`) for an answer. A
    /// non-finite or non-positive value falls back to agenthicc's default.
    pub fn new(events: mpsc::UnboundedSender<TuiEvent>, timeout_s: f64) -> Self {
        let timeout_s = if timeout_s.is_finite() && timeout_s > 0.0 {
            timeout_s
        } else {
            DEFAULT_QUESTION_TIMEOUT_S
        };
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            events,
            timeout: Duration::from_secs_f64(timeout_s),
            next_id: AtomicU64::new(1),
        }
    }

    /// Deliver the answers for request `id` (`None` = the user cancelled).
    /// Returns whether a request was waiting for them.
    pub async fn respond(&self, id: &str, answers: Option<Vec<String>>) -> bool {
        match self.pending.lock().await.remove(id) {
            Some(tx) => tx.send(answers).is_ok(),
            None => false,
        }
    }

    /// Cancel every outstanding round (`/cancel`, shutdown).
    pub async fn cancel_all(&self) {
        for (_, tx) in self.pending.lock().await.drain() {
            let _ = tx.send(None);
        }
    }

    /// Whether a round is waiting for the user.
    pub async fn has_pending(&self) -> bool {
        !self.pending.lock().await.is_empty()
    }
}

#[async_trait]
impl UserQuestionGate for TuiQuestionGate {
    async fn ask(&self, questions: &[UserQuestion]) -> QuestionOutcome {
        if questions.is_empty() {
            return QuestionOutcome::Answered(Vec::new());
        }
        let id = format!("q-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        let asked = TuiEvent::QuestionsAsked {
            id: id.clone(),
            questions: questions.to_vec(),
            timeout_s: self.timeout.as_secs_f64(),
        };
        if self.events.send(asked).is_err() {
            // The TUI is gone: nobody can answer.
            self.pending.lock().await.remove(&id);
            return QuestionOutcome::Cancelled;
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(Some(answers))) => QuestionOutcome::Answered(answers),
            Ok(Ok(None)) | Ok(Err(_)) => QuestionOutcome::Cancelled,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                let _ = self.events.send(TuiEvent::QuestionsExpired { id });
                QuestionOutcome::TimedOut {
                    timeout_s: self.timeout.as_secs_f64(),
                }
            }
        }
    }
}

// ── Overlay ──────────────────────────────────────────────────────────────────

/// Per-question selection state.
#[derive(Debug, Clone, Default)]
struct QState {
    cursor: usize,
    answer: String,
    answered: bool,
    opt_scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Selecting,
    Typing,
}

/// What a key did to the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionsKey {
    /// Still open.
    Continue,
    /// Every question is answered: one answer per question, in order.
    Submit(Vec<String>),
    /// The user dismissed the round.
    Cancel,
}

/// One round of `ask_user` questions on screen.
#[derive(Debug, Clone)]
pub struct QuestionsOverlay {
    /// The gate's request id.
    pub id: String,
    questions: Vec<UserQuestion>,
    states: Vec<QState>,
    current: usize,
    mode: Mode,
    typed: String,
    deadline: Option<Instant>,
}

impl QuestionsOverlay {
    /// Open a round; `timeout_s` drives the "N s remaining" countdown.
    pub fn new(
        id: impl Into<String>,
        questions: Vec<UserQuestion>,
        timeout_s: Option<f64>,
    ) -> Self {
        let deadline = timeout_s
            .filter(|t| t.is_finite() && *t > 0.0)
            .map(|t| Instant::now() + Duration::from_secs_f64(t));
        let states = vec![QState::default(); questions.len()];
        Self {
            id: id.into(),
            questions,
            states,
            current: 0,
            mode: Mode::Selecting,
            typed: String::new(),
            deadline,
        }
    }

    /// The questions, in order.
    pub fn questions(&self) -> &[UserQuestion] {
        &self.questions
    }

    /// The question currently shown.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Whether the free-text editor is open.
    pub fn is_typing(&self) -> bool {
        self.mode == Mode::Typing
    }

    fn options(&self, q: usize) -> Vec<String> {
        let mut opts = self.questions[q].options.clone();
        opts.push(OTHER_LABEL.to_string());
        opts
    }

    fn is_other(&self, q: usize, opt: usize) -> bool {
        opt + 1 == self.options(q).len()
    }

    /// Answered with text that is not one of the preset options.
    fn is_free_text_answer(&self, q: usize) -> bool {
        let st = &self.states[q];
        st.answered && !self.questions[q].options.contains(&st.answer)
    }

    fn all_answered(&self) -> bool {
        self.states.iter().all(|s| s.answered)
    }

    fn answers(&self) -> Vec<String> {
        self.states.iter().map(|s| s.answer.clone()).collect()
    }

    /// Focus the next unanswered question after the current one.
    fn advance(&mut self) {
        let n = self.questions.len();
        for step in 1..=n {
            let idx = (self.current + step) % n;
            if !self.states[idx].answered {
                self.current = idx;
                return;
            }
        }
    }

    /// Record `answer` for the current question; submit when it was the last.
    fn confirm(&mut self, answer: String) -> QuestionsKey {
        let st = &mut self.states[self.current];
        st.answer = answer;
        st.answered = true;
        if self.all_answered() {
            QuestionsKey::Submit(self.answers())
        } else {
            self.advance();
            QuestionsKey::Continue
        }
    }

    /// Apply one key press.
    pub fn handle_key(&mut self, key: KeyEvent) -> QuestionsKey {
        if self.questions.is_empty() {
            return match key.code {
                KeyCode::Esc => QuestionsKey::Cancel,
                _ => QuestionsKey::Continue,
            };
        }
        match self.mode {
            Mode::Selecting => self.handle_selecting(key),
            Mode::Typing => self.handle_typing(key),
        }
    }

    fn handle_selecting(&mut self, key: KeyEvent) -> QuestionsKey {
        let q = self.current;
        let n = self.options(q).len();
        match key.code {
            KeyCode::Esc => return QuestionsKey::Cancel,
            KeyCode::Up => {
                let st = &mut self.states[q];
                st.cursor = (st.cursor + n - 1) % n;
            }
            KeyCode::Down => {
                let st = &mut self.states[q];
                st.cursor = (st.cursor + 1) % n;
            }
            KeyCode::Left => self.current = self.current.saturating_sub(1),
            KeyCode::Right => self.current = (self.current + 1).min(self.questions.len() - 1),
            KeyCode::Enter => {
                let cursor = self.states[q].cursor;
                if self.is_other(q, cursor) {
                    // Re-open a previous free-text answer for editing.
                    self.typed = if self.is_free_text_answer(q) {
                        self.states[q].answer.clone()
                    } else {
                        String::new()
                    };
                    self.mode = Mode::Typing;
                } else {
                    let answer = self.questions[q].options[cursor].clone();
                    return self.confirm(answer);
                }
            }
            _ => {}
        }
        QuestionsKey::Continue
    }

    fn handle_typing(&mut self, key: KeyEvent) -> QuestionsKey {
        match key.code {
            KeyCode::Esc => {
                self.typed.clear();
                self.mode = Mode::Selecting;
            }
            KeyCode::Enter => {
                let text = self.typed.trim().to_string();
                // An empty answer keeps the editor open, as in agenthicc.
                if !text.is_empty() {
                    self.typed.clear();
                    self.mode = Mode::Selecting;
                    return self.confirm(text);
                }
            }
            KeyCode::Backspace => {
                self.typed.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.typed.clear();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.typed.push(c);
            }
            _ => {}
        }
        QuestionsKey::Continue
    }

    /// Text pasted while typing goes into the answer.
    pub fn paste(&mut self, text: &str) {
        if self.mode == Mode::Typing {
            self.typed.push_str(&text.replace(['\r', '\n'], " "));
        }
    }

    /// Seconds left before the gate gives up, for the countdown.
    pub fn remaining_seconds(&self) -> Option<u64> {
        self.deadline.map(|d| {
            d.saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil() as u64
        })
    }

    /// The overlay rows, at most `max_rows`, each fitting `cols` columns.
    pub fn overlay_lines(&self, cols: u16, max_rows: usize) -> Vec<OverlayLine> {
        let width = (cols as usize).saturating_sub(4).max(10);
        let total = self.questions.len();
        let mut lines = Vec::new();
        let answered = self.states.iter().filter(|s| s.answered).count();
        let mut title = format!("❓ Questions ({answered} of {total} answered)");
        if let Some(secs) = self.remaining_seconds() {
            title.push_str(&format!(" · {secs}s remaining"));
        }
        lines.push(OverlayLine::new(title, OverlayStyle::Title));
        if total == 0 {
            lines.push(OverlayLine::new("  No questions.", OverlayStyle::Dim));
            lines.push(OverlayLine::new("  Esc close", OverlayStyle::Dim));
            return lines;
        }
        let q = self.current;
        let left = if q > 0 { "← " } else { "  " };
        let right = if q + 1 < total { " →" } else { "" };
        let dots: Vec<&str> = self
            .states
            .iter()
            .map(|s| if s.answered { "●" } else { "○" })
            .collect();
        lines.push(OverlayLine::new(
            format!(
                "  {left}Question {} of {total}{right}   {}",
                q + 1,
                dots.join(" ")
            ),
            OverlayStyle::Dim,
        ));
        for row in wrap(&self.questions[q].text, width, QUESTION_ROWS) {
            lines.push(OverlayLine::new(format!("  {row}"), OverlayStyle::Normal));
        }
        // One row is the hint; the rest hold options (or the editor).
        let budget = max_rows.saturating_sub(lines.len() + 1).max(1);
        match self.mode {
            Mode::Typing => {
                let shown = tail_chars(&self.typed, width.saturating_sub(4));
                lines.push(OverlayLine::new(
                    format!("  › {shown}▏"),
                    OverlayStyle::Selected,
                ));
                lines.push(OverlayLine::new(
                    "  Enter confirm   Esc back to options",
                    OverlayStyle::Dim,
                ));
            }
            Mode::Selecting => {
                let opts = self.options(q);
                let st = &self.states[q];
                let start = scroll_start(st.cursor, opts.len(), budget, st.opt_scroll);
                for (i, label) in opts.iter().enumerate().skip(start).take(budget) {
                    let label = if self.is_other(q, i) && self.is_free_text_answer(q) {
                        format!("Other: \"{}\"", truncate(&st.answer, 40))
                    } else {
                        label.clone()
                    };
                    let chosen = st.answered
                        && (st.answer == label
                            || (self.is_other(q, i) && self.is_free_text_answer(q)));
                    let mark = if chosen { "✓" } else { " " };
                    let (pointer, style) = if i == st.cursor {
                        ("❯", OverlayStyle::Selected)
                    } else {
                        (" ", OverlayStyle::Normal)
                    };
                    lines.push(OverlayLine::new(
                        format!(
                            "  {pointer} {mark} {}",
                            truncate(&label, width.saturating_sub(6))
                        ),
                        style,
                    ));
                }
                let hint = if self.all_answered() {
                    "  ↑↓ option   ←→ question   Enter SUBMIT ALL   Esc cancel"
                } else {
                    "  ↑↓ option   ←→ question   Enter confirm   Esc cancel"
                };
                lines.push(OverlayLine::new(hint, OverlayStyle::Dim));
            }
        }
        lines.truncate(max_rows.max(1));
        lines
    }
}

/// The first visible option index keeping `cursor` inside a `rows` window.
fn scroll_start(cursor: usize, len: usize, rows: usize, hint: usize) -> usize {
    if len <= rows {
        return 0;
    }
    let mut start = hint.min(len - rows);
    if cursor < start {
        start = cursor;
    } else if cursor >= start + rows {
        start = cursor + 1 - rows;
    }
    start
}

/// Word-wrap `text` to `width` columns, at most `rows` rows (the last one
/// ellipsised when the text is longer).
fn wrap(text: &str, width: usize, rows: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if candidate.chars().count() <= width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        line = word.to_string();
        while line.chars().count() > width {
            let head: String = line.chars().take(width).collect();
            out.push(head);
            line = line.chars().skip(width).collect();
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    if out.len() > rows {
        out.truncate(rows);
        if let Some(last) = out.last_mut() {
            *last = format!("{}…", truncate(last, width.saturating_sub(1)));
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// The last `max` characters of `s` (the editor shows the end being typed).
fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(n - max + 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn two_questions() -> QuestionsOverlay {
        QuestionsOverlay::new(
            "q-1",
            vec![
                UserQuestion::new(
                    "db",
                    "Which database?",
                    vec!["postgres".into(), "sqlite".into()],
                ),
                UserQuestion::new("cache", "Which cache?", vec!["redis".into()]),
            ],
            None,
        )
    }

    #[test]
    fn enter_confirms_and_advances_then_submits_all() {
        let mut o = two_questions();
        assert_eq!(o.handle_key(key(KeyCode::Down)), QuestionsKey::Continue);
        assert_eq!(o.handle_key(key(KeyCode::Enter)), QuestionsKey::Continue);
        assert_eq!(
            o.current(),
            1,
            "focus moves to the next unanswered question"
        );
        assert_eq!(
            o.handle_key(key(KeyCode::Enter)),
            QuestionsKey::Submit(vec!["sqlite".into(), "redis".into()])
        );
    }

    #[test]
    fn up_and_down_wrap_but_left_and_right_clamp() {
        let mut o = two_questions();
        o.handle_key(key(KeyCode::Up));
        // postgres, sqlite, Other → Up from the top lands on Other.
        assert_eq!(o.handle_key(key(KeyCode::Enter)), QuestionsKey::Continue);
        assert!(o.is_typing(), "Other opens the free-text editor");
        o.handle_key(key(KeyCode::Esc));
        assert!(!o.is_typing());
        o.handle_key(key(KeyCode::Left));
        assert_eq!(o.current(), 0);
        o.handle_key(key(KeyCode::Right));
        o.handle_key(key(KeyCode::Right));
        assert_eq!(o.current(), 1);
    }

    #[test]
    fn other_takes_free_text_and_empty_text_keeps_the_editor_open() {
        let mut o =
            QuestionsOverlay::new("q-2", vec![UserQuestion::free_text("why", "Why?")], None);
        // A free-text question has only "Other".
        o.handle_key(key(KeyCode::Enter));
        assert!(o.is_typing());
        assert_eq!(o.handle_key(key(KeyCode::Enter)), QuestionsKey::Continue);
        assert!(o.is_typing(), "an empty answer is not accepted");
        for c in "speed".chars() {
            o.handle_key(key(KeyCode::Char(c)));
        }
        o.handle_key(key(KeyCode::Backspace));
        o.paste("d!");
        assert_eq!(
            o.handle_key(key(KeyCode::Enter)),
            QuestionsKey::Submit(vec!["speed!".into()])
        );
    }

    #[test]
    fn esc_cancels_the_round() {
        let mut o = two_questions();
        assert_eq!(o.handle_key(key(KeyCode::Esc)), QuestionsKey::Cancel);
    }

    #[test]
    fn the_overlay_fits_its_row_budget_and_shows_progress() {
        let mut o = QuestionsOverlay::new(
            "q-3",
            vec![UserQuestion::new(
                "pick",
                "A long question ".repeat(30),
                (1..=12).map(|i| format!("option {i}")).collect(),
            )],
            Some(300.0),
        );
        let rows = o.overlay_lines(80, crate::menu::OVERLAY_MAX_ROWS);
        assert!(rows.len() <= crate::menu::OVERLAY_MAX_ROWS);
        assert!(rows[0].text.contains("0 of 1 answered"), "{}", rows[0].text);
        assert!(rows[0].text.contains("s remaining"), "{}", rows[0].text);
        assert!(rows.iter().all(|r| r.text.chars().count() <= 80));
        // Scrolling to the last option keeps the cursor visible.
        o.handle_key(key(KeyCode::Up));
        let rows = o.overlay_lines(80, crate::menu::OVERLAY_MAX_ROWS);
        assert!(
            rows.iter()
                .any(|r| r.style == OverlayStyle::Selected && r.text.contains(OTHER_LABEL))
        );
    }

    #[tokio::test]
    async fn the_gate_round_trips_answers_through_the_event_loop() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gate = Arc::new(TuiQuestionGate::new(tx, 30.0));
        let asking = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.ask(&[UserQuestion::new("db", "DB?", vec!["pg".into()])])
                    .await
            })
        };
        let Some(TuiEvent::QuestionsAsked {
            id,
            questions,
            timeout_s,
        }) = rx.recv().await
        else {
            panic!("expected QuestionsAsked")
        };
        assert_eq!(questions.len(), 1);
        assert_eq!(timeout_s, 30.0);
        assert!(gate.respond(&id, Some(vec!["pg".into()])).await);
        assert_eq!(
            asking.await.unwrap(),
            QuestionOutcome::Answered(vec!["pg".into()])
        );
        assert!(!gate.respond(&id, None).await, "a round is answered once");
    }

    #[tokio::test]
    async fn cancel_and_timeout_are_distinct_outcomes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let gate = Arc::new(TuiQuestionGate::new(tx, 0.05));
        let q = [UserQuestion::free_text("x", "X?")];
        // Timeout: nobody answers, the overlay is told to close.
        assert_eq!(
            gate.ask(&q).await,
            QuestionOutcome::TimedOut { timeout_s: 0.05 }
        );
        assert!(matches!(
            rx.recv().await,
            Some(TuiEvent::QuestionsAsked { .. })
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TuiEvent::QuestionsExpired { .. })
        ));
        // Cancel: /cancel drops every pending round.
        let (tx, _rx) = mpsc::unbounded_channel();
        let gate = Arc::new(TuiQuestionGate::new(tx, 30.0));
        let asking = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { gate.ask(&[UserQuestion::free_text("x", "X?")]).await })
        };
        while !gate.has_pending().await {
            tokio::task::yield_now().await;
        }
        gate.cancel_all().await;
        assert_eq!(asking.await.unwrap(), QuestionOutcome::Cancelled);
    }

    #[test]
    fn a_bad_timeout_falls_back_to_agenthiccs_default() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let gate = TuiQuestionGate::new(tx, f64::NAN);
        assert_eq!(
            gate.timeout,
            Duration::from_secs_f64(DEFAULT_QUESTION_TIMEOUT_S)
        );
    }
}
