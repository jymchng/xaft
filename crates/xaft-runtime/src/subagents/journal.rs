//! The session's durable subagent journal (agenthicc's `ConversationJournal`
//! subagent records, `memory/journal.py`).
//!
//! One JSONL file per session, `<workspace>/.xaft/subagents/<session>.jsonl`
//! — the per-workspace `.xaft/` the terminal and worker stores already use.
//! Three record kinds, in agenthicc's field names with `schema_version: 1`:
//!
//! * `subagent_worker_result` — one worker's complete outcome, written
//!   (and fsynced) *before* the worker's completion is announced, so output
//!   produced before a cancellation is never lost;
//! * `subagent_pool_result` — a pool's complete aggregate, written only when
//!   every worker succeeded: it is the resume cache `spawn_subagents` reuses
//!   for the same task set;
//! * `agent_message_event` — the parent/worker communication lifecycle.
//!
//! Reading tolerates an interrupted write: a corrupt line ends the fold, as
//! agenthicc treats a bad trailing line.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

/// Where a session's journal lives.
pub fn journal_path(workspace: &Path, session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    workspace
        .join(".xaft")
        .join("subagents")
        .join(format!("{safe}.jsonl"))
}

/// The journal of one session.
#[derive(Debug)]
pub struct SubagentJournal {
    path: PathBuf,
    /// Serialises appends; holds the next `seq`.
    next_seq: Mutex<Option<u64>>,
}

impl SubagentJournal {
    /// The journal of `session_id` in `workspace`. Nothing is created until
    /// the first record is written.
    pub fn for_session(workspace: &Path, session_id: &str) -> Self {
        Self::at(journal_path(workspace, session_id))
    }

