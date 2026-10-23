//! Durable parallel-worker manifests — the data behind `/workers` (parity
//! slash row 21).
//!
//! agenthicc's `/workers` lists `ManifestStore().list()` — one JSON manifest
//! per parallel orchestration, each holding a list of worker tasks — filtered
//! to the current session. This module writes and reads manifests in that
//! **same wire format** (`agenthicc.worktrees.model.ParallelManifest.to_dict()`:
//! `orchestration_id`, `parent_session_id`, `status`, `tasks[]` with
//! `task_id` / `description` / `status` / `worktree_id`, …), one file per
//! orchestration, written atomically.
//!
//! # Who writes them in xaft
//!
//! xaft's parallel fan-out is the planner's `explore_repository` tool: a
//! `SubagentPool` that runs up to eight explorer subagents concurrently, one
//! per file. [`WorkerRecorder`] records each call as one orchestration whose
//! tasks are those subagents, so `/workers` shows real worker status for the
//! session. The recorder writes under the workspace's `.xaft/workers/` — the
//! same per-workspace `.xaft/` the background-terminal store uses.
//!
//! agenthicc's *other* kind of parallel worker — worktree-isolated coding
//! workers from `parallel_code_plan` — belongs to the `worktrees` subsystem
//! (section-1 rows 50–53). Its manifests use this format too, so the reader
//! here lists them alongside the explore fan-outs when a store root for them
//! is supplied; their `worktree_id` fills the column the explore workers leave
//! as `—` (explorer subagents share the session's workspace).
//!
//! The reader is deliberately lenient: it parses manifests as JSON values and
//! skips anything unreadable, so one corrupt file — or a manifest written by a
//! newer build with extra fields — never hides the rest (agenthicc skips bad
//! manifests the same way).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Whether `id` is a plain file name (agenthicc's `Path(id).name == id`,
/// stricter: `\` is refused too).
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Where a workspace's parallel fan-outs are recorded: `<workspace>/.xaft/workers`.
pub fn workspace_worker_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".xaft").join("workers")
}

/// One directory of `{orchestration_id}.json` manifests.
#[derive(Debug, Clone)]
pub struct WorkerManifestStore {
    root: PathBuf,
}

impl WorkerManifestStore {
    /// A store rooted at `root`; the directory is created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory holding the manifests.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Atomically write one manifest (write-then-rename).
    pub fn save(&self, manifest: &Value) -> std::io::Result<()> {
        let id = manifest
            .get("orchestration_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !is_safe_id(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid orchestration id",
            ));
        }
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!("{id}.json"));
        let tmp = path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(manifest)?;
        text.push('\n');
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    /// Every readable manifest, most recently updated first, ties broken on
    /// `orchestration_id` (agenthicc's `ManifestStore.list` order).
    pub fn list(&self) -> Vec<Value> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut manifests: Vec<Value> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
            .filter(|v| {
                v.get("orchestration_id")
                    .and_then(Value::as_str)
                    .is_some_and(is_safe_id)
            })
            .collect();
        sort_manifests(&mut manifests);
        manifests
    }
}

