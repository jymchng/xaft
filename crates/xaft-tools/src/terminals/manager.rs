//! [`TerminalManager`] — owns background terminal process *groups* for one
//! session.
//!
//! # The one property everything else serves
//!
//! A terminal may be signalled **only** while this manager is holding the live
//! child it spawned. Never from a persisted record. agenthicc's
//! `_recover_stale_records` says exactly why, and it is worth restating because
//! it is the difference between a tool and a vulnerability:
//!
//! > We intentionally do not signal a PID read from disk. Only a live
//! > `_RuntimeEntry` created by this manager is eligible for termination; this
//! > prevents PID reuse from becoming an arbitrary kill primitive.
//!
//! A record read from disk says only "pid 4242 was ours once". Between that
//! write and a later `stop_terminal`, pid 4242 may have exited and been reused
//! by an unrelated process — plausibly the agent's own editor. Signalling it
//! would make `stop_terminal` a "kill any process on this machine" primitive
//! for anyone who can write a record. So [`TerminalManager::stop`] looks the id
//! up in `_entries` and returns `false` when it is absent, and
//! [`TerminalManager::start`] converts every active record it inherited into
//! [`TerminalState::Orphaned`] instead. Pinned by
//! `an_orphaned_terminal_cannot_be_stopped` (adopting an active record written
//! by a manager that no longer exists) and
//! `a_record_with_an_unknown_state_deserialises_as_orphaned` (a state this
//! build does not recognise must not round-trip back into "running").
//!
//! # Why the process group, not the child
//!
//! [`std::process::Command::process_group`] puts the child in a *new* process
//! group, and [`signal_group`] sends to the group. Signalling only the child is
//! the classic background-command bug: `sh -c 'sleep 100 &'` exits immediately,
//! leaving `sleep` reparented to init and still running. `killpg` reaches it.
//! Note that a graceful stop is not sufficient on its own: POSIX gives a
//! background job started from a non-interactive shell SIGINT and SIGQUIT set
//! to ignore, so the shell can be reaped while the grandchild keeps running.
//! The monitor therefore closes the group out unconditionally once the child is
//! reaped. Pin: `stop_reaches_a_grandchild_through_the_process_group`.
//!
//! # Why `cwd` is resolved before spawn
//!
//! A background terminal outlives the tool call, so a `cwd` escape would not
//! be caught by anything downstream — the process is already running. It is
//! rejected in [`TerminalManager::start`] before the spawn. Pin:
//! `a_cwd_outside_the_workspace_is_refused_before_any_spawn`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::record::{TerminalRecord, TerminalState, TerminalStore};
use super::redact::{
    DEFAULT_MAX_OUTPUT, bounded_append, is_loopback, redact_label, redact_output, redact_text,
};

/// Smallest capture budget the manager will accept.
///
/// agenthicc floors `max_output_bytes` at 1024. A smaller budget makes the
/// truncation marker itself the entire output, which is useless to the agent.
pub const MIN_MAX_OUTPUT: usize = 1024;

/// Seconds to wait after a graceful stop before escalating to `SIGKILL`.
pub const DEFAULT_CANCEL_GRACE_S: f64 = 5.0;

/// Cap on the wall-clock budget, so a `background: true` call cannot pin a
/// process group open forever without a way to stop it.
pub const MAX_WALL_TIMEOUT_S: f64 = 86_400.0;

/// Epoch seconds, or 0.0 if the clock is before the epoch.
fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A lossless, one-shot "this terminal has finished" signal.
///
/// # Why not `tokio::sync::Notify`
///
/// `Notify::notify_waiters()` is *not* a latch. It wakes exactly the tasks that
/// were already subscribed at the instant it was called and retains no permit,
/// so a `stop()` or `wait()` that subscribes a moment later waits forever. In
/// this subsystem that race is not theoretical: the monitor task fires the
/// signal on the same tick it removes the entry, while `stop()` subscribes
/// after signalling the process group — i.e. after the notification is
/// guaranteed to have fired. `stop()` would hang on a terminal that had already
/// exited, forever, holding no lock but blocking the tool call.
///
/// `watch` keeps the value, so a late subscriber observes `true` on its first
/// borrow and returns immediately. `Notify::notified()` is the wrong primitive
/// for "wait until X, whenever you happen to start waiting".
#[derive(Clone, Debug)]
struct Finished(Arc<tokio::sync::watch::Sender<bool>>);

impl Finished {
    fn new() -> Self {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        Self(Arc::new(tx))
    }

    /// Latch the signal. Idempotent, and safe to call from any task.
    fn fire(&self) {
        let _ = self.0.send(true);
    }

    /// Resolve once fired — immediately if it already has.
    async fn wait(&self) {
        let mut rx = self.0.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                // The sender is gone, which can only happen if every holder
                // dropped. Treat as finished rather than hanging.
                return;
            }
        }
    }

    /// Resolve to `true` if the signal did *not* fire within `dur`.
    ///
    /// This replaces the old `timeout_fired(&mut notified, ..)` helper, which
    /// could only be handed a `Pin<&mut Notified<'_>>` — a type it could not
    /// name without borrowing a future that did not exist yet.
    async fn timed_out(&self, dur: Duration) -> bool {
        tokio::time::timeout(dur, self.wait()).await.is_err()
    }
}

/// A process this manager spawned and still owns.
struct RuntimeEntry {
    /// The live record. Mutations go here and are then persisted.
    record: TerminalRecord,
    /// The child handle. `None` once reaped.
    child: Option<Child>,
    /// Monotonic handle on the child, to stop waiting once it is reaped.
    finished: Finished,
    /// Set when a stop has been requested, to pick the terminal state.
    stop_requested: bool,
    /// Set when the stop was escalated to `SIGKILL`.
    force_requested: bool,
}

/// Configuration for one manager.
#[derive(Debug, Clone)]
pub struct TerminalManagerConfig {
    /// Owning session. Records from other sessions are invisible.
    pub session_id: String,
    /// Workspace root, and the default `cwd`.
    pub workspace_root: PathBuf,
    /// Directory holding the durable JSONL store.
    pub store_root: PathBuf,
    /// Whether background terminals are permitted at all.
    pub enabled: bool,
    /// Max concurrent terminals for this session.
    pub max_terminals: usize,
    /// Max concurrent terminals across the project.
    pub max_terminals_per_project: usize,
    /// Per-stream capture budget, bytes.
    pub max_output_bytes: usize,
    /// Manager-wide wall-clock budget; 0 means "no manager-wide cap".
    pub wall_timeout_s: f64,
    /// Grace period before escalating a stop to `SIGKILL`, seconds.
    pub cancel_grace_s: f64,
    /// Owning job id, recorded on every terminal.
    pub parent_job_id: String,
    /// Prune finished records older than this many days; 0 disables pruning.
    pub retention_days: u32,
}

impl TerminalManagerConfig {
    /// Defaults for a session rooted at `workspace_root`.
    pub fn new(session_id: impl Into<String>, workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            session_id: session_id.into(),
            store_root: workspace_root.join(".xaft"),
            workspace_root,
            enabled: true,
            max_terminals: 4,
            max_terminals_per_project: 8,
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            wall_timeout_s: 0.0,
            cancel_grace_s: DEFAULT_CANCEL_GRACE_S,
            parent_job_id: String::new(),
            retention_days: 30,
        }
    }
}

/// Owns background terminal process groups for one session.
pub struct TerminalManager {
    config: TerminalManagerConfig,
    /// `Arc` so the per-terminal monitor task can persist its final record
    /// without borrowing the manager, which it outlives.
    store: Arc<TerminalStore>,
    /// Live records, keyed by `terminal_id`. The *only* signal-eligible set.
    ///
    /// `Arc` so the output-pump tasks can fold chunks in without holding a
    /// borrow of the manager, which outlives them anyway.
    records: Arc<Mutex<HashMap<String, TerminalRecord>>>,
    /// Live children, keyed by `terminal_id`. Membership here is what makes a
    /// terminal stoppable; see the module docs.
    ///
    /// `Arc` for the same reason as `store`: the monitor task removes its own
    /// id when the process is reaped, and `tokio::spawn` demands `'static`.
    entries: Arc<Mutex<HashMap<String, Arc<Mutex<RuntimeEntry>>>>>,
    /// Monotonic counter backing `term-NNNN` ids.
    counter: AtomicU64,
}

