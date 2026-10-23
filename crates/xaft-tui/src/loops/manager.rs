//! [`LoopManager`] — controller and scheduler for one live session's loop.
//!
//! A port of agenthicc's `LoopManager` (`runners/loop_scheduler.py`). It owns
//! a durable schedule and nothing else: it never sees a provider, a workflow
//! or a tool. When an iteration is due *and the session is idle*,
//! [`LoopManager::poll_due`] hands the claimed record back to the TUI, which
//! submits the payload through the ordinary input path — so a loop iteration
//! gets the same conversation, approvals, mode and security policy as a
//! message the user typed. When that run finishes, the TUI reports back with
//! [`LoopManager::finish_dispatch`].
//!
//! # Tick-driven, not task-driven
//!
//! agenthicc runs an asyncio task that sleeps until the next due time. Here
//! the TUI event loop calls [`LoopManager::poll_due`] on every pass, and the
//! manager answers from a clock it is given. That keeps the scheduler a plain
//! synchronous state machine — deterministic under a fake clock in tests —
//! and means there is no second task that could race the event loop for the
//! session.
//!
//! # `busy_poll_s`
//!
//! While an iteration is due but the session is busy, the record sits in
//! `waiting_for_idle` and idleness is re-checked every `busy_poll_s` seconds
//! (floored at 50 ms). agenthicc computes this as
//! `min(time_until_due, busy_poll_s)`, which collapses to zero once the
//! iteration is overdue — i.e. it re-checks continuously; xaft applies the
//! interval the key's name and docstring describe.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xaft_config::LoopsConfig;

use super::model::{
    LOOP_SCHEMA_VERSION, LoopLifecycle, LoopPayloadKind, LoopRecord, format_loop_duration,
    looks_like_duration, parse_loop_duration, redacted_error, redacted_preview,
};
use super::store::{LoopStore, is_safe_session_id};

/// The scheduler's notion of "now", in epoch seconds.
pub type LoopClock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// The wall clock.
pub fn system_clock() -> LoopClock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    })
}

/// Validates a payload at scheduling time: `(payload, kind) -> error`.
pub type PayloadValidator<'a> = &'a dyn Fn(&str, LoopPayloadKind) -> Option<String>;

/// One scheduler lifecycle event.
///
/// `kind` is agenthicc's event name (`loop_created`, `loop_dispatch_started`,
/// …). `notice` is the transcript line for events the user should see without
/// asking; routine transitions carry none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopEvent {
    /// agenthicc's event name.
    pub kind: &'static str,
    /// A line for the transcript, if any.
    pub notice: Option<String>,
}

/// Shown when the session has no loop manager attached.
pub const LOOPS_UNAVAILABLE: &str = "Loop scheduling is unavailable in this session.";

/// Shown when `[loops] enabled = false` blocks an operation.
pub const LOOPS_DISABLED: &str = "Loop scheduling is disabled by configuration.";

/// Controller and scheduler for one live session's loop.
pub struct LoopManager {
    session_id: String,
    settings: LoopsConfig,
    root: PathBuf,
    store: LoopStore,
    record: Option<LoopRecord>,
    clock: LoopClock,
    /// `true` once [`LoopManager::start`] ran with loops enabled.
    running: bool,
    /// While `waiting_for_idle`, the earliest time idleness is re-checked.
    next_idle_check_at: f64,
    events: Vec<LoopEvent>,
}

impl LoopManager {
    /// A manager for `session_id`, persisting under `root`
    /// (`core.data_dir/loops`). It does nothing until [`LoopManager::start`].
    pub fn new(
        session_id: impl Into<String>,
        settings: LoopsConfig,
        root: impl Into<PathBuf>,
    ) -> Self {
        let session_id = session_id.into();
        let root = root.into();
        Self {
            store: LoopStore::new(root.clone(), session_id.clone()),
            session_id,
            settings,
            root,
            record: None,
            clock: system_clock(),
            running: false,
            next_idle_check_at: 0.0,
            events: Vec::new(),
        }
    }

    /// Replace the clock (tests drive the scheduler with a fake one).
    pub fn with_clock(mut self, clock: LoopClock) -> Self {
        self.clock = clock;
        self
    }

    /// The session that owns this manager's loop.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The `[loops]` settings in force.
    pub fn settings(&self) -> &LoopsConfig {
        &self.settings
    }

    /// The directory every session's loop lives under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The live record, if any.
    pub fn record(&self) -> Option<&LoopRecord> {
        self.record.as_ref()
    }

    /// Whether the scheduler is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Whether a loop can still schedule work (agenthicc's `active`).
    pub fn active(&self) -> bool {
        self.record.as_ref().is_some_and(|r| r.state.is_active())
    }

