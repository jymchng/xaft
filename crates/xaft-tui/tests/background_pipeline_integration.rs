//! Integration tests for PRD 33d — Background Agent Execution (Ctrl+B).
//!
//! Tests cover the state machine for detaching, buffering, completing, and
//! re-attaching background pipelines, exercised via direct `TuiEvent` injection
//! and synthetic key presses. No real terminal or runtime required.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use xaft_runtime::session::{AgentSession, SessionStatus};
use xaft_tui::bridge::TuiEvent;
use xaft_tui::state::{
    AppState, BackgroundStatus, MAX_BUFFERED_MUTATIONS, WorkflowPhase, commit_line_texts,
};
use xaft_tui::transcript::{LineKind, RenderMutation, StyledLine};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ctrl_b() -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char('b'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn digit_key(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn make_active_state() -> AppState {
    // Non-empty task → starts in Planning phase (phase.is_active() == true).
    let mut s = AppState::new("refactor the auth module");
    s.task_start_time = Some(Instant::now());
    s
}

fn fake_session() -> AgentSession {
    use std::path::PathBuf;
    let mut s = AgentSession::new(
        "test task",
        PathBuf::from("."),
        "default".to_string(),
        "claude-3-5-sonnet-20241022".to_string(),
    );
    s.status = SessionStatus::Completed {
        summary: "done".to_string(),
    };
    s
}

fn task_complete_event() -> TuiEvent {
    TuiEvent::TaskComplete {
        summary: "completed successfully".to_string(),
        session: fake_session(),
    }
}

fn commit_texts(s: &AppState) -> Vec<&str> {
    commit_line_texts(&s.mutations)
}

// ── AC1: Ctrl+B detaches pipeline and unlocks the input bar ───────────────────

#[test]
fn ctrl_b_detaches_active_pipeline_and_sets_idle() {
    let mut s = make_active_state();
    assert!(s.phase.is_active(), "pre-condition: phase must be active");
    assert!(!s.bg_mode);

    s.handle_event(TuiEvent::Key(ctrl_b()));

    assert_eq!(
        s.phase,
        WorkflowPhase::Idle,
        "phase must become Idle after detach"
    );
    assert!(s.bg_mode, "bg_mode must be true after detach");
    assert_eq!(s.background_entries.len(), 1);
    assert_eq!(s.background_entries[0].status, BackgroundStatus::Running);

    // Transcript must include the detach notification.
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("moved to background")),
        "detach notification not found in: {texts:?}"
    );
    // accepts_new_task() must now be true.
    assert!(s.accepts_new_task());
}

#[test]
fn ctrl_b_noop_when_phase_idle() {
    let mut s = AppState::new(""); // Idle
    s.handle_event(TuiEvent::Key(ctrl_b()));
    // No background entries created, no "No background pipelines" yet (that's
    // only when entries == 0 AND phase is idle — the re-attach path).
    assert!(s.bg_mode == false);
    // The "No background pipelines." message fires on the re-attach path.
    let texts = commit_texts(&s);
    assert!(texts.iter().any(|t| t.contains("No background pipelines.")));
}

// ── AC2: Detached events routed to buffer, not transcript ─────────────────────

#[test]
fn agent_output_buffered_when_bg_mode() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    s.mutations.clear(); // clear detach notification

    s.handle_event(TuiEvent::AgentOutput {
        agent_name: "coder".to_string(),
        content: "refactoring now…".to_string(),
    });

    // Nothing should appear in the main transcript.
    assert!(
        commit_texts(&s).is_empty(),
        "agent output must NOT reach main transcript while in bg mode"
    );
    // Must appear in the background buffer.
    let bg = &s.background_entries[0];
    let buf_texts: Vec<&str> = bg
        .buffered
        .iter()
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        buf_texts.iter().any(|t| t.contains("refactoring now")),
        "agent output must be buffered: {buf_texts:?}"
    );
}

#[test]
fn tool_started_buffered_when_bg_mode() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b()));
    s.mutations.clear();

    s.handle_event(TuiEvent::ToolStarted {
        tool_name: "read_file".to_string(),
        tool_use_id: "tu-1".to_string(),
        input: serde_json::json!({"path": "src/auth.rs"}),
        started_at: Instant::now(),
    });

    assert!(
        commit_texts(&s).is_empty(),
        "tool start must NOT reach main transcript while in bg mode"
    );
    let bg = &s.background_entries[0];
    assert!(!bg.buffered.is_empty(), "tool start must be buffered");
}

// ── AC3: Ctrl+B re-attaches single background pipeline ───────────────────────

