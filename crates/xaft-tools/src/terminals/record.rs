//! Durable, display-safe state for one background terminal.
//!
//! # Why records are separate from processes
//!
//! A [`TerminalRecord`] outlives the process it describes. It is written to an
//! append-only JSONL file the moment the process starts and rewritten as the
//! process progresses, so a user can inspect a terminal that finished hours
//! ago. That durability is also the security boundary this module exists to
//! defend: see [`TerminalManager`](super::manager::TerminalManager)'s note on
//! never signalling a persisted PID.
//!
//! # Why `result()` and `to_dict()` are different shapes
//!
//! [`TerminalRecord::result`] is the tool's answer — the compact envelope the
//! agent reads, carrying exactly the fields needed to decide what to do next.
//! [`TerminalRecord::to_dict`] is the full durable row. Keeping them separate
//! means widening the stored schema never silently widens what a tool call
//! returns, and vice versa.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Durable lifecycle states for an owned terminal.
///
/// A transliteration of agenthicc's `TerminalState`. The wire values are the
/// lowercase strings the agent sees, so they are part of the tool contract and
/// must not be renamed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    /// Accepted, not yet spawned.
    Starting,
    /// Spawned and live.
    Running,
    /// A stop has been signalled but the process has not reaped yet.
    Stopping,
    /// Finished with return code 0.
    Exited,
    /// Finished with a non-zero return code.
    Failed,
    /// Killed by the command's own wall-clock deadline.
    TimedOut,
    /// The owning task was cancelled.
    Cancelled,
    /// Never spawned (bad program, bad cwd, permission denied).
    SpawnFailed,
    /// Refused before any spawn (limits, bad arguments).
    Rejected,
    /// Stopped on request.
    Stopped,
    /// Ownership could not be proven, or the process outlived its manager.
    ///
    /// This is the fail-closed sink for every state we cannot vouch for: the
    /// `#[serde(other)]` arm catches a state string this build does not know,
    /// and `Default` catches a row that has no `state` at all. Both mean
    /// "do not let this ever be signalled", which is why this variant is the
    /// only one carrying the attribute and why it is the enum's default.
    #[serde(other)]
    #[default]
    Orphaned,
}

impl TerminalState {
    /// Whether this state means "a process may still be running".
    ///
    /// agenthicc's `_ACTIVE_STATES`. This is the predicate that decides whether
    /// `stop` may signal, and whether a recovered record becomes `Orphaned`.
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    /// The wire string, matching agenthicc's `StrEnum` values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::SpawnFailed => "spawn_failed",
            Self::Rejected => "rejected",
            Self::Stopped => "stopped",
            Self::Orphaned => "orphaned",
        }
    }
}