    /// Take the events emitted since the last drain.
    pub fn drain_events(&mut self) -> Vec<LoopEvent> {
        std::mem::take(&mut self.events)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn emit(&mut self, kind: &'static str, notice: Option<String>) {
        self.events.push(LoopEvent { kind, notice });
    }

    fn save_and_emit(&mut self, kind: &'static str, notice: Option<String>) {
        if self.settings.persist {
            if let Some(record) = self.record.clone() {
                if let Err(e) = self.store.save(&record) {
                    self.emit(
                        "loop_persistence_failed",
                        Some(format!("↻ Loop state could not be saved: {e}")),
                    );
                }
            }
        }
        self.emit(kind, notice);
    }

    fn load_record_if_needed(&mut self) -> Option<LoopRecord> {
        if self.record.is_none() && self.settings.persist {
            self.record = self.store.load().ok().flatten();
        }
        self.record.clone()
    }

    /// Start the scheduler; `rehydrate` restores a persisted loop on an
    /// explicit resume.
    ///
    /// With `[loops] enabled = false` nothing starts and nothing is restored —
    /// creation and resume are blocked, while status, pause and stop still
    /// work on whatever is on disk.
    pub fn start(&mut self, rehydrate: bool) {
        if self.running || !self.settings.enabled {
            return;
        }
        if rehydrate && self.settings.persist {
            match self.store.load() {
                Err(e) => self.emit(
                    "loop_recovery_failed",
                    Some(format!("↻ Saved loop could not be recovered: {e}")),
                ),
                Ok(Some(loaded))
                    if matches!(
                        loaded.state,
                        LoopLifecycle::Scheduled
                            | LoopLifecycle::WaitingForIdle
                            | LoopLifecycle::Running
                    ) =>
                {
                    let now = self.now();
                    let mut record = loaded;
                    record.state = LoopLifecycle::Scheduled;
                    record.pending = true;
                    record.next_due_at = record.next_due_at.min(now);
                    record.updated_at = now;
                    record.revision += 1;
                    let notice = format!(
                        "↻ Loop restored from the saved session (every {}); missed iterations \
                         are coalesced into one, due now.",
                        format_loop_duration(record.interval_s as f64)
                    );
                    self.record = Some(record);
                    self.save_and_emit("loop_rehydrated", Some(notice));
                }
                Ok(_) => {}
            }
        }
        self.running = true;
        if self.settings.persist {
            let _ = self.store.claim_owner();
        }
    }

    /// Stop scheduling without touching durable state.
    pub fn shutdown(&mut self) {
        self.running = false;
        self.store.release_owner();
    }

    /// Apply one `/loop` subcommand and return the user-facing message.
    pub fn handle_command(&mut self, args: &str, validator: PayloadValidator<'_>) -> String {
        let stripped = args.trim();
        let lowered = stripped.to_lowercase();
        match lowered.as_str() {
            "status" => return self.status_text(),
            "stop" => return self.stop(),
            "pause" => return self.pause(),
            _ => {}
        }
        if !self.settings.enabled {
            return LOOPS_DISABLED.to_string();
        }
        if lowered == "resume" {
            return self.resume();
        }

        let mut interval_s = self.settings.default_interval_s;
        let mut payload = stripped.to_string();
        if !payload.is_empty() {
            let (first, remainder) = match payload.split_once(' ') {
                Some((first, rest)) => (first.to_string(), Some(rest.trim().to_string())),
                None => (payload.clone(), None),
            };
            if looks_like_duration(&first) {
                match parse_loop_duration(&first) {
                    Ok(seconds) => interval_s = seconds,
                    Err(e) => return format!("Cannot schedule loop: {e}"),
                }
                payload = remainder.unwrap_or_default();
            } else if first.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return format!(
                    "Cannot schedule loop: {}",
                    super::model::INTERVAL_SHAPE_ERROR
                );
            }
        }
        if payload.is_empty() {
            return "Usage: /loop [<interval>] <prompt or registered slash command>".to_string();
        }
        if interval_s < self.settings.min_interval_s {
            return format!(
                "Cannot schedule loop: interval must be at least {}",
                format_loop_duration(self.settings.min_interval_s as f64)
            );
        }
        if interval_s > self.settings.max_interval_s {
            return format!(
                "Cannot schedule loop: interval must be at most {}",
                format_loop_duration(self.settings.max_interval_s as f64)
            );
        }
        let kind = LoopPayloadKind::of(&payload);
        if kind == LoopPayloadKind::Command && !self.settings.allow_slash_commands {
            return "Scheduled slash commands are disabled by configuration.".to_string();
        }
        if let Some(error) = validator(&payload, kind) {
            return error;
        }
        let size = payload.len();
        if size > self.settings.max_prompt_bytes {
            return format!(
                "Loop payload is too large ({size} bytes; maximum {}).",
                self.settings.max_prompt_bytes
            );
        }

        let now = self.now();
        let record = LoopRecord {
            schema_version: LOOP_SCHEMA_VERSION,
            loop_id: xaft_runtime::session::SessionId::new()
                .to_string()
                .replace('-', ""),
            session_id: self.session_id.clone(),
            conversation_id: self.session_id.clone(),
            payload_kind: kind,
            payload,
            interval_s,
            created_at: now,
            updated_at: now,
            next_due_at: now,
            expires_at: now + self.settings.max_age_s as f64,
            state: LoopLifecycle::Scheduled,
            revision: 0,
            runs: 0,
            coalesced_runs: 0,
            failures: 0,
            consecutive_failures: 0,
            last_dispatch_at: None,
            last_completed_at: None,
            last_error: String::new(),
            pending: true,
        };
        if self.settings.persist {
            if let Err(e) = self.store.save(&record) {
                return format!("Cannot persist loop: {e}");
            }
        }
        let replaced = self.record.as_ref().is_some_and(|old| {
            !matches!(old.state, LoopLifecycle::Stopped | LoopLifecycle::Expired)
        });
        self.record = Some(record);
        self.emit(
            if replaced {
                "loop_replaced"
            } else {
                "loop_created"
            },
            None,
        );
        format!(
            "↻ Loop scheduled every {}; first iteration is due now and expires in {}.",
            format_loop_duration(interval_s as f64),
            format_loop_duration(self.settings.max_age_s as f64)
        )
    }