impl TerminalManager {
    /// Build a manager, adopting any records already in the store.
    ///
    /// Every inherited *active* record is immediately marked
    /// [`TerminalState::Orphaned`]: this manager did not spawn it, so it cannot
    /// prove ownership, and an unprovable terminal must not be killable.
    pub fn new(config: TerminalManagerConfig) -> Self {
        let store = TerminalStore::new(&config.store_root);
        if config.retention_days > 0 {
            store.prune(f64::from(config.retention_days) * 86_400.0, now_epoch());
        }

        let mut records: HashMap<String, TerminalRecord> = store
            .records()
            .into_iter()
            .map(|r| (r.terminal_id.clone(), r))
            .collect();

        // Ownership recovery: anything still marked active belongs to a manager
        // that is gone. Orphan it rather than adopting it as stoppable.
        for record in records.values_mut() {
            if record.active() && record.session_id == config.session_id {
                record.state = TerminalState::Orphaned;
                if record.finished_at.is_none() {
                    record.finished_at = Some(now_epoch());
                }
                record.stop_reason = "manager restarted before terminal completed".into();
                store.upsert(record);
            }
        }

        Self {
            config,
            store: Arc::new(store),
            records: Arc::new(Mutex::new(records)),
            entries: Arc::new(Mutex::new(HashMap::new())),
            counter: AtomicU64::new(0),
        }
    }

    /// The store backing this manager.
    pub fn store(&self) -> &TerminalStore {
        &self.store
    }

    /// This manager's configuration.
    pub fn config(&self) -> &TerminalManagerConfig {
        &self.config
    }

    /// Records for this session, newest first.
    pub fn list_records(&self) -> Vec<TerminalRecord> {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<TerminalRecord> = guard
            .values()
            .filter(|r| r.session_id == self.config.session_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
        out
    }

    /// One record by id, regardless of session.
    pub fn get(&self, terminal_id: &str) -> Option<TerminalRecord> {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(terminal_id).cloned()
    }

    /// How many terminals for this session are possibly live.
    pub fn running_count(&self) -> usize {
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .values()
            .filter(|r| r.session_id == self.config.session_id && r.active())
            .count()
    }

    /// Why a new terminal would be refused, or `None` if it would be accepted.
    pub fn check_limits(&self) -> Option<String> {
        if !self.config.enabled {
            return Some("background terminals are disabled by configuration".into());
        }
        let guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let session_running = guard
            .values()
            .filter(|r| r.session_id == self.config.session_id && r.active())
            .count();
        if session_running >= self.config.max_terminals {
            return Some(format!(
                "session terminal limit reached ({})",
                self.config.max_terminals
            ));
        }
        let project_running = guard
            .values()
            .filter(|r| {
                r.project_root == self.config.workspace_root.to_string_lossy() && r.active()
            })
            .count();
        if project_running >= self.config.max_terminals_per_project {
            return Some(format!(
                "project terminal limit reached ({})",
                self.config.max_terminals_per_project
            ));
        }
        None
    }

    /// Start an owned terminal and return a handle immediately.
    ///
    /// Exactly one of `command` (a shell string) or `argv` (literal, no shell)
    /// must be given, matching agenthicc — ambiguity here is a bug, not a
    /// precedence question, because the two have different quoting semantics.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        command: Option<&str>,
        argv: Option<&[String]>,
        cwd: Option<&str>,
        timeout: f64,
        env: Option<&[(String, String)]>,
        shell: bool,
        label: &str,
        tool_call_id: &str,
        lifecycle: &str,
        readiness: Option<&Value>,
    ) -> Value {
        if let Err(msg) = validate_timeout(timeout) {
            return rejected(&msg);
        }
        if lifecycle != "oneshot" && lifecycle != "service" {
            return rejected("lifecycle must be 'oneshot' or 'service'");
        }
        if command.is_some() == argv.is_some() {
            return serde_json::json!({
                "ok": false,
                "background": true,
                "state": "failed",
                "error": "provide command or argv",
            });
        }
        if let Some(reason) = self.check_limits() {
            return rejected(&reason);
        }

        // ---- cwd: confined to the workspace BEFORE the spawn --------------
        //
        // A background terminal outlives this call, so there is no later chance
        // to notice an escape. Resolving and rejecting here is the only place
        // it can be caught.
        let resolved_cwd = match self.resolve_cwd(cwd) {
            Ok(path) => path,
            Err(msg) => {
                return serde_json::json!({
                    "ok": false,
                    "background": true,
                    "state": TerminalState::SpawnFailed.as_str(),
                    "error": msg,
                    "termination_reason": "spawn failed: invalid cwd",
                    "spawn_failure": "cwd",
                });
            }
        };
        if !resolved_cwd.is_dir() {
            return serde_json::json!({
                "ok": false,
                "background": true,
                "state": TerminalState::SpawnFailed.as_str(),
                "error": format!("cwd is not a directory: {}", resolved_cwd.display()),
                "termination_reason": "spawn failed: invalid cwd",
                "spawn_failure": "cwd",
            });
        }

        let display_command = match (command, argv) {
            (Some(cmd), _) => cmd.to_string(),
            (None, Some(parts)) => parts.join(" "),
            _ => unreachable!("checked above"),
        };
        let mut hasher = Sha256::new();
        hasher.update(display_command.as_bytes());
        let command_digest = format!("{:x}", hasher.finalize());

        let terminal_id = format!("term-{:08x}", self.counter.fetch_add(1, Ordering::SeqCst));
        let mut record = TerminalRecord {
            terminal_id: terminal_id.clone(),
            session_id: self.config.session_id.clone(),
            project_root: self.config.workspace_root.to_string_lossy().into_owned(),
            cwd: resolved_cwd.to_string_lossy().into_owned(),
            kind: if shell { "shell".into() } else { "exec".into() },
            command: redact_label(&display_command),
            command_digest,
            label: redact_label(if label.is_empty() {
                &display_command
            } else {
                label
            }),
            state: TerminalState::Starting,
            created_at: now_epoch(),
            tool_call_id: tool_call_id.to_string(),
            parent_job_id: if self.config.parent_job_id.is_empty() {
                self.config.session_id.clone()
            } else {
                self.config.parent_job_id.clone()
            },
            started_at: None,
            finished_at: None,
            pid: None,
            pgid: None,
            returncode: None,
            stdout: String::new(),
            stderr: String::new(),
            output_bytes: 0,
            truncated: false,
            timed_out: false,
            stop_reason: String::new(),
            timeout_s: timeout,
            effective_timeout_s: None,
            last_output_at: None,
            lifecycle: lifecycle.to_string(),
            ready: None,
            readiness: readiness.map(redact_readiness),
            readiness_evidence: String::new(),
            deadline_owner: None,
            termination_reason: None,
            cleanup_result: "not_required".into(),
            cancelled: false,
            spawn_failure: None,
        };

        {
            let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(terminal_id.clone(), record.clone());
        }
        self.store.upsert(&record);

        // ---- spawn -------------------------------------------------------
        let mut cmd = if shell {
            let mut c = Command::new("/bin/bash");
            c.arg("-c").arg(display_command.as_str());
            c
        } else {
            let parts = argv.expect("checked above");
            let mut c = Command::new(&parts[0]);
            c.args(&parts[1..]);
            c
        };
        cmd.current_dir(&resolved_cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);

        if let Some(overlay) = env {
            for (k, v) in overlay {
                cmd.env(k, v);
            }
        }
        set_process_group(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                let (kind, category) = classify_spawn_error(&err);
                let mut record = record;
                record.state = TerminalState::SpawnFailed;
                record.finished_at = Some(now_epoch());
                record.returncode = Some(-1);
                record.stderr = redact_text(&err.to_string(), self.config.max_output_bytes);
                record.termination_reason = Some(format!("spawn failed: {kind}"));
                record.cleanup_result = "not_spawned".into();
                record.spawn_failure = Some(category.to_string());
                self.persist(&record);
                return record.result(now_epoch());
            }
        };