    /// A journal at an explicit path.
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            next_seq: Mutex::new(None),
        }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record, durably. Failures are logged, never raised: the
    /// journal must not make a valid subagent exchange fail.
    fn write(&self, kind: &str, fields: Value) {
        let mut seq = self.next_seq.lock().unwrap_or_else(|p| p.into_inner());
        let current = *seq.get_or_insert_with(|| self.read_records().len() as u64);
        let mut record = Map::new();
        record.insert("seq".into(), json!(current));
        record.insert("kind".into(), json!(kind));
        record.insert("schema_version".into(), json!(1));
        if let Value::Object(fields) = fields {
            record.extend(fields);
        }
        match self.append(&Value::Object(record)) {
            Ok(()) => *seq = Some(current + 1),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "subagent journal write failed")
            }
        }
    }

    fn append(&self, record: &Value) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // Worker output can carry anything the workspace holds.
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.sync_all()
    }

    /// Every well-formed record, stopping at the first corrupt line.
    pub fn read_records(&self) -> Vec<Value> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut records = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value @ Value::Object(_)) => records.push(value),
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        records
    }

    fn fold(&self, kind: &str) -> Vec<Value> {
        self.read_records()
            .into_iter()
            .filter(|r| r.get("kind").and_then(Value::as_str) == Some(kind))
            .collect()
    }

    /// agenthicc's `subagent_worker_result`.
    pub fn worker_result(&self, pool_id: &str, fingerprint: &str, result: &super::SubagentResult) {
        self.write(
            "subagent_worker_result",
            json!({
                "pool_id": pool_id,
                "fingerprint": fingerprint,
                "task_id": result.task_id,
                "agent_type": result.agent_type,
                "label": result.label,
                "ok": result.ok,
                "text": result.text,
                "error": result.error,
                "duration_ms": result.duration_ms,
                "tool_calls": result.tool_calls,
                "changed_paths": result.changed_paths,
            }),
        );
    }

    /// agenthicc's `subagent_pool_result` — callers write only complete,
    /// fully successful pools.
    pub fn pool_result(&self, fingerprint: &str, aggregate: &super::AggregatedResult) {
        self.write(
            "subagent_pool_result",
            json!({
                "pool_id": aggregate.pool_id,
                "fingerprint": fingerprint,
                "total": aggregate.total,
                "succeeded": aggregate.succeeded,
                "failed": aggregate.failed,
                "text": aggregate.text,
            }),
        );
    }

    /// agenthicc's `agent_message_event`.
    pub fn message_event(
        &self,
        pool_id: &str,
        conversation_id: &str,
        parent_run_id: &str,
        policy_revision: &str,
        event_kind: &str,
        payload: Value,
    ) {
        let bounded = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        self.write(
            "agent_message_event",
            json!({
                "pool_id": bounded(pool_id, 128),
                "conversation_id": bounded(conversation_id, 128),
                "parent_run_id": bounded(parent_run_id, 128),
                "policy_revision": bounded(policy_revision, 128),
                "event_kind": bounded(event_kind, 64),
                "payload": payload,
            }),
        );
    }

    /// Worker records, in journal order.
    pub fn worker_results(&self) -> Vec<Value> {
        self.fold("subagent_worker_result")
    }

    /// Communication records, in journal order.
    pub fn message_events(&self) -> Vec<Value> {
        self.fold("agent_message_event")
    }

    /// agenthicc's `fold_subagent_pool_results`: complete pools only.
    pub fn pool_results(&self) -> Vec<Value> {
        self.fold("subagent_pool_result")
            .into_iter()
            .filter(|r| {
                r.get("failed").and_then(Value::as_u64) == Some(0)
                    && r.get("fingerprint").is_some_and(Value::is_string)
                    && r.get("text").is_some_and(Value::is_string)
            })
            .collect()
    }

    /// The most recent complete aggregate for `fingerprint`, if any.
    pub fn cached_pool_text(&self, fingerprint: &str) -> Option<String> {
        self.pool_results()
            .into_iter()
            .rev()
            .find(|r| r.get("fingerprint").and_then(Value::as_str) == Some(fingerprint))
            .and_then(|r| r.get("text").and_then(Value::as_str).map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::{AggregatedResult, SubagentResult};

    fn aggregate(failed: usize, text: &str) -> AggregatedResult {
        AggregatedResult {
            pool_id: "p1".into(),
            total: 2,
            succeeded: 2 - failed,
            failed,
            text: text.into(),
        }
    }

    #[test]
    fn records_carry_agenthiccs_kinds_and_a_rising_seq() {
        let dir = tempfile::TempDir::new().unwrap();
        let journal = SubagentJournal::for_session(dir.path(), "sess-1");
        journal.worker_result(
            "p1",
            "fp",
            &SubagentResult {
                task_id: "task-0".into(),
                agent_type: "explorer".into(),
                label: "explorer #1".into(),
                ok: true,
                text: "found it".into(),
                error: String::new(),
                duration_ms: 12.0,
                tool_calls: vec!["read_file".into()],
                changed_paths: vec![],
            },
        );
        journal.pool_result("fp", &aggregate(0, "=== explorer #1 ==="));
        let records = journal.read_records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["kind"], "subagent_worker_result");
        assert_eq!(records[0]["schema_version"], 1);
        assert_eq!(records[0]["seq"], 0);
        assert_eq!(records[1]["seq"], 1);
        assert_eq!(records[0]["tool_calls"], json!(["read_file"]));
        assert!(journal.path().ends_with(".xaft/subagents/sess-1.jsonl"));
        // A second handle continues the sequence.
        let again = SubagentJournal::for_session(dir.path(), "sess-1");
        again.pool_result("fp2", &aggregate(0, "x"));
        assert_eq!(again.read_records()[2]["seq"], 2);
    }

    #[test]
    fn only_complete_pools_are_a_cache_and_the_newest_wins() {
        let dir = tempfile::TempDir::new().unwrap();
        let journal = SubagentJournal::for_session(dir.path(), "s");
        journal.pool_result("fp", &aggregate(1, "partial"));
        assert_eq!(journal.cached_pool_text("fp"), None);
        journal.pool_result("fp", &aggregate(0, "old"));
        journal.pool_result("fp", &aggregate(0, "new"));
        assert_eq!(journal.cached_pool_text("fp").as_deref(), Some("new"));
        assert_eq!(journal.cached_pool_text("other"), None);
    }

    #[test]
    fn a_corrupt_tail_ends_the_fold() {
        let dir = tempfile::TempDir::new().unwrap();
        let journal = SubagentJournal::for_session(dir.path(), "s");
        journal.pool_result("fp", &aggregate(0, "kept"));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .unwrap();
        file.write_all(b"{\"kind\": \"subagent_pool_res").unwrap();
        assert_eq!(journal.cached_pool_text("fp").as_deref(), Some("kept"));
    }

    #[test]
    fn session_ids_cannot_escape_the_journal_directory() {
        let path = journal_path(Path::new("/w"), "../../etc/passwd");
        assert_eq!(path, Path::new("/w/.xaft/subagents/______etc_passwd.jsonl"));
    }
}