    /// Bounded status, with the payload redacted to a short preview.
    pub fn status_text(&self) -> String {
        let record = match &self.record {
            Some(r) => Some(r.clone()),
            None if self.settings.persist => match self.store.load() {
                Ok(r) => r,
                Err(e) => return format!("Loop status unavailable: {e}"),
            },
            None => None,
        };
        let Some(record) = record else {
            return "No active loop.".to_string();
        };
        if record.state.is_terminal() {
            let detail = if record.last_error.is_empty() {
                String::new()
            } else {
                format!(" · error={}", record.last_error)
            };
            return format!(
                "Loop {} · runs={} · failures={}{detail}",
                record.state.as_str(),
                record.runs,
                record.consecutive_failures
            );
        }
        let remaining = (record.next_due_at - self.now()).max(0.0);
        format!(
            "Loop {} · every {} · next in {} · runs={} · failures={} · payload={}",
            record.state.as_str(),
            format_loop_duration(record.interval_s as f64),
            format_loop_duration(remaining),
            record.runs,
            record.consecutive_failures,
            redacted_preview(&record.payload, 96)
        )
    }

    /// The status the TUI shows while a loop is live, `None` otherwise.
    ///
    /// agenthicc's footer projection: hidden when there is no loop or the
    /// loop is stopped, expired or failed.
    pub fn footer_status(&self) -> Option<String> {
        let record = self.record.as_ref()?;
        if record.state.is_terminal() {
            return None;
        }
        Some(self.status_text())
    }

    /// `/loop stop`.
    pub fn stop(&mut self) -> String {
        let Some(mut record) = self.load_record_if_needed() else {
            return "No active loop.".to_string();
        };
        if record.state == LoopLifecycle::Stopped {
            return "Loop is already stopped.".to_string();
        }
        record.state = LoopLifecycle::Stopped;
        record.pending = false;
        record.updated_at = self.now();
        record.revision += 1;
        self.record = Some(record);
        self.save_and_emit("loop_stopped", None);
        "Loop stopped.".to_string()
    }

    /// `/loop pause`.
    pub fn pause(&mut self) -> String {
        let Some(mut record) = self.load_record_if_needed() else {
            return "No active loop.".to_string();
        };
        if matches!(
            record.state,
            LoopLifecycle::Stopped | LoopLifecycle::Expired
        ) {
            return "No active loop.".to_string();
        }
        if record.state == LoopLifecycle::Paused {
            return "Loop is already paused.".to_string();
        }
        record.state = LoopLifecycle::Paused;
        record.updated_at = self.now();
        record.revision += 1;
        self.record = Some(record);
        self.save_and_emit("loop_paused", None);
        "Loop paused.".to_string()
    }

    /// `/loop resume`.
    pub fn resume(&mut self) -> String {
        let Some(mut record) = self.load_record_if_needed() else {
            return "No resumable loop.".to_string();
        };
        if record.state.is_terminal() {
            return "No resumable loop.".to_string();
        }
        if record.state != LoopLifecycle::Paused {
            return "Loop is not paused.".to_string();
        }
        let now = self.now();
        record.state = LoopLifecycle::Scheduled;
        record.pending = true;
        record.next_due_at = now;
        record.updated_at = now;
        record.revision += 1;
        self.record = Some(record);
        self.save_and_emit("loop_resumed", None);
        "Loop resumed; one iteration is due now.".to_string()
    }

    /// One scheduler tick: return the claimed record when an iteration should
    /// be dispatched now.
    ///
    /// `idle` is the caller's answer to "may a loop submit a turn right
    /// now?". Missed intervals are coalesced: a loop that comes due while the
    /// session is busy waits in `waiting_for_idle` and runs once.
    pub fn poll_due(&mut self, idle: bool) -> Option<LoopRecord> {
        if !self.running {
            return None;
        }
        // The TUI calls this on every pass of its event loop, so decide from
        // a borrow and touch (or clone) the record only on a transition.
        let (state, pending, next_due_at, expires_at) = {
            let record = self.record.as_ref()?;
            (
                record.state,
                record.pending,
                record.next_due_at,
                record.expires_at,
            )
        };
        if matches!(
            state,
            LoopLifecycle::Paused
                | LoopLifecycle::Stopped
                | LoopLifecycle::Expired
                | LoopLifecycle::Failed
                | LoopLifecycle::Running
        ) {
            return None;
        }
        let now = self.now();
        if now >= expires_at {
            let record = self.record.as_mut()?;
            record.state = LoopLifecycle::Expired;
            record.pending = false;
            record.updated_at = now;
            record.revision += 1;
            self.save_and_emit(
                "loop_expired",
                Some(format!(
                    "↻ Loop expired after {}.",
                    format_loop_duration(self.settings.max_age_s as f64)
                )),
            );
            return None;
        }
        if now < next_due_at {
            return None;
        }
        if state == LoopLifecycle::WaitingForIdle && now < self.next_idle_check_at {
            return None;
        }
        if !idle {
            self.next_idle_check_at = now + self.settings.busy_poll_s.max(0.05);
            if state != LoopLifecycle::WaitingForIdle || !pending {
                let record = self.record.as_mut()?;
                record.state = LoopLifecycle::WaitingForIdle;
                record.pending = true;
                record.coalesced_runs += 1;
                record.updated_at = now;
                record.revision += 1;
                self.save_and_emit("loop_due_while_busy", None);
            }
            return None;
        }
        let record = self.record.as_mut()?;
        record.state = LoopLifecycle::Running;
        record.pending = false;
        record.runs += 1;
        record.last_dispatch_at = Some(now);
        record.updated_at = now;
        record.revision += 1;
        let claimed = record.clone();
        self.save_and_emit("loop_dispatch_started", None);
        Some(claimed)
    }