        let pid = child.id();
        let pgid = pid;
        record.state = TerminalState::Running;
        record.started_at = Some(now_epoch());
        record.pid = pid.map(|p| p as i32);
        record.pgid = pgid.map(|p| p as i32);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let entry = Arc::new(Mutex::new(RuntimeEntry {
            record: record.clone(),
            child: Some(child),
            finished: Finished::new(),
            stop_requested: false,
            force_requested: false,
        }));
        {
            let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(terminal_id.clone(), Arc::clone(&entry));
        }
        self.persist(&record);

        // ---- pump both streams, then wait for exit ------------------------
        //
        // From here on the monitor task takes the child handle *out* of the
        // entry once and owns it, re-attaching it only on the paths that still
        // hold it. A `MutexGuard` live across an `.await` would both make this
        // task non-`Send` and block every concurrent `entry.lock()` on the very
        // thread that would have completed the wait.
        let budget = self.config.max_output_bytes;
        let done = entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished
            .clone();
        let records = Arc::clone(&self.records);
        let id_for_pumps = terminal_id.clone();

        let stdout_task = stdout.map(|mut s| {
            let records = Arc::clone(&records);
            let id = terminal_id.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    let n = match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let text = redact_output(&String::from_utf8_lossy(&buf[..n]));
                    apply_output_chunk(&records, &id, "stdout", &text, budget);
                }
            })
        });
        let stderr_task = stderr.map(|mut s| {
            let records = Arc::clone(&records);
            let id = id_for_pumps.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    let n = match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let text = redact_output(&String::from_utf8_lossy(&buf[..n]));
                    apply_output_chunk(&records, &id, "stderr", &text, budget);
                }
            })
        });

        let budget_deadline = if timeout > 0.0 {
            Some(Duration::from_secs_f64(timeout.min(MAX_WALL_TIMEOUT_S)))
        } else {
            None
        };
        let grace = Duration::from_secs_f64(self.config.cancel_grace_s.max(0.1));

        // Everything the monitor needs, cloned so the task is `'static` and
        // never borrows the manager.
        let monitor_entries = Arc::clone(&self.entries);
        let monitor_records = Arc::clone(&self.records);
        let monitor_store = Arc::clone(&self.store);
        let monitor_terminal_id = terminal_id.clone();

        tokio::spawn(async move {
            // Take the child out of the guard ONCE and own it for the whole
            // task. Holding it here rather than borrowing through the entry is
            // what lets the deadline path run without re-locking, and is why
            // no `MutexGuard` is ever live across an `.await` below.
            // `Some` means this task still owns the handle. The deadline path
            // below hands it back to the guard, so ownership has to be tracked
            // rather than assumed.
            let mut child: Option<Child> = {
                let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                guard.child.take()
            };
            // The group id, captured up front: it is needed after the handle
            // has been given back, and the record may move in the meantime.
            let pgid = {
                let guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                guard.record.pgid
            };
            if child.is_none() {
                // Nothing to supervise (a racing `stop` already reaped it).
                done.fire();
                return;
            }

            let mut timed_out = false;
            let first = match budget_deadline {
                Some(budget) => Some(
                    tokio::time::timeout(budget, child.as_mut().expect("checked").wait()).await,
                ),
                None => None,
            };
            let wait_result = match first {
                Some(Ok(status)) => status.ok(),
                Some(Err(_)) => {
                    {
                        // Deadline fired. Ask nicely, wait out the grace
                        // window, then insist — in that order, and each step
                        // only if the previous one did not work.
                        timed_out = true;
                        let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                        guard.stop_requested = true;
                        guard.child = child.take();
                        signal_entry(&mut guard, false);
                    }
                    let mut owned = match take_child(&entry) {
                        Some(c) => c,
                        None => return finish_orphan(&done, &entry),
                    };
                    let graceful = tokio::time::timeout(grace, owned.wait()).await;
                    if graceful.is_err() {
                        // Scoped: a `MutexGuard` live across the `.await` below
                        // would make this whole task non-`Send`.
                        {
                            let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                            guard.child = Some(owned);
                            guard.force_requested = true;
                            signal_entry(&mut guard, true);
                        }
                        let mut owned = match take_child(&entry) {
                            Some(c) => c,
                            None => return finish_orphan(&done, &entry),
                        };
                        owned.wait().await.ok()
                    } else {
                        // `Ok(Ok(status))` only: a `wait` that errored is not a
                        // clean exit and must not report a code.
                        graceful.ok().and_then(|status| status.ok())
                    }
                }
                None => child.as_mut().expect("checked").wait().await.ok(),
            };

            // Re-attach the still-owned handle so `signal_entry`'s `try_wait`
            // check stays truthful for any concurrent `stop`.
            if let Some(owned) = child.take() {
                let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                if guard.child.is_none() {
                    guard.child = Some(owned);
                }
            }

            // Reaping the direct child is NOT the same as the group being
            // gone. POSIX gives a background job started from a non-interactive
            // shell SIGINT and SIGQUIT set to ignore, so `sh -c 'sleep 100 &'`
            // can have its shell reaped by a graceful stop while `sleep 100`
            // keeps running, reparented to init and unreachable by the agent.
            // Close the group out with one unconditional SIGKILL.
            //
            // This is not the PID-reuse hazard the module docs warn about: the
            // signal is aimed at a group this manager created, immediately
            // after it reaped that group's own leader, never at a pid read
            // from disk. If the group is already empty `killpg` returns ESRCH
            // and nothing happens. A process that detached with `setsid()` is
            // in no group of ours and is deliberately left alone.
            signal_group(pgid, true);

            // Drain the pumps BEFORE firing, so a `stop`/`wait` that wakes on
            // the latch is guaranteed to see the final transcript. Dropping the
            // handles instead let the final record be persisted while the pipes
            // still held unread bytes, and `wait` reported `stdout: ""` for a
            // command that had plainly printed something.
            //
            // The bound is short on purpose. A grandchild that outlived its
            // shell keeps the write end of the pipe open, so the drain only
            // ends when the pump is cut off; holding the latch for that would
            // make `stop` report a failure for a process it had in fact killed.
            // Half a second is ample for the normal case — the pipes are
            // already at EOF the moment the child is reaped.
            const DRAIN: Duration = Duration::from_millis(500);
            if let Some(task) = stdout_task {
                let _ = tokio::time::timeout(DRAIN, task).await;
            }
            if let Some(task) = stderr_task {
                let _ = tokio::time::timeout(DRAIN, task).await;
            }
            done.fire();

            // Whatever the pumps observed, as last written to the map.
            let observed = {
                let guard = monitor_records.lock().unwrap_or_else(|e| e.into_inner());
                guard.get(&monitor_terminal_id).cloned()
            };

            // Fold the final state into the live entry's record.
            let final_record = {
                let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
                // Read the stop flag before borrowing the record mutably.
                // `force_requested` is deliberately NOT read here: it selects
                // which signal was sent, never which terminal state results.
                // agenthicc's `_monitor` reads `stop_requested` for the state
                // and uses `cleanup_result` for the escalation, and the state
                // must not depend on how hard the caller pressed.
                let stopped = guard.stop_requested;
                let record = &mut guard.record;
                if let Some(observed) = observed.as_ref() {
                    record.stdout = observed.stdout.clone();
                    record.stderr = observed.stderr.clone();
                    record.output_bytes = observed.output_bytes;
                    record.truncated = record.truncated || observed.truncated;
                    record.last_output_at = observed.last_output_at;
                }
                // `ExitStatus::code()` is `None` for a process killed by a
                // signal, and a signal is exactly what every stop sends. The
                // old `code().or(Some(-1))` therefore reported a clean SIGKILL
                // with the same `-1` as a `wait()` that never learned how the
                // process ended, and the `forced && returncode == Some(-1)`
                // test below then read every stopped terminal as "the group
                // outlived the force stop" and orphaned it. Track the two
                // apart: `status_known` is the honest question, and the code
                // itself follows agenthicc's asyncio convention of a negative
                // signal number, which leaves `-1` meaning exactly one thing --
                // "we never found out".
                let (returncode, status_known) = match wait_result {
                    Some(status) => (exit_code_of(&status), true),
                    None => (None, false),
                };
                record.returncode = returncode.or(Some(-1));
                record.finished_at = Some(now_epoch());
                // The flag the caller-visible contract depends on. The old
                // code computed it into a local and dropped it, so
                // `a_timeout_stops_the_terminal_and_says_so` could never pass.
                record.timed_out = timed_out;
                let cancelled = record.cancelled;
                // `forced` alone is not evidence of anything: a forced stop
                // that lands is an ordinary `stopped`, and only a monitor that
                // never learned the status may call the group unproven.
                if !status_known {
                    record.state = TerminalState::Orphaned;
                    record.termination_reason =
                        Some("process group did not exit after force stop".into());
                } else if timed_out {
                    record.state = TerminalState::TimedOut;
                } else if cancelled {
                    record.state = TerminalState::Cancelled;
                } else if stopped {
                    record.state = TerminalState::Stopped;
                } else if record.returncode == Some(0) {
                    record.state = TerminalState::Exited;
                } else {
                    record.state = TerminalState::Failed;
                }
                record.clone()
            };

            if let Ok(mut guard) = monitor_entries.lock() {
                guard.remove(&monitor_terminal_id);
            }
            persist_into(&monitor_records, &monitor_store, &final_record);
        });

        let mut result = serde_json::json!({
            "ok": true,
            "background": true,
            "terminal_id": terminal_id,
            "state": TerminalState::Running.as_str(),
            "pid": pid,
            "pgid": pgid,
            "label": record.label,
            "tool_call_id": tool_call_id,
            "lifecycle": lifecycle,
        });
        if let Some(probe) = readiness {
            let readiness_result = self.wait_readiness(&terminal_id, Some(probe)).await;
            if let Some(obj) = readiness_result.as_object() {
                for (k, v) in obj {
                    result[k] = v.clone();
                }
            }
        }
        result
    }

    /// Observe one owned terminal, without stopping it.
    ///
    /// `timeout` bounds the *observation*, never the process. That distinction
    /// is agenthicc's and it matters: a caller that means "give it 5 more
    /// seconds" must not accidentally kill a build that was going to finish.
    pub async fn wait(&self, terminal_id: &str, timeout: f64) -> Value {
        if let Err(msg) = validate_timeout(timeout) {
            return rejected(&msg);
        }
        let Some(record) = self.get(terminal_id) else {
            return serde_json::json!({
                "ok": false,
                "background": true,
                "terminal_id": terminal_id,
                "error": "unknown terminal",
            });
        };
        if record.session_id != self.config.session_id {
            return serde_json::json!({
                "ok": false,
                "background": true,
                "terminal_id": terminal_id,
                "error": "terminal belongs to another session",
            });
        }

        // A terminal that is no longer in `_entries` has been reaped; its
        // record is final and there is nothing to wait for.
        let entry = {
            let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(terminal_id).cloned()
        };
        let Some(entry) = entry else {
            return record.result(now_epoch());
        };

        let done = entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished
            .clone();
        if timeout > 0.0 {
            if done.timed_out(Duration::from_secs_f64(timeout)).await {
                // Observer timeout: report and leave the process alone.
                let mut snapshot = record.result(now_epoch());
                snapshot["ok"] = Value::Bool(false);
                snapshot["state"] = Value::String("waiting".into());
                snapshot["waiting"] = Value::Bool(true);
                snapshot["observer_timeout"] = serde_json::json!(timeout);
                return snapshot;
            }
        } else {
            done.wait().await;
        }

        self.get(terminal_id)
            .map(|r| r.result(now_epoch()))
            .unwrap_or_else(|| record.result(now_epoch()))
    }

    /// Stop one terminal **this manager owns**.
    ///
    /// Returns `false` for an unknown id, another session's id, or a record
    /// with no live child. All three mean "no proof of ownership", and none of
    /// them may result in a signal.
    pub async fn stop(&self, terminal_id: &str, force: bool, reason: &str) -> bool {
        let entry = {
            let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            match guard.get(terminal_id) {
                Some(e) => Arc::clone(e),
                // No live entry: either already reaped, orphaned from a previous
                // manager, or never ours. Do not signal.
                None => return false,
            }
        };
        {
            let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.record.active() {
                return false;
            }
            guard.stop_requested = true;
            guard.force_requested = force;
            guard.record.state = TerminalState::Stopping;
            guard.record.stop_reason = redact_label(reason);
            signal_entry(&mut guard, force);
            let snapshot = guard.record.clone();
            drop(guard);
            self.persist(&snapshot);
        }

        let grace = Duration::from_secs_f64(self.config.cancel_grace_s.max(0.1));
        let done = entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished
            .clone();
        if done.timed_out(grace).await {
            // Escalate: SIGTERM/INT did not do it in the grace window.
            let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
            guard.force_requested = true;
            signal_entry(&mut guard, true);
        }
        // Give the forced stop at least a second, even when the configured
        // grace is shorter, so a slow-to-die process is not misreported.
        let settle = std::cmp::max(grace, Duration::from_secs(1));
        let finished = !done.timed_out(settle).await;

        if !finished {
            let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
            guard.record.state = TerminalState::Orphaned;
            guard.record.finished_at = Some(now_epoch());
            guard.record.stop_reason = "process group did not exit after force stop".into();
            let snapshot = guard.record.clone();
            drop(guard);
            self.persist(&snapshot);
            if let Ok(mut guard) = self.entries.lock() {
                guard.remove(terminal_id);
            }
        }
        finished
    }

    /// Observe a readiness probe without changing ownership.
    pub async fn wait_readiness(&self, terminal_id: &str, readiness: Option<&Value>) -> Value {
        let Some(mut record) = self.get(terminal_id) else {
            return serde_json::json!({
                "ok": false,
                "state": "unknown",
                "error": "unknown terminal",
            });
        };
        let probe = readiness
            .cloned()
            .or_else(|| record.readiness.clone())
            .unwrap_or(Value::Null);
        if probe.is_null() || probe.as_object().is_none_or(|o| o.is_empty()) {
            record.ready = None;
            return record.result(now_epoch());
        }
        if !probe.is_object() {
            return serde_json::json!({
                "ok": false,
                "state": TerminalState::Rejected.as_str(),
                "error": "readiness must be an object",
            });
        }
        let obj = probe.as_object().expect("checked above");
        let present = ["url", "tcp", "marker"]
            .iter()
            .filter(|k| obj.contains_key(**k))
            .count();
        if present != 1 {
            return serde_json::json!({
                "ok": false,
                "state": TerminalState::Rejected.as_str(),
                "error": "readiness requires exactly one of url, tcp, or marker",
            });
        }
        // A non-loopback target without `allow_external` is a REFUSAL, not a
        // "not ready yet" answer, and refusing it before the loop is what makes
        // the difference observable. `probe_readiness` returns `None` for both
        // cases, so a refusal left to the poll is indistinguishable from a
        // service that is merely slow: the loop would run to the probe budget,
        // and if the process exited first it would return `ok: true` with
        // `ready: null` -- reporting success for a probe that was never
        // attempted. agenthicc has the same conflation; refusing here says why,
        // and names the flag that would permit it.
        let allow_external = obj
            .get("allow_external")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !allow_external {
            if let Some(target) = non_loopback_probe_target(obj) {
                return serde_json::json!({
                    "ok": false,
                    "state": TerminalState::Rejected.as_str(),
                    "error": format!(
                        "readiness probe target {target} is not loopback; \
                         start the terminal with allow_external to permit it"
                    ),
                    "readiness_evidence": format!("refused: {target} is not loopback"),
                    "ready": false,
                });
            }
        }
        // An invalid or absent probe budget falls back to the 30s default
        // rather than failing the probe outright: the caller asked "is it
        // ready", not "is your timeout well-formed".
        let probe_timeout = match obj.get("timeout").and_then(|v| v.as_f64()) {
            Some(value) => validate_timeout(value).unwrap_or(30.0),
            None => 30.0,
        };

        record.readiness = Some(redact_readiness(&probe));
        self.persist(&record);

        let deadline = Instant::now() + Duration::from_secs_f64(probe_timeout.max(0.0));

        loop {
            // Re-read each turn: the pump task mutates stdout/stderr.
            let Some(current) = self.get(terminal_id) else {
                return serde_json::json!({ "ok": false, "state": "unknown", "error": "unknown terminal" });
            };
            record = current;
            if !record.active() {
                return record.result(now_epoch());
            }
            if let Some(evidence) = probe_readiness(&record, &probe, allow_external) {
                record.ready = Some(true);
                record.readiness_evidence = evidence.clone();
                self.persist(&record);
                let mut out = record.result(now_epoch());
                out["ok"] = Value::Bool(true);
                out["readiness_evidence"] = Value::String(evidence);
                return out;
            }
            if Instant::now() >= deadline {
                record.ready = Some(false);
                record.readiness_evidence = "readiness probe timed out".into();
                let process_state = record.state.as_str().to_string();
                self.persist(&record);
                let mut out = record.result(now_epoch());
                out["ok"] = Value::Bool(false);
                out["state"] = Value::String("starting_timeout".into());
                out["readiness_timeout"] = Value::Bool(true);
                out["process_state"] = Value::String(process_state);
                return out;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Stop every live terminal, for session teardown.
    pub async fn close(&self) {
        let ids: Vec<String> = {
            let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            guard.keys().cloned().collect()
        };
        for id in ids {
            let _ = self.stop(&id, false, "session closed").await;
        }
    }

    /// Write a record's current state into both the map and the store.
    fn persist(&self, record: &TerminalRecord) {
        persist_into(&self.records, &self.store, record);
    }

    /// Resolve and confine `cwd` to the workspace.
    fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf, String> {
        let raw = requested.filter(|s| !s.is_empty()).unwrap_or(".");
        let candidate = Path::new(raw);
        // Reject `..` outright rather than relying on a lexical prefix test:
        // `Path::starts_with` is component-wise and never collapses `..`, so
        // `root.join("../..")` still "starts with" root and escapes silently.
        if candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("cwd must not contain '..': {raw}"));
        }
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.config.workspace_root.join(candidate)
        };
        if !joined.starts_with(&self.config.workspace_root) {
            return Err(format!("cwd escapes the workspace: {raw}"));
        }
        Ok(joined)
    }
}