#[test]
fn ctrl_b_reattaches_and_replays_buffered_output() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach

    // Simulate 3 buffered lines.
    for i in 0..3 {
        s.background_entries[0]
            .buffered
            .push(RenderMutation::CommitLine(StyledLine::new(
                format!("  buffered line {i}"),
                LineKind::AgentText,
            )));
    }

    // Return to Idle so re-attach path triggers.
    s.phase = WorkflowPhase::Idle;
    s.bg_mode = false; // simulate: phase became idle without bg completing yet
    // Actually let's go through the proper Idle + bg_entries path:
    // Reset state manually to simulate the re-attach scenario.
    s.bg_mode = false;
    s.mutations.clear();

    s.handle_event(TuiEvent::Key(ctrl_b())); // re-attach

    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("buffered line 0")),
        "buffered lines must be replayed: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("buffered line 2")),
        "all buffered lines must be replayed: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Resumed") || t.contains("Replayed")),
        "re-attach label must be present: {texts:?}"
    );
    assert_eq!(
        s.background_entries.len(),
        0,
        "entry must be removed after re-attach"
    );
}

// ── AC4: Multiple bg pipelines → numbered selection list ──────────────────────

#[test]
fn ctrl_b_shows_selection_list_for_multiple_bg_pipelines() {
    let mut s = AppState::new(""); // Idle
    // Insert two completed bg entries directly.
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 1,
        task_summary: "task alpha".to_string(),
        started_at: Instant::now() - Duration::from_secs(30),
        buffered: vec![],
        status: BackgroundStatus::Completed,
        truncated: false,
    });
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 2,
        task_summary: "task beta".to_string(),
        started_at: Instant::now() - Duration::from_secs(10),
        buffered: vec![],
        status: BackgroundStatus::Completed,
        truncated: false,
    });

    s.handle_event(TuiEvent::Key(ctrl_b()));

    assert!(s.awaiting_bg_select, "awaiting_bg_select must be true");
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("task alpha")),
        "selection list must include first entry: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("task beta")),
        "selection list must include second entry: {texts:?}"
    );
}

#[test]
fn digit_key_selects_bg_pipeline() {
    let mut s = AppState::new(""); // Idle
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 1,
        task_summary: "first bg task".to_string(),
        started_at: Instant::now(),
        buffered: vec![RenderMutation::CommitLine(StyledLine::new(
            "  bg output line".to_string(),
            LineKind::AgentText,
        ))],
        status: BackgroundStatus::Completed,
        truncated: false,
    });
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 2,
        task_summary: "second bg task".to_string(),
        started_at: Instant::now(),
        buffered: vec![],
        status: BackgroundStatus::Completed,
        truncated: false,
    });
    s.awaiting_bg_select = true;

    // Press '1' to select the first pipeline.
    s.handle_event(TuiEvent::Key(digit_key('1')));

    assert!(!s.awaiting_bg_select);
    assert_eq!(
        s.background_entries.len(),
        1,
        "selected entry must be removed"
    );
    assert_eq!(s.background_entries[0].task_summary, "second bg task");
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("bg output line")),
        "selected pipeline buffer must be replayed: {texts:?}"
    );
}

// ── AC5: Background completion notification ───────────────────────────────────

#[test]
fn task_complete_in_bg_mode_shows_bg_done_notification() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    s.mutations.clear();

    s.handle_event(task_complete_event());

    assert!(!s.bg_mode, "bg_mode must be cleared after completion");
    assert_eq!(s.background_entries[0].status, BackgroundStatus::Completed);
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("[bg done]")),
        "[bg done] notification not found: {texts:?}"
    );
    // phase should go to Done, task_done = true (no new task was queued).
    assert_eq!(s.phase, WorkflowPhase::Done);
    assert!(s.task_done);
}

#[test]
fn runtime_error_in_bg_mode_shows_bg_failed_notification() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    s.mutations.clear();

    s.handle_event(TuiEvent::RuntimeError("out of memory".to_string()));

    assert!(!s.bg_mode);
    assert_eq!(s.background_entries[0].status, BackgroundStatus::Failed);
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("[bg failed]")),
        "[bg failed] notification not found: {texts:?}"
    );
}

// ── AC6: Max background tasks enforced ───────────────────────────────────────

