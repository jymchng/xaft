//! The ephemeral region — the spinner + status lines refreshed in place
//! beneath the transcript — was 0/34 covered, so the one place that decides
//! what a user sees while a run is in flight had no test at all.
//!
//! These tests drive the real `build_ephemeral` over a real `AppState`. They
//! assert the rendered strings rather than just "some output was produced",
//! because the formatting IS the contract: the verb tells the user which
//! phase is running, the token counts are compacted for width, the cost is
//! fixed-point, and the mode badge is what tells a user the run is not in
//! Auto.
//!
//! Times are seeded an hour in the past so `format_elapsed` renders
//! "60m 0s" deterministically; a test that asserted a live `elapsed()` would
//! flake the moment the machine got busy.

use std::time::{Duration, Instant};

use xaft_tui::state::WorkflowPhase;
use xaft_tui::{AppState, EphemeralState, build_ephemeral};

/// Build a state that is definitely showing something (phase is active).
fn active_state() -> AppState {
    let mut s = AppState::new_for_test();
    s.phase = WorkflowPhase::Planning;
    s
}

fn spinner_of(state: &AppState) -> String {
    build_ephemeral(state)
        .expect("an active phase must render the ephemeral region")
        .spinner_line
}

// ── Nothing to show ─────────────────────────────────────────────────────────

#[test]
fn an_idle_state_with_no_active_agents_renders_nothing() {
    let mut s = AppState::new_for_test();
    s.phase = WorkflowPhase::Idle;
    assert!(
        build_ephemeral(&s).is_none(),
        "an idle state with nothing running must not render a spinner"
    );
}

#[test]
fn an_in_flight_agent_alone_keeps_the_ephemeral_region_visible() {
    // Subagents keep working after the top-level phase goes idle, so an
    // active agent is on its own enough to justify showing the region.
    let mut s = AppState::new_for_test();
    s.phase = WorkflowPhase::Idle;
    s.agent_tracker.on_llm_start("coder");

    let rendered = build_ephemeral(&s).expect("an active agent must render the region");
    assert!(
        rendered.spinner_line.contains("Working…"),
        "an agent running with no top-level phase should read as Working, got {:?}",
        rendered.spinner_line
    );
}

#[test]
fn a_finished_agent_does_not_keep_the_region_alive_on_its_own() {
    let mut s = AppState::new_for_test();
    s.phase = WorkflowPhase::Idle;
    s.agent_tracker.on_llm_start("coder");
    s.agent_tracker.on_run_complete("coder");
    assert!(
        build_ephemeral(&s).is_none(),
        "once every agent is done there is nothing left to report"
    );
}

// ── Phase verb ──────────────────────────────────────────────────────────────

#[test]
fn every_active_phase_renders_its_own_verb() {
    // Only the four active phases reach this path with no agents running;
    // Idle/Done/Error are not `is_active()` and fall through to the generic
    // "Working", which is covered by the next test.
    for (phase, verb) in [
        (WorkflowPhase::Planning, "Planning"),
        (WorkflowPhase::Coding, "Coding"),
        (WorkflowPhase::QaReview, "Reviewing"),
        (WorkflowPhase::Fixing, "Fixing"),
    ] {
        let mut s = AppState::new_for_test();
        s.phase = phase.clone();
        let line = spinner_of(&s);
        assert!(
            line.contains(&format!("{verb}…")),
            "phase {phase:?} should render {verb:?}, got {line:?}"
        );
    }
}

#[test]
fn inactive_phases_fall_back_to_the_generic_working_verb() {
    // Idle, Done and Error are all "not active", so on their own they render
    // nothing at all. With a subagent still running they do render, and they
    // must not claim a specific phase they are not in.
    for phase in [
        WorkflowPhase::Idle,
        WorkflowPhase::Done,
        WorkflowPhase::Error,
    ] {
        let mut s = AppState::new_for_test();
        s.phase = phase.clone();
        s.agent_tracker.on_llm_start("coder");
        let line = spinner_of(&s);
        assert!(
            line.contains("Working…"),
            "phase {phase:?} with an agent running should read as Working, got {line:?}"
        );
    }
}

// ── Spinner line ────────────────────────────────────────────────────────────

#[test]
fn the_spinner_line_omits_the_timing_suffix_before_a_clock_has_started() {
    let mut s = active_state();
    s.task_start_time = None;
    s.agent_start_time = None;
    assert_eq!(
        spinner_of(&s),
        "✢ Planning…",
        "with no start time there is nothing to time or count"
    );
}

#[test]
fn a_started_clock_adds_elapsed_time_and_compacted_output_tokens() {
    let mut s = active_state();
    // 3600s ago renders as a stable "60m 0s" for the next hour.
    s.task_start_time = Some(Instant::now() - Duration::from_secs(3600));
    s.total_output_tokens = 8_600; // 8.6k, not "8600"

    assert_eq!(
        spinner_of(&s),
        "✢ Planning… (60m 0s · ↓ 8.6k tokens)",
        "the spinner must show how long the phase has run and how much it has emitted"
    );
}

#[test]
fn the_agent_clock_is_used_when_no_task_clock_is_running() {
    // A resumed run starts agents before it has a task-level start time, so
    // the agent clock is the fallback rather than showing no timing at all.
    let mut s = active_state();
    s.task_start_time = None;
    s.agent_start_time = Some(Instant::now() - Duration::from_secs(3600));

    assert!(
        spinner_of(&s).contains("60m 0s"),
        "the agent start time must stand in for a missing task start time"
    );
}