    /// Report how the iteration dispatched for `loop_id` ended.
    ///
    /// A success re-arms the loop one interval after it *finished* (the
    /// cadence is completion-to-start, so a slow run never stacks up). A
    /// failure retries with exponential backoff bounded by the interval, and
    /// `max_consecutive_failures` in a row stop the loop as `failed`. A pause
    /// issued while the iteration ran wins; a stop or a replacement makes the
    /// report a no-op.
    pub fn finish_dispatch(&mut self, loop_id: &str, outcome: Result<(), String>) {
        let Some(mut record) = self.record.clone() else {
            return;
        };
        if record.loop_id != loop_id || record.state == LoopLifecycle::Stopped {
            return;
        }
        let now = self.now();
        match outcome {
            Ok(()) => {
                let (state, notice) = if record.state == LoopLifecycle::Paused {
                    (LoopLifecycle::Paused, None)
                } else if now >= record.expires_at {
                    (
                        LoopLifecycle::Expired,
                        Some(format!(
                            "↻ Loop expired after {}.",
                            format_loop_duration(self.settings.max_age_s as f64)
                        )),
                    )
                } else {
                    (LoopLifecycle::Scheduled, None)
                };
                record.state = state;
                record.pending = false;
                record.next_due_at = now + record.interval_s as f64;
                record.last_completed_at = Some(now);
                record.last_error.clear();
                record.consecutive_failures = 0;
                record.updated_at = now;
                record.revision += 1;
                self.record = Some(record);
                self.save_and_emit("loop_dispatch_completed", notice);
            }
            Err(message) => {
                let consecutive = record.consecutive_failures.saturating_add(1);
                let terminal = consecutive >= self.settings.max_consecutive_failures;
                let state = if record.state == LoopLifecycle::Paused {
                    LoopLifecycle::Paused
                } else if terminal {
                    LoopLifecycle::Failed
                } else {
                    LoopLifecycle::Scheduled
                };
                // Backoff is bounded by the interval and never a tight storm:
                // 1s, 2s, 4s, … capped at 256s and at the interval itself.
                let exponent = consecutive.saturating_sub(1).min(8);
                let delay = (record.interval_s as f64).min((1u64 << exponent).max(1) as f64);
                let error = redacted_error(&message);
                record.state = state;
                record.pending = state == LoopLifecycle::Scheduled;
                record.next_due_at = now + delay;
                record.failures += 1;
                record.consecutive_failures = consecutive;
                record.last_completed_at = Some(now);
                record.last_error = error.clone();
                record.updated_at = now;
                record.revision += 1;
                self.record = Some(record);
                if terminal {
                    self.save_and_emit(
                        "loop_failed",
                        Some(format!(
                            "↻ Loop stopped after {consecutive} consecutive failures: {error}"
                        )),
                    );
                } else {
                    self.save_and_emit(
                        "loop_retry_scheduled",
                        Some(format!(
                            "↻ Loop iteration failed ({error}); retrying in {}.",
                            format_loop_duration(delay)
                        )),
                    );
                }
            }
        }
    }

    /// Every valid persisted loop job visible to this installation.
    pub fn list_jobs(&self) -> Vec<LoopRecord> {
        LoopStore::list_all(&self.root)
    }

    /// Make `target` due immediately without bypassing the scheduler.
    ///
    /// This session's own job is re-armed and runs on the next idle tick. A job
    /// owned by another *live* session is refused, so two schedulers never race
    /// for one loop; an unowned job is marked due and runs when its session is
    /// next resumed.
    pub fn run_job_now(&mut self, target: &LoopRecord) -> String {
        if !self.settings.enabled {
            return LOOPS_DISABLED.to_string();
        }
        let short = target.short_id();
        if target.session_id != self.session_id {
            let store = LoopStore::new(self.root.clone(), target.session_id.clone());
            if store.owned_by_another_live_process() {
                return format!(
                    "Loop {short} belongs to a live session; resume that session to run it."
                );
            }
            let current = match store.load() {
                Ok(Some(current)) if current.loop_id == target.loop_id => current,
                Ok(_) => return "The selected loop no longer exists.".to_string(),
                Err(e) => return format!("Cannot run loop {short}: {e}"),
            };
            let now = self.now();
            let mut updated = current;
            updated.state = LoopLifecycle::Scheduled;
            updated.pending = true;
            updated.next_due_at = now;
            updated.updated_at = now;
            updated.revision += 1;
            if let Err(e) = store.save(&updated) {
                return format!("Cannot run loop {short}: {e}");
            }
            let session: String = target.session_id.chars().take(12).collect();
            return format!("Loop {short} is due now; attach session {session} to execute it.");
        }
        let current = match self.record.clone() {
            Some(r) if r.loop_id == target.loop_id => Some(r),
            _ => match self.store.load() {
                Ok(r) => r,
                Err(e) => return format!("Cannot run loop {short}: {e}"),
            },
        };
        let Some(mut current) = current.filter(|r| r.loop_id == target.loop_id) else {
            return "The selected loop no longer exists.".to_string();
        };
        let now = self.now();
        current.state = LoopLifecycle::Scheduled;
        current.pending = true;
        current.next_due_at = now;
        current.updated_at = now;
        current.revision += 1;
        self.record = Some(current);
        self.save_and_emit("loop_run_requested", None);
        format!("Loop {short} is due now.")
    }