/// Durable metadata for one terminal process.
///
/// Field-for-field a transliteration of agenthicc's `TerminalRecord`, minus the
/// Python-only conveniences. `command` and `label` are redacted on the way in
/// (by `super::redact::redact_label`) rather than on the way out, so an
/// unredacted value is never even resident in the record.
///
/// The struct-level `#[serde(default)]` is deliberate, not laziness. This file
/// is durable, append-only and user-visible, so it will outlive the binary that
/// wrote it: a row written by a newer build may carry fields this build has
/// never heard of, and a hand-edited row may be missing some entirely. Falling
/// back to defaults keeps such a row readable instead of skipping it, and every
/// default here fails closed — most importantly [`TerminalState::default`] is
/// [`TerminalState::Orphaned`], which is never signal-eligible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalRecord {
    /// Stable handle returned to the agent, e.g. `term-1a2b3c4d`.
    pub terminal_id: String,
    /// Owning session. A record from another session is never visible.
    pub session_id: String,
    /// Project root, used for the per-project concurrency limit.
    pub project_root: String,
    /// Resolved working directory.
    pub cwd: String,
    /// `exec` when argv is passed literally, `shell` when a shell parses it.
    pub kind: String,
    /// Redacted, bounded display form of the command.
    pub command: String,
    /// Redacted, bounded display label.
    pub label: String,
    /// Current lifecycle state.
    pub state: TerminalState,
    /// Unix epoch seconds at which the terminal was accepted.
    pub created_at: f64,
    /// The tool call that started it, for correlating agent transcripts.
    #[serde(default)]
    pub tool_call_id: String,
    /// Owning job, so a session teardown can find its terminals.
    #[serde(default)]
    pub parent_job_id: String,
    /// Epoch seconds at which the process was spawned.
    #[serde(default)]
    pub started_at: Option<f64>,
    /// Epoch seconds at which the process was reaped.
    #[serde(default)]
    pub finished_at: Option<f64>,
    /// Child PID. **Never** signalled from this value alone.
    #[serde(default)]
    pub pid: Option<i32>,
    /// Process-group id. This, not `pid`, is what `killpg` takes.
    #[serde(default)]
    pub pgid: Option<i32>,
    /// Exit status, once reaped.
    #[serde(default)]
    pub returncode: Option<i32>,
    /// Redacted, bounded stdout tail.
    #[serde(default)]
    pub stdout: String,
    /// Redacted, bounded stderr tail.
    #[serde(default)]
    pub stderr: String,
    /// Total bytes observed, before bounding — may exceed the stored text.
    #[serde(default)]
    pub output_bytes: u64,
    /// Whether either stream hit the capture budget.
    #[serde(default)]
    pub truncated: bool,
    /// Whether the wall-clock deadline fired.
    #[serde(default)]
    pub timed_out: bool,
    /// Why a stop was requested.
    #[serde(default)]
    pub stop_reason: String,
    /// Requested wall-clock budget, seconds. 0 means none.
    #[serde(default)]
    pub timeout_s: f64,
    /// Budget actually applied after the precedence rules.
    #[serde(default)]
    pub effective_timeout_s: Option<f64>,
    /// Epoch seconds of the most recent output.
    #[serde(default)]
    pub last_output_at: Option<f64>,
    /// `oneshot` or `service`.
    #[serde(default)]
    pub lifecycle: String,
    /// Tri-state readiness: `None` when no probe ran.
    #[serde(default)]
    pub ready: Option<bool>,
    /// The redacted probe definition.
    #[serde(default)]
    pub readiness: Option<serde_json::Value>,
    /// Human-readable proof the probe succeeded.
    #[serde(default)]
    pub readiness_evidence: String,
    /// Which layer owned the deadline.
    #[serde(default)]
    pub deadline_owner: Option<String>,
    /// Why the terminal ended, in prose.
    #[serde(default)]
    pub termination_reason: Option<String>,
    /// How cleanup went: `not_required`, `graceful_timeout`, `force_timeout`,
    /// `cleanup_unproven`, or `not_spawned`.
    #[serde(default)]
    pub cleanup_result: String,
    /// Whether the owning task was cancelled.
    #[serde(default)]
    pub cancelled: bool,
    /// Why the spawn failed, if it did.
    #[serde(default)]
    pub spawn_failure: Option<String>,
    /// SHA-256 of the unredacted command, used to detect a duplicate service.
    ///
    /// A digest rather than the command text, because the command may hold a
    /// secret and the store is durable.
    #[serde(default)]
    pub command_digest: String,
}

impl TerminalRecord {
    /// Seconds since start, using the finish time when already reaped.
    pub fn elapsed_s(&self, now: f64) -> f64 {
        let end = self.finished_at.unwrap_or(now);
        let start = self.started_at.unwrap_or(self.created_at);
        (end - start).max(0.0)
    }

    /// Whether this record describes a possibly-live process.
    pub fn active(&self) -> bool {
        self.state.active()
    }