#[test]
fn a_running_task_takes_precedence_over_the_agent_clock() {
    let mut s = active_state();
    s.task_start_time = Some(Instant::now() - Duration::from_secs(3600));
    s.agent_start_time = Some(Instant::now());

    let line = spinner_of(&s);
    // Exact match rather than a substring: a substring check for the agent
    // clock would also match inside "60m 0s", which is the whole trap here.
    assert_eq!(
        line, "✢ Planning… (60m 0s · ↓ 0 tokens)",
        "the task clock must win when both are set"
    );
}

#[test]
fn the_spinner_icon_advances_every_five_ticks_and_wraps() {
    // Four icons, one every five ticks.
    let seen: Vec<char> = [0u64, 5, 10, 15, 20]
        .iter()
        .map(|tick| {
            let mut s = active_state();
            s.spinner_tick = *tick;
            s.task_start_time = None;
            build_ephemeral(&s)
                .unwrap()
                .spinner_line
                .chars()
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(
        seen,
        vec!['✢', '✣', '✤', '✥', '✢'],
        "the icon must cycle through all four frames and wrap after 20 ticks"
    );
}

// ── Status line ─────────────────────────────────────────────────────────────

#[test]
fn a_state_with_no_token_usage_has_no_status_line() {
    let mut s = active_state();
    s.total_input_tokens = 0;
    s.total_output_tokens = 0;
    assert_eq!(
        build_ephemeral(&s).unwrap().status_line,
        None,
        "a token readout of \"0 in / 0 out\" is noise, not information"
    );
}

#[test]
fn token_counts_render_the_status_line_with_input_output_and_cost() {
    let mut s = active_state();
    s.total_input_tokens = 1_200; // "1.2k"
    s.total_output_tokens = 400;
    s.total_cost_usd = 0.0042;

    assert_eq!(
        build_ephemeral(&s).unwrap().status_line.as_deref(),
        Some("Tokens: 1.2k in / 400 out  ·  $0.0042"),
        "the status line must show both directions and a fixed-point cost"
    );
}

#[test]
fn output_tokens_alone_are_enough_to_render_the_status_line() {
    // Input can legitimately be 0 for a cached-prefix run; the readout must
    // still appear rather than the whole line disappearing.
    let mut s = active_state();
    s.total_input_tokens = 0;
    s.total_output_tokens = 25;
    s.total_cost_usd = 0.0;

    assert_eq!(
        build_ephemeral(&s).unwrap().status_line.as_deref(),
        Some("Tokens: 0 in / 25 out  ·  $0.0000")
    );
}

#[test]
fn a_million_tokens_render_in_millions() {
    let mut s = active_state();
    s.total_input_tokens = 3_400_000;
    s.total_output_tokens = 2_000_000;
    s.total_cost_usd = 12.5;

    assert_eq!(
        build_ephemeral(&s).unwrap().status_line.as_deref(),
        Some("Tokens: 3.4M in / 2.0M out  ·  $12.5000")
    );
}

// ── Mode badge ──────────────────────────────────────────────────────────────

#[test]
fn auto_is_the_default_mode_and_carries_no_badge() {
    // Pinning the default matters: the badge branch keys off the literal
    // name "auto", so a change of default silently changes the status line.
    let mut s = AppState::new_for_test();
    assert_eq!(
        s.mode_manager.active_name(),
        "auto",
        "a fresh state must start in Auto"
    );

    s.phase = WorkflowPhase::Coding;
    s.total_input_tokens = 10;
    s.total_output_tokens = 10;
    let line = s;
    assert_eq!(
        build_ephemeral(&line).unwrap().status_line.as_deref(),
        Some("Tokens: 10 in / 10 out  ·  $0.0000"),
        "Auto is the unfiltered default and must not be badged"
    );
}

#[test]
fn any_other_mode_is_badged_onto_the_status_line() {
    let mut s = active_state();
    s.total_input_tokens = 10;
    s.total_output_tokens = 10;
    s.mode_manager.set("safe").expect("safe is a built-in mode");

    let badge = s.mode_manager.active().ansi_badge();
    let status = build_ephemeral(&s).unwrap().status_line.unwrap();

    assert!(
        status.starts_with(&format!("{badge}  ")),
        "a non-Auto mode must be badged before the token readout, got {status:?}"
    );
    assert!(
        status.contains("Tokens: 10 in / 10 out"),
        "the badge must not displace the token readout, got {status:?}"
    );
}

#[test]
fn switching_between_two_non_auto_modes_changes_the_badge() {
    let mut s = active_state();
    s.total_input_tokens = 10;
    s.total_output_tokens = 10;

    s.mode_manager.set("plan").unwrap();
    let planned = build_ephemeral(&s).unwrap().status_line.unwrap();
    s.mode_manager.set("safe").unwrap();
    let safe = build_ephemeral(&s).unwrap().status_line.unwrap();

    assert_ne!(
        planned, safe,
        "two different modes must not render the same badge"
    );
}

// ── Shape of the returned value ─────────────────────────────────────────────

#[test]
fn a_rendered_region_always_carries_a_spinner_line() {
    // `status_line` is optional but `spinner_line` is not: every path that
    // returns Some must have decided what to show.
    let mut s = active_state();
    s.phase = WorkflowPhase::Idle;
    s.agent_tracker.on_llm_start("qa");

    let rendered: EphemeralState = build_ephemeral(&s).unwrap();
    assert!(
        !rendered.spinner_line.is_empty(),
        "Some(_) must always carry a spinner line"
    );
}