/// Sort manifests newest-updated first, then by id.
pub fn sort_manifests(manifests: &mut [Value]) {
    manifests.sort_by(|a, b| {
        let ua = a.get("updated_at").and_then(Value::as_f64).unwrap_or(0.0);
        let ub = b.get("updated_at").and_then(Value::as_f64).unwrap_or(0.0);
        ub.total_cmp(&ua).then_with(|| {
            let ia = a
                .get("orchestration_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let ib = b
                .get("orchestration_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            ia.cmp(ib)
        })
    });
}

/// Every manifest under `roots` that `session_id` owns.
///
/// agenthicc's filter: with no session id every manifest is visible,
/// otherwise only those whose `parent_session_id` matches.
pub fn manifests_for_session(roots: &[PathBuf], session_id: Option<&str>) -> Vec<Value> {
    let mut all: Vec<Value> = roots
        .iter()
        .flat_map(|root| WorkerManifestStore::new(root).list())
        .filter(|m| match session_id {
            None => true,
            Some(id) => m.get("parent_session_id").and_then(Value::as_str) == Some(id),
        })
        .collect();
    sort_manifests(&mut all);
    all
}

/// Records one parallel fan-out as a durable orchestration.
///
/// Recording is best-effort: a write failure is logged and never fails the
/// fan-out it describes — losing a status row is better than losing the work.
#[derive(Debug, Clone)]
pub struct WorkerRecorder {
    store: WorkerManifestStore,
    parent_session_id: String,
    repository: String,
}

impl WorkerRecorder {
    /// Record into `root` on behalf of `parent_session_id`, for the
    /// repository at `repository`.
    pub fn new(
        root: impl Into<PathBuf>,
        parent_session_id: impl Into<String>,
        repository: impl Into<String>,
    ) -> Self {
        Self {
            store: WorkerManifestStore::new(root),
            parent_session_id: parent_session_id.into(),
            repository: repository.into(),
        }
    }

    /// The store this recorder writes to.
    pub fn store(&self) -> &WorkerManifestStore {
        &self.store
    }

    /// Open an orchestration with one `running` task per `(task_id,
    /// description)`, and persist it. Returns the manifest to finish later.
    pub fn begin(&self, tasks: &[(String, String)]) -> Value {
        let now = now_secs();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tasks: Vec<Value> = tasks
            .iter()
            .map(|(task_id, description)| {
                json!({
                    "task_id": task_id,
                    "description": description,
                    "dependencies": [],
                    "status": "running",
                    "worker_session_id": "",
                    "worktree_id": "",
                    "result": null,
                    "error": "",
                    "created_at": now,
                    "updated_at": now,
                })
            })
            .collect();
        let manifest = json!({
            "orchestration_id": id,
            "parent_session_id": self.parent_session_id,
            "run_id": "",
            "repository": self.repository,
            "parent_branch": "",
            "base_commit": "",
            "status": "running",
            "tasks": tasks,
            "worktrees": [],
            "created_at": now,
            "updated_at": now,
            "last_error": "",
            "verification": [],
        });
        self.persist(&manifest);
        manifest
    }

    /// Close an orchestration: each task's outcome is `Ok(())` or
    /// `Err(error)`, in task order. The orchestration is `complete` unless
    /// every task failed, or `cancelled` is set, in which case it is `failed`.
    pub fn finish(&self, manifest: &mut Value, outcomes: &[Result<(), String>], cancelled: bool) {
        let now = now_secs();
        let mut failures = 0usize;
        if let Some(tasks) = manifest.get_mut("tasks").and_then(Value::as_array_mut) {
            for (task, outcome) in tasks.iter_mut().zip(outcomes) {
                let (status, error) = match outcome {
                    Ok(()) => ("completed", String::new()),
                    Err(e) => {
                        failures += 1;
                        ("failed", e.clone())
                    }
                };
                task["status"] = json!(status);
                task["error"] = json!(error);
                task["updated_at"] = json!(now);
            }
            if cancelled {
                for task in tasks.iter_mut() {
                    if task["status"] == json!("running") {
                        task["status"] = json!("cancelled");
                        task["updated_at"] = json!(now);
                    }
                }
            }
        }
        let all_failed = !outcomes.is_empty() && failures == outcomes.len();
        let (status, last_error) = if cancelled {
            ("failed", "cancelled".to_string())
        } else if all_failed {
            ("failed", "every worker failed".to_string())
        } else {
            ("complete", String::new())
        };
        manifest["status"] = json!(status);
        manifest["last_error"] = json!(last_error);
        manifest["updated_at"] = json!(now);
        self.persist(manifest);
    }

    fn persist(&self, manifest: &Value) {
        if let Err(e) = self.store.save(manifest) {
            tracing::warn!(error = %e, root = %self.store.root().display(), "worker manifest not persisted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_fan_out_round_trips_in_agenthiccs_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = WorkerRecorder::new(dir.path(), "sess-1", "/repo");
        let mut manifest = recorder.begin(&[
            ("explore-1".into(), "src/a.rs".into()),
            ("explore-2".into(), "src/b.rs".into()),
        ]);
        let listed = recorder.store().list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["status"], json!("running"));
        assert_eq!(listed[0]["tasks"][0]["status"], json!("running"));
        assert_eq!(listed[0]["parent_session_id"], json!("sess-1"));

        recorder.finish(&mut manifest, &[Ok(()), Err("timeout".into())], false);
        let listed = recorder.store().list();
        assert_eq!(listed[0]["status"], json!("complete"));
        assert_eq!(listed[0]["tasks"][0]["status"], json!("completed"));
        assert_eq!(listed[0]["tasks"][1]["status"], json!("failed"));
        assert_eq!(listed[0]["tasks"][1]["error"], json!("timeout"));
        assert_eq!(listed[0]["tasks"][1]["worktree_id"], json!(""));
    }

    #[test]
    fn a_fan_out_where_every_worker_failed_or_was_cancelled_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = WorkerRecorder::new(dir.path(), "sess-1", "/repo");
        let mut all_failed = recorder.begin(&[("explore-1".into(), "a".into())]);
        recorder.finish(&mut all_failed, &[Err("boom".into())], false);
        assert_eq!(all_failed["status"], json!("failed"));

        let mut cancelled = recorder.begin(&[("explore-1".into(), "a".into())]);
        recorder.finish(&mut cancelled, &[], true);
        assert_eq!(cancelled["status"], json!("failed"));
        assert_eq!(cancelled["last_error"], json!("cancelled"));
        assert_eq!(cancelled["tasks"][0]["status"], json!("cancelled"));
    }

    #[test]
    fn listing_filters_by_session_merges_roots_and_skips_garbage() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        WorkerRecorder::new(a.path(), "mine", "/r").begin(&[("t".into(), "d".into())]);
        WorkerRecorder::new(a.path(), "theirs", "/r").begin(&[("t".into(), "d".into())]);
        WorkerRecorder::new(b.path(), "mine", "/r").begin(&[("t".into(), "d".into())]);
        std::fs::write(a.path().join("broken.json"), "{not json").unwrap();
        std::fs::write(a.path().join("../escape.json"), "{}").ok();

        let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
        assert_eq!(manifests_for_session(&roots, Some("mine")).len(), 2);
        assert_eq!(manifests_for_session(&roots, Some("theirs")).len(), 1);
        assert_eq!(manifests_for_session(&roots, None).len(), 3);
        assert!(manifests_for_session(&[a.path().join("missing")], None).is_empty());
    }

    #[test]
    fn an_unsafe_orchestration_id_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkerManifestStore::new(dir.path());
        assert!(store.save(&json!({"orchestration_id": "../x"})).is_err());
        assert!(store.save(&json!({})).is_err());
    }
}
