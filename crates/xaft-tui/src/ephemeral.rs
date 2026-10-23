//! Ephemeral region: spinner + status lines that update in-place below the transcript.

use crate::state::{AppState, WorkflowPhase};
use crate::state::{format_elapsed, format_tokens_compact};

/// Content of the ephemeral region rendered beneath the transcript.
#[derive(Debug, Clone)]
pub struct EphemeralState {
    /// E.g. "✣ Planning… (5m 34s · ↓ 11k tokens)"
    pub spinner_line: String,
    /// E.g. "Tokens: 1.2k in / 400 out  ·  $0.0042"
    pub status_line: Option<String>,
}

/// Build the current ephemeral state from `AppState`.
///
/// Returns `None` when there is nothing to show (idle, no active agent).
///
/// The `transcript_loading` branch is checked FIRST and outranks everything,
/// mirroring agenthicc's status bar, which tests `transcript_loading` before its
/// waiting/running/idle branches so a resumed transcript is visibly in progress
/// even though no agent is running yet.
pub fn build_ephemeral(state: &AppState) -> Option<EphemeralState> {
    if state.transcript_loading {
        return Some(crate::replay::loading_state());
    }

    if !state.phase.is_active() && state.agent_tracker.active_count() == 0 {
        return startup_status(state).or_else(|| loop_status(state));
    }

    let icon = spinner_icon(state.spinner_tick);
    let verb = phase_verb(&state.phase);
    let start = state.task_start_time.or(state.agent_start_time);

    let spinner_line = if let Some(start) = start {
        let elapsed_str = format_elapsed(start.elapsed());
        let out_tok = format_tokens_compact(state.total_output_tokens);
        format!("{icon} {verb}… ({elapsed_str} · ↓ {out_tok} tokens)")
    } else {
        format!("{icon} {verb}…")
    };

    let status_line = if state.total_input_tokens > 0 || state.total_output_tokens > 0 {
        let in_tok = format_tokens_compact(state.total_input_tokens);
        let out_tok = format_tokens_compact(state.total_output_tokens);
        // Prepend mode badge when not in Auto mode.
        let mode = state.mode_manager.active();
        let mode_prefix = if mode.name != "auto" {
            format!("{}  ", mode.ansi_badge())
        } else {
            String::new()
        };
        Some(format!(
            "{mode_prefix}Tokens: {in_tok} in / {out_tok} out  ·  ${:.4}",
            state.total_cost_usd
        ))
    } else {
        None
    };

    Some(EphemeralState {
        spinner_line,
        status_line,
    })
}

/// A compact startup indicator while deferred phases are still loading.
///
/// agenthicc's status bar shows a `Startup` indicator while readiness work is
/// pending; `/startup` has the full report.
fn startup_status(state: &AppState) -> Option<EphemeralState> {
    let loading = state.services.startup.as_ref()?.loading();
    if loading.is_empty() {
        return None;
    }
    Some(EphemeralState {
        spinner_line: format!("◌ Startup: loading {}…", loading.join(", ")),
        status_line: None,
    })
}

/// The live `/loop` status while the session is idle.
///
/// agenthicc projects `LoopManager.status_text()` into its footer whenever a
/// loop is live and hides it once the loop is stopped, expired or failed. The
/// ephemeral region is xaft's live status surface (refreshed ~10 fps), so the
/// "next in …" countdown stays current.
fn loop_status(state: &AppState) -> Option<EphemeralState> {
    let manager = state.services.loop_manager.as_ref()?;
    let status = manager
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .footer_status()?;
    Some(EphemeralState {
        spinner_line: format!("↻ {status}"),
        status_line: None,
    })
}

fn phase_verb(phase: &WorkflowPhase) -> &'static str {
    match phase {
        WorkflowPhase::Planning => "Planning",
        WorkflowPhase::Coding => "Coding",
        WorkflowPhase::QaReview => "Reviewing",
        WorkflowPhase::Fixing => "Fixing",
        _ => "Working",
    }
}

fn spinner_icon(tick: u64) -> char {
    const ICONS: &[char] = &['✢', '✣', '✤', '✥'];
    ICONS[(tick as usize / 5) % ICONS.len()]
}
