//! Session-scoped recurring prompts — `/loop` and `/loops` (parity rows 3/4).
//!
//! The xaft port of agenthicc's PRD-193 loop scheduler
//! (`agenthicc/runners/loop_scheduler.py`, its `/loop` and `/loops` commands in
//! `commands/builtins.py`, and the `LoopJobsOverlay`). Configured by `[loops]`
//! (`xaft_config::LoopsConfig`, parity rows 143-152 and 185).
//!
//! ```text
//! /loop 5m Check the deployment and fix a failing health check.
//! /loop status | pause | resume | stop
//! /loops
//! ```
//!
//! # How an iteration runs
//!
//! The scheduler never talks to a provider. When an iteration is due and the
//! session is idle, `AppState::tick_loop_scheduler` takes the claimed record
//! and submits its payload through the very path a typed message takes — a
//! prompt becomes a normal user turn, a `/command` runs through the slash
//! dispatcher — so iterations reuse the conversation, the active mode, the
//! approval gate and the security policy. A loop never starts a concurrent
//! turn: a due time that finds the session busy is coalesced into a single
//! pending iteration.
//!
//! # Persistence
//!
//! With `persist = true` the record lives at `core.data_dir/loops/<session>/`
//! and is rehydrated only when that session is explicitly resumed
//! (`xaft --resume <id>` / `--continue`), matching agenthicc.

pub mod manager;
pub mod model;
pub mod overlay;
pub mod store;

pub use manager::{
    LOOPS_DISABLED, LOOPS_UNAVAILABLE, LoopClock, LoopEvent, LoopManager, system_clock,
    validate_loop_payload,
};
pub use model::{
    LoopLifecycle, LoopPayloadKind, LoopRecord, format_loop_duration, parse_loop_duration,
    redacted_preview,
};
pub use overlay::LoopJobsMenu;
pub use store::{LoopStorageError, LoopStore};

/// agenthicc's `Scheduled Loops` table (Job ID / State / Session / Payload),
/// for a `/loops` with no overlay host.
pub fn loop_table_lines(records: &[LoopRecord], current_session: &str) -> Vec<String> {
    let mut lines = vec![
        "Scheduled Loops".to_string(),
        format!(
            "{:<12} {:<16} {:<12} {}",
            "Job ID", "State", "Session", "Payload"
        ),
    ];
    for record in records {
        let session = if record.session_id == current_session {
            "current".to_string()
        } else {
            record.session_id.chars().take(12).collect()
        };
        lines.push(format!(
            "{:<12} {:<16} {:<12} {}",
            record.short_id(),
            record.state.as_str(),
            session,
            redacted_preview(&record.payload, 100)
        ));
    }
    if records.is_empty() {
        lines.push(format!(
            "{:<12} {:<16} {:<12} {}",
            "—", "—", "—", "No scheduled loop jobs"
        ));
    }
    lines
}