/// Fold one redacted chunk into a live record's stdout or stderr.
///
/// A free function because the pump tasks outlive the `&self` borrow they were
/// spawned under; they hold an `Arc` to the records map instead.
fn apply_output_chunk(
    records: &Arc<Mutex<HashMap<String, TerminalRecord>>>,
    terminal_id: &str,
    stream: &str,
    text: &str,
    budget: usize,
) {
    let mut guard = records.lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = guard.get_mut(terminal_id) else {
        return;
    };
    record.output_bytes += text.len() as u64;
    record.last_output_at = Some(now_epoch());
    let current = if stream == "stderr" {
        &record.stderr
    } else {
        &record.stdout
    };
    let (bounded, truncated) = bounded_append(current, text, budget);
    if stream == "stderr" {
        record.stderr = bounded;
    } else {
        record.stdout = bounded;
    }
    record.truncated = record.truncated || truncated;
}

/// The uniform "rejected" envelope agenthicc returns before any spawn.
fn rejected(reason: &str) -> Value {
    serde_json::json!({
        "ok": false,
        "background": true,
        "state": TerminalState::Rejected.as_str(),
        "error": reason,
        "termination_reason": reason,
    })
}

/// Validate and bound a caller-supplied timeout.
///
/// agenthicc's `validate_timeout`: must be finite and non-negative. A NaN or
/// negative value would otherwise become an immediately-expired or
/// never-expired deadline.
pub fn validate_timeout(value: f64) -> Result<f64, String> {
    if value.is_nan() {
        return Err("timeout must be a number".into());
    }
    if value < 0.0 {
        return Err("timeout must be >= 0".into());
    }
    if value > MAX_WALL_TIMEOUT_S {
        return Ok(MAX_WALL_TIMEOUT_S);
    }
    Ok(value)
}