    /// Delete one job, refusing another live session's.
    pub fn delete_job(&mut self, target: &LoopRecord) -> String {
        let short = target.short_id();
        let own = target.session_id == self.session_id;
        let store = if own {
            self.store.clone()
        } else {
            let store = LoopStore::new(self.root.clone(), target.session_id.clone());
            if store.owned_by_another_live_process() {
                return format!(
                    "Loop {short} belongs to a live session; stop it from that session before \
                     deleting it."
                );
            }
            store
        };
        match store.delete(Some(&target.loop_id)) {
            Err(e) => return format!("Cannot delete loop {short}: {e}"),
            Ok(None) => return "The selected loop no longer exists.".to_string(),
            Ok(Some(_)) => {}
        }
        if own && self.record.is_some() {
            self.record = None;
            self.emit("loop_deleted", None);
        }
        format!("Loop {short} deleted.")
    }

    /// Move this session's loop to `session_id`.
    ///
    /// A fresh TUI owns its loop under a provisional id until its first run
    /// mints the real session id inside the runtime. Rebinding then moves the
    /// record, so `xaft --resume <id>` — the id the exit hint prints —
    /// rehydrates the loop as agenthicc's explicit resume does.
    pub fn rebind_session(&mut self, session_id: &str) {
        if session_id == self.session_id || !is_safe_session_id(session_id) {
            return;
        }
        let old_store = self.store.clone();
        self.session_id = session_id.to_string();
        self.store = LoopStore::new(self.root.clone(), self.session_id.clone());
        if let Some(mut record) = self.record.take() {
            record.session_id = self.session_id.clone();
            record.conversation_id = self.session_id.clone();
            record.updated_at = self.now();
            record.revision += 1;
            let loop_id = record.loop_id.clone();
            self.record = Some(record);
            if self.settings.persist {
                self.save_and_emit("loop_rebound", None);
                let _ = old_store.delete(Some(&loop_id));
            }
        }
        if self.running && self.settings.persist {
            old_store.release_owner();
            let _ = self.store.claim_owner();
        }
    }
}

impl Drop for LoopManager {
    fn drop(&mut self) {
        // The owner marker means "a live process holds this session"; leaving
        // it behind would only ever make a dead owner look alive.
        self.store.release_owner();
    }
}

