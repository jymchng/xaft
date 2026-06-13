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
pub fn build_ephemeral(state: &AppState) -> Option<EphemeralState> {
    if !state.phase.is_active() && state.agent_tracker.active_count() == 0 {
        return None;
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