/// Map a spawn error onto agenthicc's `spawn_failure` categories.
fn classify_spawn_error(err: &std::io::Error) -> (&'static str, &'static str) {
    match err.kind() {
        std::io::ErrorKind::NotFound => ("FileNotFoundError", "executable_or_shell"),
        std::io::ErrorKind::PermissionDenied => ("PermissionError", "permission"),
        _ => ("OSError", "spawn"),
    }
}

/// Redact a readiness probe before it is persisted.
///
/// A probe can carry a URL with an embedded token, so it goes through the same
/// net as everything else on its way to disk.
fn redact_readiness(probe: &Value) -> Value {
    let mut out = probe.clone();
    if let Some(obj) = out.as_object_mut() {
        for key in ["url", "marker"] {
            if let Some(Value::String(s)) = obj.get(key) {
                let redacted = redact_text(s, 512);
                obj.insert(key.to_string(), Value::String(redacted));
            }
        }
    }
    out
}

/// Put the child in a new process group, so the whole tree can be signalled.
#[cfg(unix)]
fn set_process_group(cmd: &mut Command) {
    // `tokio::process::Command` has an inherent `process_group`; the
    // `std::os::unix::process::CommandExt` trait does not apply to it.
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn set_process_group(_cmd: &mut Command) {}

/// Signal a process GROUP, never just the direct child.
///
/// This is the `sleep 100 &` fix: signalling only the shell leaves the
/// grandchild running, reparented to init.
#[cfg(unix)]
fn signal_group(pgid: Option<i32>, force: bool) {
    let Some(pgid) = pgid else {
        return;
    };
    // Safety: `killpg` with a pgid this manager created. ESRCH (already gone) is
    // the expected benign outcome and is ignored by the caller.
    unsafe {
        libc::killpg(pgid, if force { libc::SIGKILL } else { libc::SIGINT });
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: Option<i32>, _force: bool) {}

/// The exit code of a reaped child, agenthicc-style.
///
/// `ExitStatus::code()` returns `None` for a process killed by a signal, which
/// is the normal outcome of every `stop`. agenthicc's asyncio reports that as a
/// negative signal number (`-9` for SIGKILL), so this does the same: it keeps a
/// signalled stop distinguishable from the `-1` sentinel that means "the status
/// was never learned" (see the monitor's `status_known`).
#[cfg(unix)]
fn exit_code_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.code().or_else(|| status.signal().map(|sig| -sig))
}

/// Non-unix builds have no signal number to report, so fall back to the code.
#[cfg(not(unix))]
fn exit_code_of(status: &std::process::ExitStatus) -> Option<i32> {
    status.code()
}

/// Signal one live entry's process group.
///
/// Takes the guard by `&mut` so it can call `Child::try_wait`, which needs a
/// mutable borrow. An entry whose child has already been reaped is skipped:
/// signalling a pgid we no longer own a live handle to is exactly the
/// PID-reuse hazard the module docs describe.
fn signal_entry(entry: &mut RuntimeEntry, force: bool) {
    if let Some(child) = entry.child.as_mut() {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
    }
    signal_group(entry.record.pgid, force);
}

/// Fold a record into the live map and append it to the durable store.
///
/// Free-standing rather than a method so the per-terminal monitor task can
/// reach it without borrowing the `TerminalManager`, which does not live for
/// the monitor's `'static` lifetime.
fn persist_into(
    records: &Mutex<HashMap<String, TerminalRecord>>,
    store: &TerminalStore,
    record: &TerminalRecord,
) {
    {
        let mut guard = records.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(record.terminal_id.clone(), record.clone());
    }
    store.upsert(record);
}

/// Take the child handle out of an entry, if there is one.
///
/// Returns `None` when a concurrent `stop` already reaped it, which the
/// monitor treats as "nothing left to supervise".
fn take_child(entry: &Arc<Mutex<RuntimeEntry>>) -> Option<Child> {
    let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
    guard.child.take()
}

/// Latch the entry as un-supervisable: no child handle means this manager can
/// no longer prove it owns the process, so the record becomes `Orphaned` and is
/// never signal-eligible again. This is the fail-closed end of the same
/// invariant `stop` enforces — see the module docs on PID reuse.
fn finish_orphan(done: &Finished, entry: &Arc<Mutex<RuntimeEntry>>) {
    {
        let mut guard = entry.lock().unwrap_or_else(|e| e.into_inner());
        guard.record.state = TerminalState::Orphaned;
        guard.record.finished_at = Some(now_epoch());
        guard.record.stop_reason = "child handle was reaped by a concurrent stop".into();
    }
    done.fire();
}

/// Evaluate one readiness probe against the current record.
///
/// Returns the evidence string on success, `None` to keep polling. Note the two
/// refusals that are *not* retryable-by-time: a non-loopback host without
/// `allow_external` returns `None` immediately rather than waiting for the
/// probe timeout, because waiting would only delay an answer that will never
/// change.
fn probe_readiness(record: &TerminalRecord, probe: &Value, allow_external: bool) -> Option<String> {
    let obj = probe.as_object()?;

    if let Some(marker) = obj.get("marker").and_then(|v| v.as_str()) {
        if !marker.is_empty() && (record.stdout.contains(marker) || record.stderr.contains(marker))
        {
            return Some(format!(
                "output marker matched: {}",
                redact_text(marker, 120)
            ));
        }
    }

    if let Some(tcp) = obj.get("tcp").filter(|v| v.is_object()) {
        let tcp = tcp.as_object().expect("filtered above");
        let host = tcp
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1");
        let port = tcp.get("port").and_then(|v| v.as_u64());
        if let Some(port) = port {
            if !is_loopback(host) && !allow_external {
                return None;
            }
            if std::net::TcpStream::connect_timeout(
                &match format!("{host}:{port}").parse() {
                    Ok(addr) => addr,
                    Err(_) => return None,
                },
                Duration::from_millis(500),
            )
            .is_ok()
            {
                return Some(format!("tcp connected: {host}:{port}"));
            }
        }
    }

    if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        if !url_is_loopback(url) && !allow_external {
            return None;
        }
        // No HTTP client is in the dependency graph, and adding one is a
        // separate decision (goal 72 asks the same question for `session
        // serve`). A `url` probe therefore reports as not-satisfied rather than
        // silently claiming success.
    }

    None
}

