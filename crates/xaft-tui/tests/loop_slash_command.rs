//! `/loop` and `/loops` driven through the live `AppState` (parity rows 3/4).
//!
//! The handler unit tests prove each message; the manager tests prove the
//! schedule. These prove what neither can: that an iteration actually leaves
//! through the same path a typed message takes — the run channel for a prompt,
//! the slash dispatcher for a command — that the run's completion (or
//! failure) is what re-arms (or backs off) the loop, and that the scheduler
//! never dispatches while the session is busy.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;
use xaft_config::LoopsConfig;
use xaft_runtime::UserMessage;
use xaft_runtime::session::AgentSession;
use xaft_tui::bridge::TuiEvent;
use xaft_tui::loops::{LoopLifecycle, LoopManager};
use xaft_tui::state::AppState;
use xaft_tui::transcript::RenderMutation;

fn key(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// Type `text` into the input bar and press Enter, exactly as a user would.
fn submit(state: &mut AppState, text: &str) {
    state.input_bar.set_text(text);
    state.handle_event(key(KeyCode::Enter));
}

fn committed(state: &mut AppState) -> Vec<String> {
    state
        .mutations
        .drain(..)
        .filter_map(|m| match m {
            RenderMutation::CommitLine(l) => Some(l.text),
            _ => None,
        })
        .collect()
}

struct Harness {
    state: AppState,
    runs: mpsc::UnboundedReceiver<UserMessage>,
    manager: Arc<Mutex<LoopManager>>,
    _dir: tempfile::TempDir,
}

fn harness(settings: LoopsConfig) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = LoopManager::new("session-1", settings, dir.path().join("loops"));
    manager.start(false);
    let manager = Arc::new(Mutex::new(manager));
    let (tx, rx) = mpsc::unbounded_channel();
    let mut state = AppState::new("");
    state.user_message_tx = Some(tx);
    state.attach_loop_manager(Some(Arc::clone(&manager)));
    Harness {
        state,
        runs: rx,
        manager,
        _dir: dir,
    }
}

/// What `app.rs` does when it takes a message off the channel and starts a run.
fn accept_run(h: &mut Harness) -> Option<String> {
    let msg = h.runs.try_recv().ok()?;
    h.state.note_task_accepted();
    h.state.task_done = false;
    h.state.phase = xaft_tui::WorkflowPhase::Planning;
    h.state
        .intent_running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    Some(msg.as_text_lossy())
}