/// agenthicc's `_validate_loop_payload`, over xaft's `COMMAND_TABLE`.
///
/// Prompts always pass. A command must name a registered slash command, may
/// not schedule another loop, and may not be one of the commands that only
/// make sense typed by hand: `/workflow` and `/compact` (refused by agenthicc
/// too) and `/quit` (xaft-only — a scheduled quit would end the session the
/// loop belongs to).
pub fn validate_loop_payload(payload: &str, kind: LoopPayloadKind) -> Option<String> {
    if kind != LoopPayloadKind::Command {
        return None;
    }
    let name = payload.split_whitespace().next().unwrap_or(payload);
    let trigger = name.trim_start_matches('/').to_ascii_lowercase();
    let Some(meta) = crate::slash::parser::SlashCommandParser::get_meta(&trigger) else {
        return Some(format!(
            "Unknown scheduled command '{name}'. Use /help to inspect commands."
        ));
    };
    match meta.canonical {
        "loop" => Some("A loop payload cannot schedule another loop.".to_string()),
        "workflow" | "compact" | "quit" => Some(format!(
            "Scheduled command /{} is not supported by /loop.",
            meta.canonical
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A settable clock, in epoch seconds.
    #[derive(Clone)]
    struct FakeClock(Arc<AtomicU64>);

    impl FakeClock {
        fn new(start: f64) -> Self {
            Self(Arc::new(AtomicU64::new(start.to_bits())))
        }
        fn advance(&self, seconds: f64) {
            let now = f64::from_bits(self.0.load(Ordering::SeqCst));
            self.0.store((now + seconds).to_bits(), Ordering::SeqCst);
        }
        fn clock(&self) -> LoopClock {
            let cell = Arc::clone(&self.0);
            Arc::new(move || f64::from_bits(cell.load(Ordering::SeqCst)))
        }
    }

    fn settings() -> LoopsConfig {
        LoopsConfig {
            default_interval_s: 60,
            min_interval_s: 1,
            max_interval_s: 86_400,
            max_age_s: 3_600,
            busy_poll_s: 1.0,
            ..LoopsConfig::default()
        }
    }

    fn no_validation(_: &str, _: LoopPayloadKind) -> Option<String> {
        None
    }

    fn manager(dir: &Path, clock: &FakeClock, settings: LoopsConfig) -> LoopManager {
        let mut m = LoopManager::new("session-1", settings, dir).with_clock(clock.clock());
        m.start(false);
        m
    }

    fn kinds(m: &mut LoopManager) -> Vec<&'static str> {
        m.drain_events().into_iter().map(|e| e.kind).collect()
    }

    #[test]
    fn the_first_iteration_is_due_now_and_the_cadence_follows_completion() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        let reply = m.handle_command("1m inspect the fixture", &no_validation);
        assert!(reply.contains("scheduled"), "{reply}");

        let claimed = m.poll_due(true).expect("due immediately");
        assert_eq!(claimed.payload, "inspect the fixture");
        assert_eq!(claimed.state, LoopLifecycle::Running);
        assert_eq!(claimed.runs, 1);
        assert!(
            m.poll_due(true).is_none(),
            "a running iteration never overlaps"
        );

        clock.advance(5.0);
        m.finish_dispatch(&claimed.loop_id, Ok(()));
        let record = m.record().unwrap();
        assert_eq!(record.state, LoopLifecycle::Scheduled);
        // One interval after the run *finished*, not after it started.
        assert_eq!(record.next_due_at, 1_065.0);
        assert!(kinds(&mut m).contains(&"loop_dispatch_completed"));

        clock.advance(59.0);
        assert!(m.poll_due(true).is_none(), "not due yet");
        clock.advance(1.0);
        assert_eq!(m.poll_due(true).map(|r| r.runs), Some(2));
    }

    #[test]
    fn a_due_iteration_waits_for_idle_and_is_coalesced() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("1m inspect", &no_validation);

        assert!(m.poll_due(false).is_none());
        let record = m.record().unwrap();
        assert_eq!(record.state, LoopLifecycle::WaitingForIdle);
        assert!(record.pending);
        assert_eq!(record.coalesced_runs, 1);

        // Still busy across many missed intervals: one pending run, not many.
        clock.advance(600.0);
        assert!(m.poll_due(false).is_none());
        assert_eq!(m.record().unwrap().coalesced_runs, 1);

        clock.advance(1.0);
        let claimed = m.poll_due(true).expect("idle again: exactly one iteration");
        assert_eq!(claimed.runs, 1);
    }

    /// `poll_due` runs on every pass of the TUI's event loop: a poll with
    /// nothing to do must leave the record, its file and the event log alone.
    #[test]
    fn a_poll_with_nothing_to_do_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("1m inspect", &no_validation);
        let claimed = m.poll_due(true).expect("due immediately");
        m.finish_dispatch(&claimed.loop_id, Ok(()));
        m.drain_events();
        let scheduled = m.record().unwrap().revision;

        clock.advance(30.0);
        for _ in 0..1_000 {
            assert!(m.poll_due(true).is_none(), "not due for another 30s");
        }
        assert_eq!(m.record().unwrap().revision, scheduled);

        // Due while busy: one transition to `waiting_for_idle`, then quiet.
        clock.advance(30.0);
        assert!(m.poll_due(false).is_none());
        let waiting = m.record().unwrap().revision;
        assert_eq!(waiting, scheduled + 1);
        for _ in 0..1_000 {
            assert!(m.poll_due(false).is_none());
        }
        clock.advance(5.0);
        assert!(m.poll_due(false).is_none(), "re-checked, still busy");
        assert_eq!(m.record().unwrap().revision, waiting);
        assert_eq!(kinds(&mut m), vec!["loop_due_while_busy"]);
    }

    /// Row 150 `busy_poll_s`: while waiting for idle, idleness is re-checked
    /// only every `busy_poll_s` seconds.
    #[test]
    fn busy_poll_s_spaces_the_idle_rechecks() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(
            dir.path(),
            &clock,
            LoopsConfig {
                busy_poll_s: 30.0,
                ..settings()
            },
        );
        m.handle_command("1m inspect", &no_validation);
        assert!(m.poll_due(false).is_none(), "busy at the due time");

        clock.advance(10.0);
        assert!(
            m.poll_due(true).is_none(),
            "idle now, but the next re-check is 30s after the last one"
        );
        clock.advance(20.0);
        assert!(m.poll_due(true).is_some(), "re-checked at busy_poll_s");
    }

    #[test]
    fn failures_back_off_and_the_limit_stops_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(
            dir.path(),
            &clock,
            LoopsConfig {
                max_consecutive_failures: 2,
                ..settings()
            },
        );
        m.handle_command("1m inspect", &no_validation);

        let first = m.poll_due(true).unwrap();
        m.finish_dispatch(
            &first.loop_id,
            Err("RuntimeError: provider unavailable".into()),
        );
        let record = m.record().unwrap().clone();
        assert_eq!(record.state, LoopLifecycle::Scheduled);
        assert_eq!(record.last_error, "RuntimeError: provider unavailable");
        assert_eq!(record.next_due_at, 1_001.0, "first backoff is one second");
        let notices: Vec<_> = m
            .drain_events()
            .into_iter()
            .filter_map(|e| e.notice)
            .collect();
        assert!(
            notices.iter().any(|n| n.contains("retrying in 1s")),
            "{notices:?}"
        );

        clock.advance(2.0);
        let second = m.poll_due(true).unwrap();
        m.finish_dispatch(
            &second.loop_id,
            Err("RuntimeError: provider unavailable".into()),
        );
        assert_eq!(m.record().unwrap().state, LoopLifecycle::Failed);
        assert!(
            m.status_text().starts_with("Loop failed"),
            "{}",
            m.status_text()
        );
        assert!(m.poll_due(true).is_none(), "a failed loop never runs again");
    }

    #[test]
    fn a_pause_during_a_running_iteration_wins_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("1m inspect", &no_validation);
        let claimed = m.poll_due(true).unwrap();
        assert_eq!(m.handle_command("pause", &no_validation), "Loop paused.");
        m.finish_dispatch(&claimed.loop_id, Ok(()));
        assert_eq!(m.record().unwrap().state, LoopLifecycle::Paused);
    }

    #[test]
    fn a_stop_during_a_running_iteration_makes_completion_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("1m inspect", &no_validation);
        let claimed = m.poll_due(true).unwrap();
        assert_eq!(m.stop(), "Loop stopped.");
        m.finish_dispatch(&claimed.loop_id, Ok(()));
        assert_eq!(m.record().unwrap().state, LoopLifecycle::Stopped);
    }

    #[test]
    fn command_validation_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        let only_status = |payload: &str, kind: LoopPayloadKind| {
            (kind == LoopPayloadKind::Command && payload != "/status")
                .then(|| "unknown command".to_string())
        };
        assert!(
            m.handle_command("/missing", &only_status)
                .contains("unknown")
        );
        assert!(
            m.handle_command("5x inspect", &only_status)
                .contains("interval must look")
        );
        assert!(
            m.handle_command("0s inspect", &only_status)
                .contains("outside")
        );
        assert_eq!(
            m.handle_command("", &only_status),
            "Usage: /loop [<interval>] <prompt or registered slash command>"
        );
        assert_eq!(
            m.handle_command("5m", &only_status),
            "Usage: /loop [<interval>] <prompt or registered slash command>"
        );
        assert!(
            m.handle_command("1m first", &only_status)
                .contains("scheduled")
        );
        let first = m.record().unwrap().loop_id.clone();
        assert_eq!(kinds(&mut m), vec!["loop_created"]);
        assert!(
            m.handle_command("1m second", &only_status)
                .contains("scheduled")
        );
        assert_ne!(m.record().unwrap().loop_id, first);
        assert_eq!(kinds(&mut m), vec!["loop_replaced"]);
        assert_eq!(m.handle_command("pause", &only_status), "Loop paused.");
        assert_eq!(
            m.handle_command("pause", &only_status),
            "Loop is already paused."
        );
        assert_eq!(
            m.handle_command("resume", &only_status),
            "Loop resumed; one iteration is due now."
        );
        assert_eq!(
            m.handle_command("resume", &only_status),
            "Loop is not paused."
        );
        assert_eq!(m.handle_command("stop", &only_status), "Loop stopped.");
        assert_eq!(
            m.handle_command("stop", &only_status),
            "Loop is already stopped."
        );
        assert_eq!(
            m.handle_command("resume", &only_status),
            "No resumable loop."
        );
    }

    /// Rows 144-146: the default and the bounds on the interval.
    #[test]
    fn interval_bounds_and_the_default_interval_come_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(
            dir.path(),
            &clock,
            LoopsConfig {
                default_interval_s: 120,
                min_interval_s: 60,
                max_interval_s: 3_600,
                ..settings()
            },
        );
        assert_eq!(
            m.handle_command("30s too fast", &no_validation),
            "Cannot schedule loop: interval must be at least 1m"
        );
        assert_eq!(
            m.handle_command("2h too slow", &no_validation),
            "Cannot schedule loop: interval must be at most 1h"
        );
        assert!(
            m.handle_command("no interval given", &no_validation)
                .contains("every 2m")
        );
        assert_eq!(m.record().unwrap().interval_s, 120);
    }

    /// Rows 147-148: expiry and the payload budget.
    #[test]
    fn max_age_expires_the_loop_and_max_prompt_bytes_bounds_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(
            dir.path(),
            &clock,
            LoopsConfig {
                max_age_s: 120,
                max_prompt_bytes: 8,
                ..settings()
            },
        );
        assert_eq!(
            m.handle_command("1m way too long", &no_validation),
            "Loop payload is too large (12 bytes; maximum 8)."
        );
        assert!(
            m.handle_command("1m ok", &no_validation)
                .contains("expires in 2m")
        );
        clock.advance(121.0);
        assert!(m.poll_due(true).is_none());
        assert_eq!(m.record().unwrap().state, LoopLifecycle::Expired);
        let notices: Vec<_> = m
            .drain_events()
            .into_iter()
            .filter_map(|e| e.notice)
            .collect();
        assert!(notices.iter().any(|n| n.contains("expired")), "{notices:?}");
    }

    /// Rows 143 and 152: `enabled` and `allow_slash_commands`.
    #[test]
    fn disabled_loops_refuse_creation_but_still_stop_persisted_work() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        assert!(
            m.handle_command("1m inspect", &no_validation)
                .contains("scheduled")
        );
        drop(m);

        let mut disabled = LoopManager::new(
            "session-1",
            LoopsConfig {
                enabled: false,
                ..settings()
            },
            dir.path(),
        )
        .with_clock(clock.clock());
        disabled.start(true);
        assert!(!disabled.is_running(), "a disabled scheduler never starts");
        assert_eq!(
            disabled.handle_command("1m new prompt", &no_validation),
            LOOPS_DISABLED
        );
        assert_eq!(
            disabled.handle_command("resume", &no_validation),
            LOOPS_DISABLED
        );
        assert!(
            disabled
                .handle_command("status", &no_validation)
                .starts_with("Loop scheduled")
        );
        assert_eq!(
            disabled.handle_command("stop", &no_validation),
            "Loop stopped."
        );

        let mut no_commands = manager(
            dir.path(),
            &clock,
            LoopsConfig {
                allow_slash_commands: false,
                ..settings()
            },
        );
        assert_eq!(
            no_commands.handle_command("1m /status", &no_validation),
            "Scheduled slash commands are disabled by configuration."
        );
        assert!(
            no_commands
                .handle_command("1m a prompt", &no_validation)
                .contains("scheduled")
        );
    }

    /// Row 151 `persist`: with it on, an explicit resume rehydrates the loop;
    /// with it off, nothing is written at all.
    #[test]
    fn persist_controls_whether_a_resume_rehydrates_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("10m inspect", &no_validation);
        let claimed = m.poll_due(true).unwrap();
        m.finish_dispatch(&claimed.loop_id, Ok(()));
        m.shutdown();
        drop(m);

        // Resumed an hour of wall time later: one coalesced iteration, due now.
        clock.advance(1_800.0);
        let mut resumed =
            LoopManager::new("session-1", settings(), dir.path()).with_clock(clock.clock());
        resumed.start(true);
        let record = resumed.record().expect("rehydrated").clone();
        assert_eq!(record.loop_id, claimed.loop_id);
        assert_eq!(record.state, LoopLifecycle::Scheduled);
        assert!(record.pending);
        assert!(
            resumed
                .drain_events()
                .iter()
                .any(|e| e.kind == "loop_rehydrated")
        );
        assert!(resumed.poll_due(true).is_some());

        // A fresh (non-resumed) start does not pick it up.
        let mut fresh = LoopManager::new("session-1", settings(), dir.path());
        fresh.start(false);
        assert!(fresh.record().is_none());

        let no_persist_dir = tempfile::tempdir().unwrap();
        let mut ephemeral = manager(
            no_persist_dir.path(),
            &clock,
            LoopsConfig {
                persist: false,
                ..settings()
            },
        );
        ephemeral.handle_command("1m inspect", &no_validation);
        assert!(
            LoopStore::list_all(no_persist_dir.path()).is_empty(),
            "persist = false writes nothing"
        );
    }

    #[test]
    fn jobs_can_be_listed_run_now_and_deleted_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut other =
            LoopManager::new("session-2", settings(), dir.path()).with_clock(clock.clock());
        other.start(false);
        other.handle_command("2m /status", &no_validation);
        // `other` is a live session in this very process; simulate it having
        // exited by releasing its owner marker.
        other.shutdown();
        let foreign = other.record().unwrap().clone();
        drop(other);

        let mut m = manager(dir.path(), &clock, settings());
        m.handle_command("1m inspect one", &no_validation);
        let own = m.record().unwrap().clone();
        let ids: Vec<String> = m.list_jobs().into_iter().map(|r| r.loop_id).collect();
        assert_eq!(ids.len(), 2);

        let reply = m.run_job_now(&foreign);
        assert!(
            reply.contains("is due now; attach session session-2"),
            "{reply}"
        );
        assert!(m.run_job_now(&own).ends_with("is due now."));
        assert!(m.delete_job(&foreign).ends_with("deleted."));
        assert!(
            LoopStore::new(dir.path(), "session-2")
                .load()
                .unwrap()
                .is_none()
        );
        assert_eq!(
            m.delete_job(&foreign),
            "The selected loop no longer exists."
        );
    }

    #[cfg(unix)]
    #[test]
    fn another_live_sessions_job_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let store = LoopStore::new(dir.path(), "session-live");
        let mut owner =
            LoopManager::new("session-live", settings(), dir.path()).with_clock(clock.clock());
        owner.start(false);
        owner.handle_command("2m inspect", &no_validation);
        let foreign = owner.record().unwrap().clone();
        // Mark the session as held by a live process that is not this one.
        std::fs::write(store.session_dir().join("owner.json"), r#"{"pid": 1}"#).unwrap();

        let mut m = manager(dir.path(), &clock, settings());
        assert!(
            m.run_job_now(&foreign)
                .contains("belongs to a live session")
        );
        assert!(m.delete_job(&foreign).contains("belongs to a live session"));
        assert!(store.load().unwrap().is_some(), "nothing was deleted");
    }

    #[test]
    fn rebinding_moves_the_loop_to_the_real_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m =
            LoopManager::new("provisional", settings(), dir.path()).with_clock(clock.clock());
        m.start(false);
        m.handle_command("1m inspect", &no_validation);
        m.rebind_session("real-session");
        assert_eq!(m.session_id(), "real-session");
        assert!(
            LoopStore::new(dir.path(), "provisional")
                .load()
                .unwrap()
                .is_none()
        );
        let moved = LoopStore::new(dir.path(), "real-session")
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(moved.session_id, "real-session");
        assert_eq!(moved.conversation_id, "real-session");
        m.rebind_session("../escape");
        assert_eq!(m.session_id(), "real-session", "an unsafe id is ignored");
    }

    #[test]
    fn status_reports_a_redacted_bounded_preview() {
        let dir = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(1_000.0);
        let mut m = manager(dir.path(), &clock, settings());
        assert_eq!(m.status_text(), "No active loop.");
        m.handle_command("2m deploy using token-supersecret", &no_validation);
        clock.advance(0.0);
        let status = m.status_text();
        assert!(
            status.contains("payload=deploy using token [redacted]"),
            "{status}"
        );
        assert!(!status.contains("supersecret"), "{status}");
        assert_eq!(m.footer_status(), Some(status));
        m.stop();
        assert_eq!(m.footer_status(), None, "a stopped loop leaves the footer");
    }

    #[test]
    fn scheduled_payload_validation_matches_agenthicc() {
        assert_eq!(
            validate_loop_payload("check it", LoopPayloadKind::Prompt),
            None
        );
        assert_eq!(
            validate_loop_payload("/status", LoopPayloadKind::Command),
            None
        );
        assert_eq!(
            validate_loop_payload("/ps --json", LoopPayloadKind::Command),
            None
        );
        assert_eq!(
            validate_loop_payload("/loop 1m x", LoopPayloadKind::Command).as_deref(),
            Some("A loop payload cannot schedule another loop.")
        );
        assert_eq!(
            validate_loop_payload("/nope", LoopPayloadKind::Command).as_deref(),
            Some("Unknown scheduled command '/nope'. Use /help to inspect commands.")
        );
        for refused in ["/workflow review", "/compact", "/quit", "/exit"] {
            let msg = validate_loop_payload(refused, LoopPayloadKind::Command)
                .unwrap_or_else(|| panic!("{refused} must be refused"));
            assert!(msg.contains("is not supported by /loop"), "{msg}");
        }
    }
}