    /// The stable tool-result envelope for this terminal.
    ///
    /// `ok` is deliberately narrow: only a clean `Exited` with return code 0
    /// counts as success. A `Stopped` terminal that exited 0 is not a success,
    /// because the agent did not get the result it asked for.
    pub fn result(&self, now: f64) -> serde_json::Value {
        serde_json::json!({
            "ok": self.state == TerminalState::Exited && self.returncode == Some(0),
            "background": true,
            "terminal_id": self.terminal_id,
            "tool_call_id": self.tool_call_id,
            "parent_job_id": self.parent_job_id,
            "command_kind": self.kind,
            "command": self.command,
            "command_digest": self.command_digest,
            "cwd": self.cwd,
            "state": self.state.as_str(),
            "stdout": self.stdout,
            "stderr": self.stderr,
            "returncode": self.returncode.unwrap_or(-1),
            "duration_ms": (self.elapsed_s(now) * 1000.0 * 10.0).round() / 10.0,
            "timed_out": self.timed_out,
            "cancelled": self.cancelled,
            "truncated": self.truncated,
            "output_bytes": self.output_bytes,
            "label": self.label,
            "lifecycle": self.lifecycle,
            "ready": self.ready,
            "readiness_evidence": self.readiness_evidence,
            "deadline": self.deadline(),
            "deadline_owner": self.deadline_owner,
            "termination_reason": self.termination_reason.clone().or_else(|| {
                if self.stop_reason.is_empty() { None } else { Some(self.stop_reason.clone()) }
            }),
            "cleanup_result": self.cleanup_result,
            "spawn_failure": self.spawn_failure,
            "pid": self.pid,
            "pgid": self.pgid,
        })
    }

    /// The deadline sub-object, or `None` when no budget was ever requested.
    fn deadline(&self) -> Option<serde_json::Value> {
        if self.timeout_s <= 0.0 && self.deadline_owner.is_none() {
            return None;
        }
        Some(serde_json::json!({
            "requested_s": self.timeout_s,
            "effective_s": self.effective_timeout_s.unwrap_or(self.timeout_s),
            "owner": self.deadline_owner.clone().unwrap_or_else(|| "command".into()),
        }))
    }

    /// The full durable row, as written to the store.
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

/// Append-only JSONL store of terminal records.
///
/// # Why append-only rather than one file per terminal
///
/// One file per terminal means the agent can delete a record to hide a command
/// from `inspect_terminal`. Append-only means the last line wins on read, and
/// a tampered or truncated line is skipped rather than crashing the manager —
/// see [`TerminalStore::records`]. The cost is that a hot terminal rewrites its
/// whole row, which is bounded by [`super::redact::DEFAULT_MAX_OUTPUT`].
#[derive(Debug, Clone)]
pub struct TerminalStore {
    path: PathBuf,
}

impl TerminalStore {
    /// A store rooted at `root`, creating the directory if needed.
    ///
    /// Falls back to the path itself when the directory cannot be created, so
    /// a read-only data dir degrades to an in-memory-ish terminal rather than
    /// a hard failure at construction.
    pub fn new(root: &Path) -> Self {
        let path = root.join("terminals.jsonl");
        let _ = std::fs::create_dir_all(root);
        Self { path }
    }