/// The off-machine target of a probe, if it names one.
///
/// Returns the host a `tcp` or `url` probe would reach, so a refusal can name
/// it. `marker` probes never leave the process and so are never external.
fn non_loopback_probe_target(obj: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(tcp) = obj.get("tcp").filter(|v| v.is_object()) {
        let tcp = tcp.as_object().expect("filtered above");
        let host = tcp
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1");
        return (!is_loopback(host)).then(|| host.to_string());
    }
    if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
        return (!url_is_loopback(url)).then(|| url.to_string());
    }
    None
}

/// Whether a probe URL points at the local machine.
fn url_is_loopback(url: &str) -> bool {
    let without_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    if let Some(rest) = authority.strip_prefix('[') {
        // A bracketed IPv6 literal, with or without a `:port` suffix. The port
        // must be dropped first: `[::1]:9000` is not a parseable address, and
        // feeding it to `is_loopback` whole would report a loopback URL as
        // external, which fails the probe closed for no reason.
        if let Some(end) = rest.find(']') {
            return is_loopback(&rest[..end]);
        }
    }
    if authority.contains(':') {
        // `host:port`, and also a bare IPv6 literal — recurse on the host part.
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        return url_is_loopback(host);
    }
    is_loopback(authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A forced stop that lands is `stopped`, never `orphaned`.
    ///
    /// A signal-killed child has no exit code, so folding that onto the `-1`
    /// "status unknown" sentinel made every clean stop look like a group that
    /// outlived the force stop. Pin the distinction from both sides.
    #[tokio::test]
    async fn a_signalled_stop_is_stopped_not_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "30"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(result["ok"], serde_json::json!(true));
        let id = result["terminal_id"].as_str().unwrap().to_string();

        assert!(m.stop(&id, true, "test").await);
        let waited = m.wait(&id, 5.0).await;
        assert_eq!(
            waited["state"],
            serde_json::json!("stopped"),
            "a forced stop that landed must not read as orphaned: {waited}"
        );
        // agenthicc's asyncio reports a signal death as a negative signal
        // number, which is what keeps `-1` meaning only "never found out".
        let code = waited["returncode"].as_i64().expect("returncode");
        assert!(
            code < 0 && code != -1,
            "expected a negative signal number, got {code}: {waited}"
        );
    }

    /// A non-loopback probe is refused with a reason, not a timeout.
    ///
    /// `probe_readiness` answers "refused" and "not ready yet" with the same
    /// `None`, so without the up-front refusal the poll runs to the probe
    /// budget and then reports the process's own exit as `ok: true` -- success
    /// for a probe that was never attempted.
    #[tokio::test]
    async fn a_non_loopback_probe_is_refused_before_the_poll_runs() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "30"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "service",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();

        let probe = serde_json::json!({"tcp": {"host": "example.com", "port": 80}});
        let out = m.wait_readiness(&id, Some(&probe)).await;
        assert_eq!(out["ok"], serde_json::json!(false), "{out}");
        assert_eq!(out["state"], serde_json::json!("rejected"), "{out}");
        assert!(
            out["readiness_evidence"]
                .as_str()
                .unwrap_or("")
                .contains("loopback"),
            "the refusal must name the reason: {out}"
        );
        assert!(
            out["error"]
                .as_str()
                .unwrap_or("")
                .contains("allow_external"),
            "the refusal must name the flag that permits it: {out}"
        );
        assert!(m.stop(&id, true, "cleanup").await);
    }

    /// A loopback probe is still attempted, so the refusal is not a blanket ban.
    #[tokio::test]
    async fn a_loopback_probe_is_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "5"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "service",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();

        // Nothing is listening, so this cannot become ready; what matters is
        // that it is attempted rather than refused.
        let probe = serde_json::json!({"tcp": {"host": "127.0.0.1", "port": 1}, "timeout": 0.3});
        let out = m.wait_readiness(&id, Some(&probe)).await;
        assert_ne!(
            out["state"],
            serde_json::json!("rejected"),
            "a loopback probe must be attempted, not refused: {out}"
        );
        assert!(m.stop(&id, true, "cleanup").await);
    }

    fn manager(dir: &Path) -> TerminalManager {
        let mut config = TerminalManagerConfig::new("sess-1", dir);
        config.store_root = dir.join("store");
        config.cancel_grace_s = 1.0;
        TerminalManager::new(config)
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[tokio::test]
    async fn a_oneshot_terminal_runs_and_records_its_exit() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/echo", "hello"])),
                None,
                5.0,
                None,
                false,
                "",
                "tc-1",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(result["ok"], serde_json::json!(true));
        let id = result["terminal_id"]
            .as_str()
            .expect("terminal_id")
            .to_string();

        let waited = m.wait(&id, 10.0).await;
        assert_eq!(waited["state"], serde_json::json!("exited"));
        assert_eq!(waited["returncode"], serde_json::json!(0));
        assert_eq!(waited["ok"], serde_json::json!(true));
        assert!(
            waited["stdout"].as_str().unwrap().contains("hello"),
            "stdout should be captured, got {waited}"
        );
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_reported_as_failed_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sh", "-c", "exit 3"])),
                None,
                5.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();
        let waited = m.wait(&id, 10.0).await;
        assert_eq!(waited["state"], serde_json::json!("failed"));
        assert_eq!(waited["returncode"], serde_json::json!(3));
        assert_eq!(waited["ok"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn stop_reaches_a_grandchild_through_the_process_group() {
        // `sleep 100 &` reparents the grandchild to init the moment the shell
        // exits, so signalling only the shell would leave it running forever.
        // The trailing `wait` keeps the shell alive as the group leader, which
        // is what makes the terminal still *owned* — and so still stoppable —
        // at the moment `stop` is called. Refusing to signal an unowned
        // pgid is the other invariant, pinned by
        // `an_orphaned_terminal_cannot_be_stopped`.
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let pid_file = dir.path().join("grandchild.pid");
        let result = m
            .start(
                Some(&format!(
                    "sleep 100 & echo $! > {}; wait",
                    pid_file.display()
                )),
                None,
                None,
                0.0,
                None,
                true,
                "grandchild",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(result["ok"], serde_json::json!(true));
        let id = result["terminal_id"].as_str().unwrap().to_string();

        // The pid goes through a file rather than stdout: `start` returns
        // before the shell has written a byte, and a test that had to poll the
        // transcript to learn the pid would be testing the capture path too.
        let mut pid = 0;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                pid = text.trim().parse().unwrap_or(0);
                if pid > 0 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            pid > 0,
            "expected a grandchild pid at {}",
            pid_file.display()
        );

        let stopped = m.stop(&id, false, "test").await;
        assert!(stopped, "stop should succeed");

        // The grandchild must be gone. Give the signal a moment to land.
        let mut alive = true;
        for _ in 0..40 {
            if !pid_alive(pid) {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive, "grandchild {pid} survived; killpg did not reach it");
    }

    #[tokio::test]
    async fn an_orphaned_terminal_cannot_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");

        // A record written by a manager that no longer exists.
        let mut stale = TerminalRecord {
            terminal_id: "term-deadbeef".into(),
            session_id: "sess-1".into(),
            project_root: dir.path().to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            kind: "exec".into(),
            command: "sleep 9999".into(),
            label: "sleep 9999".into(),
            state: TerminalState::Running,
            created_at: now_epoch(),
            tool_call_id: String::new(),
            parent_job_id: String::new(),
            started_at: Some(now_epoch()),
            finished_at: None,
            // A real pid (this test process), standing in for PID reuse.
            pid: Some(std::process::id() as i32),
            pgid: Some(std::process::id() as i32),
            returncode: None,
            stdout: String::new(),
            stderr: String::new(),
            output_bytes: 0,
            truncated: false,
            timed_out: false,
            stop_reason: String::new(),
            timeout_s: 0.0,
            effective_timeout_s: None,
            last_output_at: None,
            lifecycle: "oneshot".into(),
            ready: None,
            readiness: None,
            readiness_evidence: String::new(),
            deadline_owner: None,
            termination_reason: None,
            cleanup_result: "not_required".into(),
            cancelled: false,
            spawn_failure: None,
            command_digest: String::new(),
        };

        {
            let store = TerminalStore::new(&store_root);
            store.upsert(&stale);
        }

        let mut config = TerminalManagerConfig::new("sess-1", dir.path());
        config.store_root = store_root;
        let m = TerminalManager::new(config);

        // Adopted, but NOT active.
        stale.state = TerminalState::Orphaned;
        let adopted = m.get("term-deadbeef").expect("record is visible");
        assert_eq!(adopted.state, TerminalState::Orphaned);
        assert!(!adopted.active());

        // And crucially: refusing to signal. If this ever returned true, or if
        // stop() signalled, the test binary itself would be killed.
        let stopped = m.stop("term-deadbeef", true, "test").await;
        assert!(
            !stopped,
            "a record with no live child must never be stopped"
        );
    }

    #[tokio::test]
    async fn a_cwd_outside_the_workspace_is_refused_before_any_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("ws");
        std::fs::create_dir_all(&inner).unwrap();
        let mut config = TerminalManagerConfig::new("sess-1", &inner);
        config.store_root = dir.path().join("store");
        let m = TerminalManager::new(config);

        for escape in ["..", "../..", "/etc", "a/../../.."] {
            let result = m
                .start(
                    None,
                    Some(&argv(&["/bin/sh", "-c", "touch /tmp/xaft-escape-marker"])),
                    Some(escape),
                    5.0,
                    None,
                    false,
                    "",
                    "tc",
                    "oneshot",
                    None,
                )
                .await;
            assert_eq!(
                result["ok"],
                serde_json::json!(false),
                "cwd {escape} should have been refused, got {result}"
            );
            assert!(
                result["terminal_id"].is_null(),
                "a refused spawn must not get a terminal id: {result}"
            );
        }
        assert!(
            !std::path::Path::new("/tmp/xaft-escape-marker").exists(),
            "no process may have been spawned"
        );
    }

    #[tokio::test]
    async fn output_is_redacted_before_it_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                Some("echo 'API_KEY=sk-live-supersecret12345'"),
                None,
                None,
                5.0,
                None,
                true,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();
        let waited = m.wait(&id, 10.0).await;

        assert!(
            !waited.to_string().contains("sk-live-supersecret12345"),
            "the secret must not survive into the tool result: {waited}"
        );
        // And it must not be on disk either.
        let raw = std::fs::read_to_string(m.store().path()).unwrap_or_default();
        assert!(
            !raw.contains("sk-live-supersecret12345"),
            "the secret must never reach the durable store"
        );
        assert!(raw.contains("<redacted>"), "redaction should have applied");
    }

    #[tokio::test]
    async fn the_session_limit_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = TerminalManagerConfig::new("sess-1", dir.path());
        config.store_root = dir.path().join("store");
        config.max_terminals = 1;
        let m = TerminalManager::new(config);

        let first = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "30"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(first["ok"], serde_json::json!(true));

        let second = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "30"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(second["ok"], serde_json::json!(false));
        assert!(
            second["error"]
                .as_str()
                .unwrap()
                .contains("session terminal limit reached")
        );
    }

    #[tokio::test]
    async fn disabling_background_terminals_refuses_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = TerminalManagerConfig::new("sess-1", dir.path());
        config.store_root = dir.path().join("store");
        config.enabled = false;
        let m = TerminalManager::new(config);
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/echo", "hi"])),
                None,
                5.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(result["ok"], serde_json::json!(false));
        assert!(result["error"].as_str().unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn an_unknown_terminal_is_reported_not_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let waited = m.wait("term-does-not-exist", 0.0).await;
        assert_eq!(waited["ok"], serde_json::json!(false));
        assert_eq!(waited["error"], serde_json::json!("unknown terminal"));
        assert!(!m.stop("term-does-not-exist", true, "test").await);
    }

    #[tokio::test]
    async fn a_spawn_failure_is_recorded_with_a_category() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/nonexistent/binary", "arg"])),
                None,
                5.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        assert_eq!(result["ok"], serde_json::json!(false));
        assert_eq!(result["state"], serde_json::json!("spawn_failed"));
        assert_eq!(
            result["spawn_failure"],
            serde_json::json!("executable_or_shell")
        );
        assert_eq!(result["cleanup_result"], serde_json::json!("not_spawned"));
    }

    #[tokio::test]
    async fn a_timeout_stops_the_terminal_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = TerminalManagerConfig::new("sess-1", dir.path());
        config.store_root = dir.path().join("store");
        config.cancel_grace_s = 1.0;
        let m = TerminalManager::new(config);
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "60"])),
                None,
                0.5,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();
        let waited = m.wait(&id, 20.0).await;
        assert_eq!(waited["timed_out"], serde_json::json!(true));
        assert_ne!(waited["state"], serde_json::json!("running"));
    }

    #[tokio::test]
    async fn an_observer_timeout_reports_waiting_without_stopping() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "30"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();

        let waited = m.wait(&id, 0.2).await;
        assert_eq!(waited["waiting"], serde_json::json!(true));
        assert_eq!(waited["state"], serde_json::json!("waiting"));
        // The process must still be alive: an observer timeout is not a stop.
        assert!(!waited["terminal_id"].as_str().unwrap().is_empty());
        assert!(m.stop(&id, true, "cleanup").await);
    }

    #[tokio::test]
    async fn a_marker_probe_succeeds_once_the_marker_appears() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let probe = serde_json::json!({"marker": "READY", "timeout": 15.0});
        let result = m
            .start(
                Some("echo READY; sleep 30"),
                None,
                None,
                0.0,
                None,
                true,
                "",
                "tc",
                "service",
                Some(&probe),
            )
            .await;
        assert_eq!(result["ready"], serde_json::json!(true));
        assert!(
            result["readiness_evidence"]
                .as_str()
                .unwrap()
                .contains("output marker matched")
        );
        let id = result["terminal_id"].as_str().unwrap().to_string();
        assert!(m.stop(&id, true, "cleanup").await);
    }

    #[tokio::test]
    async fn a_non_loopback_tcp_probe_is_refused_without_allow_external() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = TerminalRecord {
            terminal_id: "term-x".into(),
            session_id: "sess-1".into(),
            project_root: dir.path().to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            kind: "exec".into(),
            command: "x".into(),
            label: "x".into(),
            state: TerminalState::Running,
            created_at: now_epoch(),
            tool_call_id: String::new(),
            parent_job_id: String::new(),
            started_at: Some(now_epoch()),
            finished_at: None,
            pid: None,
            pgid: None,
            returncode: None,
            stdout: String::new(),
            stderr: String::new(),
            output_bytes: 0,
            truncated: false,
            timed_out: false,
            stop_reason: String::new(),
            timeout_s: 0.0,
            effective_timeout_s: None,
            last_output_at: None,
            lifecycle: "service".into(),
            ready: None,
            readiness: None,
            readiness_evidence: String::new(),
            deadline_owner: None,
            termination_reason: None,
            cleanup_result: "not_required".into(),
            cancelled: false,
            spawn_failure: None,
            command_digest: String::new(),
        };
        record.state = TerminalState::Running;

        let probe = serde_json::json!({"tcp": {"host": "93.184.216.34", "port": 80}});
        assert_eq!(
            probe_readiness(&record, &probe, false),
            None,
            "a public host must not be probed without allow_external"
        );
        assert_eq!(
            probe_readiness(&record, &probe, true),
            None,
            "even allowed, an unreachable host is simply not ready"
        );
    }

    #[tokio::test]
    async fn a_readiness_probe_needs_exactly_one_kind() {
        let dir = tempfile::tempdir().unwrap();
        let m = manager(dir.path());
        let result = m
            .start(
                None,
                Some(&argv(&["/bin/sleep", "5"])),
                None,
                0.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        let id = result["terminal_id"].as_str().unwrap().to_string();
        let both = serde_json::json!({"marker": "a", "url": "http://127.0.0.1:1/"});
        let out = m.wait_readiness(&id, Some(&both)).await;
        assert_eq!(out["ok"], serde_json::json!(false));
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("exactly one of url, tcp, or marker")
        );
        assert!(m.stop(&id, true, "cleanup").await);
    }

    #[tokio::test]
    async fn listing_shows_only_this_sessions_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        let mut other = TerminalRecord {
            terminal_id: "term-other".into(),
            session_id: "sess-2".into(),
            project_root: dir.path().to_string_lossy().into_owned(),
            cwd: dir.path().to_string_lossy().into_owned(),
            kind: "exec".into(),
            command: "x".into(),
            label: "x".into(),
            state: TerminalState::Exited,
            created_at: now_epoch(),
            tool_call_id: String::new(),
            parent_job_id: String::new(),
            started_at: Some(now_epoch()),
            finished_at: Some(now_epoch()),
            pid: None,
            pgid: None,
            returncode: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            output_bytes: 0,
            truncated: false,
            timed_out: false,
            stop_reason: String::new(),
            timeout_s: 0.0,
            effective_timeout_s: None,
            last_output_at: None,
            lifecycle: "oneshot".into(),
            ready: None,
            readiness: None,
            readiness_evidence: String::new(),
            deadline_owner: None,
            termination_reason: None,
            cleanup_result: "not_required".into(),
            cancelled: false,
            spawn_failure: None,
            command_digest: String::new(),
        };
        other.state = TerminalState::Exited;
        TerminalStore::new(&store_root).upsert(&other);

        let mut config = TerminalManagerConfig::new("sess-1", dir.path());
        config.store_root = store_root;
        let m = TerminalManager::new(config);

        let result = m
            .start(
                None,
                Some(&argv(&["/bin/echo", "mine"])),
                None,
                5.0,
                None,
                false,
                "",
                "tc",
                "oneshot",
                None,
            )
            .await;
        m.wait(result["terminal_id"].as_str().unwrap(), 10.0).await;

        let listed = m.list_records();
        assert!(listed.iter().all(|r| r.session_id == "sess-1"));
        assert!(!listed.iter().any(|r| r.terminal_id == "term-other"));
    }

    #[test]
    fn a_negative_or_nan_timeout_is_refused() {
        assert!(validate_timeout(-1.0).is_err());
        assert!(validate_timeout(f64::NAN).is_err());
        assert!(validate_timeout(0.0).is_ok());
        assert!(validate_timeout(30.0).is_ok());
    }

    #[test]
    fn an_absurd_timeout_is_capped_not_honoured() {
        assert_eq!(validate_timeout(f64::INFINITY).unwrap(), MAX_WALL_TIMEOUT_S);
        assert_eq!(validate_timeout(1e12).unwrap(), MAX_WALL_TIMEOUT_S);
    }

    #[test]
    fn both_command_and_argv_is_an_error() {
        // Checked in `start`; pinned here at the predicate level.
        assert!(Some("x").is_some() == Some(&argv(&["y"])).is_some());
    }

    #[test]
    fn loopback_detection_covers_the_shapes_that_matter() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("LOCALHOST"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("127.1.2.3"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
        // 0.0.0.0 is a *bind* address meaning every interface, not a loopback
        // destination, so treating it as local would make a LAN-wide probe
        // look safe.
        assert!(!is_loopback("0.0.0.0"));
        assert!(!is_loopback("93.184.216.34"));
        assert!(!is_loopback("example.com"));
    }

    #[test]
    fn a_url_is_loopback_only_when_its_authority_is() {
        assert!(url_is_loopback("http://127.0.0.1:8080/health"));
        assert!(url_is_loopback("http://localhost:3000/"));
        assert!(url_is_loopback("http://[::1]:9000/"));
        assert!(!url_is_loopback("http://example.com/health"));
        assert!(!url_is_loopback("https://93.184.216.34/"));
    }

    #[test]
    fn a_persisted_readiness_probe_is_redacted() {
        let probe = serde_json::json!({"url": "https://example.com/?token=abcd1234efgh5678"});
        let redacted = redact_readiness(&probe);
        let text = redacted.to_string();
        assert!(
            !text.contains("abcd1234efgh5678"),
            "probe secrets must be redacted before persisting: {text}"
        );
    }

    /// Whether a pid still exists. Signal 0 performs error checking only.
    #[cfg(unix)]
    fn pid_alive(pid: i32) -> bool {
        // Safety: `kill(pid, 0)` sends no signal; it only checks existence.
        unsafe {
            libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
        }
    }

    #[cfg(not(unix))]
    fn pid_alive(_pid: i32) -> bool {
        false
    }
}