/// What `app.rs` does when the run's result arrives.
fn complete_run(h: &mut Harness, result: Result<(), &str>) {
    let event = match result {
        Ok(()) => TuiEvent::TaskComplete {
            summary: "done".into(),
            session: AgentSession::new("t", ".".into(), "default".into(), "m".into()),
        },
        Err(e) => TuiEvent::RuntimeError(e.to_string()),
    };
    h.state.handle_event(event);
    h.state
        .intent_running
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

fn state_of(h: &Harness) -> LoopLifecycle {
    h.manager.lock().unwrap().record().expect("a loop").state
}

#[test]
fn a_prompt_iteration_goes_out_through_the_run_channel_and_completion_re_arms_it() {
    let mut h = harness(LoopsConfig {
        min_interval_s: 1,
        ..LoopsConfig::default()
    });
    submit(&mut h.state, "/loop 1m summarise the open issues");
    let out = committed(&mut h.state);
    assert!(
        out.iter().any(|l| l.contains("↻ Loop scheduled every 1m")),
        "{out:?}"
    );
    assert!(
        h.runs.try_recv().is_err(),
        "creating a loop runs nothing by itself"
    );

    // The scheduler's tick dispatches the due iteration as a normal message.
    h.state.tick_loop_scheduler();
    let out = committed(&mut h.state);
    assert!(
        out.iter().any(|l| l.contains("↻ Loop iteration 1")),
        "{out:?}"
    );
    assert!(
        out.iter().any(|l| l == "❯ summarise the open issues"),
        "{out:?}"
    );
    assert_eq!(state_of(&h), LoopLifecycle::Running);
    assert_eq!(
        accept_run(&mut h).as_deref(),
        Some("summarise the open issues"),
        "the iteration is a real run request"
    );

    // While that run is in flight nothing else is dispatched.
    h.state.tick_loop_scheduler();
    assert!(h.runs.try_recv().is_err(), "no concurrent turn");

    complete_run(&mut h, Ok(()));
    let record = h.manager.lock().unwrap().record().unwrap().clone();
    assert_eq!(record.state, LoopLifecycle::Scheduled);
    assert_eq!(record.runs, 1);
    assert_eq!(record.consecutive_failures, 0);
    assert!(h.state.loop_dispatch.is_none());
}

#[test]
fn a_failed_run_counts_toward_max_consecutive_failures() {
    let mut h = harness(LoopsConfig {
        min_interval_s: 1,
        max_consecutive_failures: 1,
        ..LoopsConfig::default()
    });
    submit(&mut h.state, "/loop 1m try the flaky thing");
    h.state.tick_loop_scheduler();
    accept_run(&mut h).expect("dispatched");
    complete_run(&mut h, Err("provider unavailable"));

    assert_eq!(state_of(&h), LoopLifecycle::Failed);
    let out = committed(&mut h.state);
    assert!(
        out.iter()
            .any(|l| l.contains("Loop stopped after 1 consecutive failures")),
        "{out:?}"
    );
    // The failed run left the session idle again, so the user can keep going.
    assert!(h.state.task_done, "a runtime error ends the run");
    assert!(h.state.loop_session_idle());
}

#[test]
fn a_command_iteration_runs_through_the_slash_dispatcher() {
    let mut h = harness(LoopsConfig {
        min_interval_s: 1,
        ..LoopsConfig::default()
    });
    submit(&mut h.state, "/loop 1m /status");
    committed(&mut h.state);

    h.state.tick_loop_scheduler();
    let out = committed(&mut h.state);
    assert!(out.iter().any(|l| l == "❯ /status"), "{out:?}");
    assert!(
        out.iter().any(|l| l.trim_start().starts_with("Session:")),
        "the /status handler ran: {out:?}"
    );
    assert!(
        h.runs.try_recv().is_err(),
        "a slash payload never reaches the agent"
    );
    // A command completes synchronously, so the loop is already re-armed.
    assert_eq!(state_of(&h), LoopLifecycle::Scheduled);
}

#[test]
fn the_scheduler_waits_while_a_turn_is_running() {
    let mut h = harness(LoopsConfig {
        min_interval_s: 1,
        ..LoopsConfig::default()
    });
    // The user's own turn is in flight.
    h.state
        .intent_running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    submit(&mut h.state, "/loop 1m check again");
    h.state.tick_loop_scheduler();
    assert!(h.runs.try_recv().is_err());
    assert_eq!(state_of(&h), LoopLifecycle::WaitingForIdle);

    // A message the run loop has not picked up yet also counts as busy.
    h.state
        .intent_running
        .store(false, std::sync::atomic::Ordering::SeqCst);
    submit(&mut h.state, "a message the user just typed");
    assert!(!h.state.loop_session_idle());
    accept_run(&mut h).expect("the user's message runs first");
    complete_run(&mut h, Ok(()));
    assert!(h.state.loop_session_idle());
}

#[test]
fn loop_controls_report_through_the_live_dispatch_path() {
    let mut h = harness(LoopsConfig::default());
    submit(&mut h.state, "/loop status");
    assert!(
        committed(&mut h.state)
            .iter()
            .any(|l| l.contains("No active loop."))
    );
    submit(&mut h.state, "/loop 10m keep an eye on CI");
    submit(&mut h.state, "/loop pause");
    assert_eq!(state_of(&h), LoopLifecycle::Paused);
    submit(&mut h.state, "/loop resume");
    assert_eq!(state_of(&h), LoopLifecycle::Scheduled);
    submit(&mut h.state, "/loop stop");
    assert_eq!(state_of(&h), LoopLifecycle::Stopped);
    let out = committed(&mut h.state);
    for expected in [
        "Loop paused.",
        "Loop resumed; one iteration is due now.",
        "Loop stopped.",
    ] {
        assert!(
            out.iter().any(|l| l.contains(expected)),
            "{expected}: {out:?}"
        );
    }
}

#[test]
fn loops_opens_an_overlay_whose_enter_makes_a_job_due() {
    let mut h = harness(LoopsConfig::default());
    submit(&mut h.state, "/loop 10m audit dependencies");
    // Make the job not-due, so "run now" is observable.
    {
        let mut m = h.manager.lock().unwrap();
        let record = m.record().unwrap().clone();
        m.poll_due(true).unwrap();
        m.finish_dispatch(&record.loop_id, Ok(()));
    }
    committed(&mut h.state);

    submit(&mut h.state, "/loops");
    assert!(h.state.menu_driver.is_active(), "/loops opens the overlay");
    let rows = h.state.overlay_snapshot().expect("the overlay is drawn");
    assert!(
        rows[0].text.starts_with("Scheduled Loops (page 1/1; 1)"),
        "{rows:?}"
    );
    assert!(rows.iter().any(|r| r.text.contains("audit dependencies")));

    h.state.handle_event(key(KeyCode::Enter));
    assert!(!h.state.menu_driver.is_active(), "Enter closes the overlay");
    let out = committed(&mut h.state);
    assert!(out.iter().any(|l| l.contains("is due now.")), "{out:?}");
    let record = h.manager.lock().unwrap().record().unwrap().clone();
    assert!(record.pending, "the job is due again");
}

#[test]
fn deleting_from_the_overlay_requires_confirmation() {
    let mut h = harness(LoopsConfig::default());
    submit(&mut h.state, "/loop 10m audit dependencies");
    submit(&mut h.state, "/loops");
    h.state.handle_event(key(KeyCode::Char('d')));
    let rows = h.state.overlay_snapshot().unwrap();
    assert!(
        rows.iter()
            .any(|r| r.text.starts_with("Delete this job permanently?"))
    );
    h.state.handle_event(key(KeyCode::Char('n')));
    assert!(h.manager.lock().unwrap().record().is_some(), "cancelled");

    h.state.handle_event(key(KeyCode::Char('d')));
    h.state.handle_event(key(KeyCode::Char('y')));
    assert!(h.manager.lock().unwrap().record().is_none(), "deleted");
    assert!(h.manager.lock().unwrap().list_jobs().is_empty());
}

#[test]
fn the_first_run_rebinds_the_loop_to_the_real_session_id() {
    let mut h = harness(LoopsConfig {
        min_interval_s: 1,
        ..LoopsConfig::default()
    });
    submit(&mut h.state, "/loop 1m watch the logs");
    h.state.tick_loop_scheduler();
    accept_run(&mut h).unwrap();
    let session = AgentSession::new("t", ".".into(), "default".into(), "m".into());
    let real_id = session.id.to_string();
    h.state.handle_event(TuiEvent::TaskComplete {
        summary: "ok".into(),
        session,
    });
    let m = h.manager.lock().unwrap();
    assert_eq!(
        m.session_id(),
        real_id,
        "the loop follows the session `--resume` takes"
    );
    assert_eq!(m.record().unwrap().session_id, real_id);
}

#[test]
fn the_ephemeral_region_shows_a_live_loop_while_idle() {
    let mut h = harness(LoopsConfig::default());
    assert!(xaft_tui::ephemeral::build_ephemeral(&h.state).is_none());
    submit(&mut h.state, "/loop 10m keep an eye on CI");
    let eph = xaft_tui::ephemeral::build_ephemeral(&h.state).expect("loop status");
    assert!(
        eph.spinner_line.starts_with("↻ Loop scheduled · every 10m"),
        "{eph:?}"
    );
    submit(&mut h.state, "/loop stop");
    assert!(xaft_tui::ephemeral::build_ephemeral(&h.state).is_none());
}