    /// The file this store appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one snapshot of `record`.
    ///
    /// A write failure is deliberately *not* fatal. Losing a durable row is bad,
    /// but killing a running terminal because the disk filled up is worse, and
    /// the agent still needs to be able to stop it.
    pub fn upsert(&self, record: &TerminalRecord) {
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(error = %err, "terminal record is not serialisable; not persisted");
                return;
            }
        };
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()
        })();
        if let Err(err) = result {
            tracing::warn!(error = %err, path = %self.path.display(), "terminal record not persisted");
        }
    }

    /// Every record, latest snapshot per `terminal_id`, newest first.
    ///
    /// Tolerates a corrupt or partially-written line: an unparseable line is
    /// skipped with a warning, because a half-written record must not prevent
    /// the agent from stopping the terminals that *are* readable.
    pub fn records(&self) -> Vec<TerminalRecord> {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let mut latest: BTreeMap<String, TerminalRecord> = BTreeMap::new();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TerminalRecord>(&line) {
                Ok(record) => {
                    latest.insert(record.terminal_id.clone(), record);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "skipping unparseable terminal record line");
                }
            }
        }
        let mut out: Vec<TerminalRecord> = latest.into_values().collect();
        out.sort_by(|a, b| b.created_at.total_cmp(&a.created_at));
        out
    }

    /// Drop records whose `finished_at` is older than `older_than_s`.
    ///
    /// Returns how many were removed. Records that are still active are never
    /// pruned — they may be about to be inspected.
    pub fn prune(&self, older_than_s: f64, now: f64) -> usize {
        let all = self.records();
        let (keep, drop): (Vec<_>, Vec<_>) = all.iter().partition(|r| match r.finished_at {
            Some(finished) => now - finished < older_than_s,
            None => true,
        });
        if drop.is_empty() {
            return 0;
        }
        let kept_lines = keep
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect::<Vec<_>>()
            .join("\n");
        let body = if kept_lines.is_empty() {
            String::new()
        } else {
            format!("{kept_lines}\n")
        };
        if let Err(err) = std::fs::write(&self.path, body) {
            tracing::warn!(error = %err, "terminal store prune failed");
            return 0;
        }
        drop.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, state: TerminalState) -> TerminalRecord {
        TerminalRecord {
            terminal_id: id.into(),
            session_id: "s1".into(),
            project_root: "/p".into(),
            cwd: "/p".into(),
            kind: "exec".into(),
            command: "sleep 1".into(),
            label: "sleep 1".into(),
            state,
            created_at: 100.0,
            tool_call_id: String::new(),
            parent_job_id: String::new(),
            started_at: Some(100.0),
            finished_at: None,
            pid: Some(1),
            pgid: Some(1),
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
            command_digest: "d".into(),
        }
    }

    #[test]
    fn active_states_match_agenthicc() {
        for s in [
            TerminalState::Starting,
            TerminalState::Running,
            TerminalState::Stopping,
        ] {
            assert!(s.active(), "{s:?} should be active");
        }
        for s in [
            TerminalState::Exited,
            TerminalState::Failed,
            TerminalState::TimedOut,
            TerminalState::Cancelled,
            TerminalState::SpawnFailed,
            TerminalState::Rejected,
            TerminalState::Stopped,
            TerminalState::Orphaned,
        ] {
            assert!(!s.active(), "{s:?} should not be active");
        }
    }

    #[test]
    fn state_wire_values_round_trip() {
        for s in [
            TerminalState::Starting,
            TerminalState::Running,
            TerminalState::Stopping,
            TerminalState::Exited,
            TerminalState::Failed,
            TerminalState::TimedOut,
            TerminalState::Cancelled,
            TerminalState::SpawnFailed,
            TerminalState::Rejected,
            TerminalState::Stopped,
            TerminalState::Orphaned,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(serde_json::from_str::<TerminalState>(&json).unwrap(), s);
        }
    }

    #[test]
    fn only_a_clean_exit_is_ok() {
        let mut r = record("t1", TerminalState::Exited);
        r.returncode = Some(0);
        assert_eq!(r.result(100.0)["ok"], serde_json::json!(true));

        r.returncode = Some(1);
        assert_eq!(r.result(100.0)["ok"], serde_json::json!(false));

        // A stopped terminal that happened to exit 0 is NOT a success: the agent
        // did not get the result it asked for.
        let mut s = record("t2", TerminalState::Stopped);
        s.returncode = Some(0);
        assert_eq!(s.result(100.0)["ok"], serde_json::json!(false));
    }

    #[test]
    fn result_defaults_a_missing_returncode_to_minus_one() {
        let r = record("t3", TerminalState::Running);
        assert_eq!(r.result(100.0)["returncode"], serde_json::json!(-1));
    }

    #[test]
    fn deadline_is_absent_when_no_budget_was_requested() {
        let r = record("t4", TerminalState::Running);
        assert_eq!(r.result(100.0)["deadline"], serde_json::Value::Null);
    }

    #[test]
    fn deadline_reports_the_effective_budget_and_owner() {
        let mut r = record("t5", TerminalState::TimedOut);
        r.timeout_s = 30.0;
        r.effective_timeout_s = Some(30.0);
        r.deadline_owner = Some("command".into());
        let deadline = r.result(100.0)["deadline"].clone();
        assert_eq!(deadline["effective_s"], serde_json::json!(30.0));
        assert_eq!(deadline["owner"], serde_json::json!("command"));
    }

    #[test]
    fn elapsed_uses_finish_time_once_reaped() {
        let mut r = record("t6", TerminalState::Running);
        assert_eq!(r.elapsed_s(130.0), 30.0);
        r.finished_at = Some(110.0);
        assert_eq!(r.elapsed_s(999.0), 10.0);
    }

    #[test]
    fn store_keeps_the_last_snapshot_per_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        let mut r = record("t1", TerminalState::Running);
        store.upsert(&r);
        r.state = TerminalState::Exited;
        r.returncode = Some(0);
        store.upsert(&r);

        let all = store.records();
        assert_eq!(all.len(), 1, "one terminal, not two snapshots");
        assert_eq!(all[0].state, TerminalState::Exited);
    }

    #[test]
    fn store_orders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        let mut older = record("old", TerminalState::Exited);
        older.created_at = 10.0;
        let mut newer = record("new", TerminalState::Exited);
        newer.created_at = 20.0;
        store.upsert(&older);
        store.upsert(&newer);

        let ids: Vec<_> = store.records().into_iter().map(|r| r.terminal_id).collect();
        assert_eq!(ids, vec!["new".to_string(), "old".to_string()]);
    }

    #[test]
    fn a_corrupt_line_does_not_hide_the_readable_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        store.upsert(&record("t1", TerminalState::Exited));
        // A partially-written line, as a crash mid-append would leave.
        std::fs::write(
            dir.path().join("terminals.jsonl"),
            format!(
                "{}\n{{\"terminal_id\": \"t2\", \"st\n",
                serde_json::to_string(&record("t1", TerminalState::Exited)).unwrap()
            ),
        )
        .unwrap();

        let all = store.records();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].terminal_id, "t1");
    }

    #[test]
    fn a_record_with_an_unknown_state_deserialises_as_orphaned() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        std::fs::write(
            dir.path().join("terminals.jsonl"),
            "{\"terminal_id\":\"t9\",\"state\":\"not-a-real-state\"}\n",
        )
        .unwrap();

        let all = store.records();
        assert_eq!(all.len(), 1);
        // An unrecognised state is not "running": defaulting to Orphaned means
        // it can never be signalled, which is the safe direction.
        assert_eq!(all[0].state, TerminalState::Orphaned);
        assert!(!all[0].active());
    }

    #[test]
    fn prune_drops_only_finished_records_older_than_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        let mut old = record("old", TerminalState::Exited);
        old.finished_at = Some(0.0);
        let mut fresh = record("fresh", TerminalState::Exited);
        fresh.finished_at = Some(9_000.0);
        let mut running = record("running", TerminalState::Running);
        running.finished_at = None;
        store.upsert(&old);
        store.upsert(&fresh);
        store.upsert(&running);

        assert_eq!(store.prune(1_000.0, 9_000.0), 1);
        let ids: Vec<_> = store.records().into_iter().map(|r| r.terminal_id).collect();
        assert_eq!(ids.len(), 2, "the live terminal must never be pruned");
        assert!(ids.contains(&"fresh".to_string()));
        assert!(ids.contains(&"running".to_string()));
    }

    #[test]
    fn prune_of_nothing_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = TerminalStore::new(dir.path());
        store.upsert(&record("t1", TerminalState::Exited));
        assert_eq!(store.prune(0.0, 0.0), 0);
        assert_eq!(store.records().len(), 1);
    }
}