#[test]
fn exceeding_max_background_tasks_rejected() {
    let mut s = make_active_state();
    // Set a limit of 1 via direct config modification (default is 2).
    // Insert a running bg entry manually to simulate being at the limit.
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 99,
        task_summary: "existing bg task".to_string(),
        started_at: Instant::now(),
        buffered: vec![],
        status: BackgroundStatus::Running,
        truncated: false,
    });
    s.background_entries.push(xaft_tui::state::BackgroundEntry {
        id: 100,
        task_summary: "existing bg task 2".to_string(),
        started_at: Instant::now(),
        buffered: vec![],
        status: BackgroundStatus::Running,
        truncated: false,
    });
    // Default max is 2; we already have 2 running → next Ctrl+B must be rejected.
    s.mutations.clear();

    s.handle_event(TuiEvent::Key(ctrl_b()));

    let texts = commit_texts(&s);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Max background tasks reached")),
        "rejection message not found: {texts:?}"
    );
    // Must still have only the 2 pre-existing entries (none added).
    assert_eq!(s.background_entries.len(), 2);
}

// ── AC9: Buffer truncation at MAX_BUFFERED_MUTATIONS ─────────────────────────

#[test]
fn buffer_truncation_at_cap() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    s.mutations.clear();

    // Send MAX_BUFFERED_MUTATIONS + 10 events.
    let total = MAX_BUFFERED_MUTATIONS + 10;
    for i in 0..total {
        s.handle_event(TuiEvent::AgentOutput {
            agent_name: "coder".to_string(),
            content: format!("output line {i}"),
        });
    }

    let bg = &s.background_entries[0];
    assert!(
        bg.buffered.len() <= MAX_BUFFERED_MUTATIONS + 1,
        "buffer must not grow beyond cap+sentinel: got {}",
        bg.buffered.len()
    );
    assert!(bg.truncated, "truncated flag must be set");

    // Exactly one truncation sentinel must be present.
    let sentinel_count = bg
        .buffered
        .iter()
        .filter(|m| match m {
            RenderMutation::CommitLine(l) => l.text.contains("truncated"),
            _ => false,
        })
        .count();
    assert_eq!(
        sentinel_count, 1,
        "exactly one truncation sentinel expected"
    );
}

// ── AC7: Approval request from background prints hint ────────────────────────

#[test]
fn approval_request_from_background_prints_hint() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    s.mutations.clear();

    s.handle_event(TuiEvent::ToolPendingApproval {
        agent_run_id: "run-1".to_string(),
        tool_name: "bash_exec".to_string(),
        tool_use_id: "tu-1".to_string(),
        input: serde_json::json!({"command": "rm -rf /tmp/work"}),
        risk: xaft_tui::approval::RiskLevel::High,
    });

    // The hint must appear in the MAIN transcript (not buffered).
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("[bg] Task paused")),
        "[bg] approval hint not found in main transcript: {texts:?}"
    );
    // The approval queue must still hold the pending entry so it resolves on re-attach.
    assert!(s.approval_queue.has_pending());
}

// ── AC8: Quit warning with running background pipelines ──────────────────────

#[test]
fn quit_with_running_bg_pipelines_shows_warning_first() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach → bg entry Running
    s.phase = WorkflowPhase::Done; // let q key pass the guard
    s.task_done = true;
    s.mutations.clear();

    // First Ctrl+Q press → warning (Ctrl+Q bypasses the input bar).
    s.handle_event(TuiEvent::Key(KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }));

    assert!(
        !s.should_quit,
        "should not quit on first press with running bg"
    );
    let texts = commit_texts(&s);
    assert!(
        texts.iter().any(|t| t.contains("background pipeline")),
        "warning must mention background pipelines: {texts:?}"
    );

    // Second Ctrl+Q → quit proceeds.
    s.mutations.clear();
    s.handle_event(TuiEvent::Key(KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }));
    assert!(s.should_quit, "should quit on second press");
}

// ── accepts_new_task during bg mode ──────────────────────────────────────────

#[test]
fn accepts_new_task_true_in_bg_mode() {
    let mut s = make_active_state();
    assert!(!s.accepts_new_task(), "must not accept while active");
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach
    assert!(s.accepts_new_task(), "must accept new task after bg detach");
}

// ── bg_new_task_sent prevents premature task_done ────────────────────────────

#[test]
fn bg_completion_with_queued_task_does_not_set_task_done() {
    let mut s = make_active_state();
    s.handle_event(TuiEvent::Key(ctrl_b())); // detach

    // Simulate: user queued a new task while bg was running.
    s.bg_new_task_sent = true;
    s.mutations.clear();

    // Background pipeline completes.
    s.handle_event(task_complete_event());

    assert!(
        !s.task_done,
        "task_done must NOT be set when a new task was already queued"
    );
    assert!(!s.bg_new_task_sent, "bg_new_task_sent must be reset");
    assert!(!s.bg_mode);
    // [bg done] notification must still appear.
    let texts = commit_texts(&s);
    assert!(texts.iter().any(|t| t.contains("[bg done]")));
}
