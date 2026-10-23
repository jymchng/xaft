//! Durable state of one phased workflow run, and where it is checkpointed.
//!
//! This is the xaft analogue of agenthicc's `WorkflowContext` + `WorkflowRun`
//! pair and its PRD-156 checkpoint store. Everything a resumed run needs is
//! here and serialisable: the intent, every phase output, the iteration
//! counters, the selected next edge, and the workflow's own key/value state
//! (goals, artifacts, authoring drafts). Live resources — providers, tools,
//! the conversation — are never stored; the conversation is re-attached from
//! the session's conversation store by its id.
//!
//! A checkpoint is written after every phase transition with a write-to-temp
//! then rename, so a crash leaves either the previous checkpoint or the new
//! one, never a torn file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// In progress — or the process died mid-run (still resumable).
    Running,
    /// Stopped on request; resumable.
    Paused,
    /// A phase failed; resumable from that phase.
    Failed,
    /// Cancelled by the user; resumable.
    Cancelled,
    /// Every phase ran to completion.
    Complete,
    /// An exit transition (e.g. `exit_workflow`) ended the run early.
    Exited,
}

impl RunStatus {
    /// The lower-case label used in listings and signals.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Complete => "complete",
            Self::Exited => "exited",
        }
    }

    /// Whether `xaft workflows resume` may continue a run in this status.
    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Paused | Self::Failed | Self::Cancelled
        )
    }

    /// Whether the run has reached an end state.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Exited)
    }
}

/// The result of one phase run — agenthicc's `PhaseOutput`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseOutput {
    /// Phase name.
    pub phase_name: String,
    /// Agent role that ran it.
    pub role: String,
    /// The phase's text result: a transition summary, or the agent's text.
    pub full_text: String,
    /// Parsed output (schema or transition arguments).
    pub structured: Option<serde_json::Value>,
    /// `Some(true)` approved, `Some(false)` rejected, `None` no verdict.
    pub approved: Option<bool>,
    /// The transition tool that ended the phase, if any.
    pub transition: Option<String>,
    /// Engine-owned extras: command outcomes, gate errors, retry markers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Wall-clock duration.
    pub duration_s: f64,
}

/// One entry of the run's phase history — agenthicc's `PhaseRunRecord`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseRunRecord {
    /// Phase name.
    pub phase_name: String,
    /// Agent role.
    pub role: String,
    /// The verdict.
    pub approved: Option<bool>,
    /// First 200 characters of the output.
    pub output_summary: String,
    /// Which entry of the phase this was (1-based).
    pub iteration: u32,
    /// Wall-clock duration.
    pub duration_s: f64,
}

/// Everything needed to resume a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunState {
    /// Durable run id (also the checkpoint file stem).
    pub run_id: String,
    /// Workflow name.
    pub workflow_name: String,
    /// The user's intent.
    pub intent: String,
    /// The session that owns the run.
    pub session_id: String,
    /// The working directory the run operates in.
    pub working_dir: PathBuf,
    /// Conversation id phases share, for re-attaching history on resume.
    pub conversation_id: String,
    /// Lifecycle status.
    pub status: RunStatus,
    /// The phase currently (or last) running.
    pub current_phase: Option<String>,
    /// Position of `current_phase` in the definition.
    pub current_phase_index: usize,
    /// The edge chosen after the last completed phase. `Some` means "resume
    /// here"; it is persisted before the next phase is entered so a crash
    /// resumes the chosen edge rather than replaying or guessing.
    pub next_phase: Option<String>,
    /// The latest output of every phase that has completed, keyed by name.
    pub phase_outputs: BTreeMap<String, PhaseOutput>,
    /// Phase names in the order their latest outputs were recorded.
    pub output_order: Vec<String>,
    /// Every phase run, in order.
    pub phase_history: Vec<PhaseRunRecord>,
    /// How many times each phase has been entered.
    pub phase_iterations: BTreeMap<String, u32>,
    /// The workflow's own key/value state: goals, artifacts, drafts, …
    pub data: BTreeMap<String, serde_json::Value>,
    /// Failure reason, when `status` is `failed`.
    pub error: Option<String>,
    /// The run's final message (the last phase's text, or the exit reason).
    pub final_text: Option<String>,
    /// Fingerprint of the definition the run started with.
    pub definition_fingerprint: String,
    /// When the run started.
    pub created_at: DateTime<Utc>,
    /// When the checkpoint was last written.
    pub updated_at: DateTime<Utc>,
}

impl WorkflowRunState {
    /// A fresh run in `Running` status.
    pub fn new(
        run_id: impl Into<String>,
        workflow_name: impl Into<String>,
        intent: impl Into<String>,
        session_id: impl Into<String>,
        working_dir: impl Into<PathBuf>,
        definition_fingerprint: impl Into<String>,
    ) -> Self {
        let run_id = run_id.into();
        let session_id = session_id.into();
        let now = Utc::now();
        Self {
            conversation_id: format!("{session_id}::workflow::{run_id}"),
            run_id,
            workflow_name: workflow_name.into(),
            intent: intent.into(),
            session_id,
            working_dir: working_dir.into(),
            status: RunStatus::Running,
            current_phase: None,
            current_phase_index: 0,
            next_phase: None,
            phase_outputs: BTreeMap::new(),
            output_order: Vec::new(),
            phase_history: Vec::new(),
            phase_iterations: BTreeMap::new(),
            data: BTreeMap::new(),
            error: None,
            final_text: None,
            definition_fingerprint: definition_fingerprint.into(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Record a phase's output as its latest.
    pub fn add_output(&mut self, output: PhaseOutput) {
        self.output_order.retain(|n| n != &output.phase_name);
        self.output_order.push(output.phase_name.clone());
        self.phase_outputs.insert(output.phase_name.clone(), output);
    }

    /// The most recently recorded output.
    pub fn last_output(&self) -> Option<&PhaseOutput> {
        self.output_order
            .last()
            .and_then(|n| self.phase_outputs.get(n))
    }

    /// agenthicc's `WorkflowContext.as_system_block()`.
    pub fn context_block(&self) -> String {
        if self.output_order.is_empty() {
            return format!("[WORKFLOW CONTEXT]\nOriginal intent: {}", self.intent);
        }
        let mut lines = vec![
            "[WORKFLOW CONTEXT]".to_string(),
            format!("Original intent: {}", self.intent),
            String::new(),
            "Completed phases:".to_string(),
        ];
        for name in &self.output_order {
            if let Some(out) = self.phase_outputs.get(name) {
                lines.push(format!(
                    "- {} ({}): {}",
                    name,
                    out.role,
                    snippet(&out.full_text, 200)
                ));
            }
        }
        lines.join("\n")
    }
}

/// The first `max` characters of `text`, with `...` when truncated.
pub fn snippet(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

/// Stable fingerprint of a definition, so a resume can warn when the
/// workflow changed underneath a checkpoint.
pub fn definition_fingerprint(def: &xaft_config::types::WorkflowDefinition) -> String {
    use std::hash::{Hash, Hasher};
    // JSON, not TOML: TOML refuses a flattened plain value after a table, and a
    // failed serialisation must never collapse every fingerprint to one value.
    let text = serde_json::to_string(def).unwrap_or_else(|_| format!("{def:?}"));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Where a working directory's workflow checkpoints live.
pub fn checkpoint_dir(working_dir: &Path) -> PathBuf {
    working_dir.join(".xaft").join("workflow-runs")
}

/// File-backed checkpoint store: one JSON file per run.
#[derive(Debug, Clone)]
pub struct CheckpointStore {
    dir: PathBuf,
}

impl CheckpointStore {
    /// A store rooted at `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store for a working directory's `.xaft/workflow-runs/`.
    pub fn for_working_dir(working_dir: &Path) -> Self {
        Self::new(checkpoint_dir(working_dir))
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, run_id: &str) -> PathBuf {
        self.dir.join(format!("{run_id}.json"))
    }

    /// Write `state` atomically, stamping `updated_at`.
    pub fn save(&self, state: &mut WorkflowRunState) -> std::io::Result<()> {
        if !is_safe_run_id(&state.run_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsafe workflow run id '{}'", state.run_id),
            ));
        }
        std::fs::create_dir_all(&self.dir)?;
        state.updated_at = Utc::now();
        let json = serde_json::to_vec_pretty(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let final_path = self.path_for(&state.run_id);
        let tmp = self
            .dir
            .join(format!(".{}.{}.tmp", state.run_id, std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &final_path)
    }

    /// Load one run by id. Accepts a unique id prefix.
    pub fn load(&self, run_id: &str) -> std::io::Result<WorkflowRunState> {
        self.load_with_path(run_id).map(|(state, _)| state)
    }

    /// Load one run and the file it came from.
    ///
    /// A checkpoint whose *embedded* run id is unsafe, or does not match its
    /// file name, is rejected: the id is used to build paths later (`save`,
    /// `remove`), so a crafted file must never steer them outside the store.
    fn load_with_path(&self, run_id: &str) -> std::io::Result<(WorkflowRunState, PathBuf)> {
        let exact = self.path_for(run_id);
        if is_safe_run_id(run_id) && exact.is_file() {
            let state = read_checked(&exact)?;
            return Ok((state, exact));
        }
        let matches: Vec<(WorkflowRunState, PathBuf)> = self
            .list_with_paths()?
            .into_iter()
            .filter(|(s, _)| s.run_id.starts_with(run_id))
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().expect("one match")),
            0 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no workflow run '{run_id}' in {}", self.dir.display()),
            )),
            n => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("workflow run prefix '{run_id}' is ambiguous ({n} matches)"),
            )),
        }
    }

    /// Every run, most recently updated first. Unreadable or unsafe files
    /// are skipped.
    pub fn list(&self) -> std::io::Result<Vec<WorkflowRunState>> {
        Ok(self
            .list_with_paths()?
            .into_iter()
            .map(|(s, _)| s)
            .collect())
    }

    fn list_with_paths(&self) -> std::io::Result<Vec<(WorkflowRunState, PathBuf)>> {
        let mut runs = Vec::new();
        let read = match std::fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(runs),
            Err(e) => return Err(e),
        };
        for entry in read.flatten() {
            let path = entry.path();
            let is_json = path.extension().and_then(|e| e.to_str()) == Some("json");
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if !is_json || hidden {
                continue;
            }
            if let Ok(state) = read_checked(&path) {
                runs.push((state, path));
            }
        }
        runs.sort_by(|a, b| b.0.updated_at.cmp(&a.0.updated_at));
        Ok(runs)
    }

    /// The most recently updated resumable run, optionally for one workflow.
    pub fn latest_resumable(
        &self,
        workflow_name: Option<&str>,
    ) -> std::io::Result<Option<WorkflowRunState>> {
        Ok(self.list()?.into_iter().find(|s| {
            s.status.is_resumable() && workflow_name.is_none_or(|w| s.workflow_name == w)
        }))
    }

    /// Delete one run's checkpoint (agenthicc's `/workflow reset <run-id>`).
    ///
    /// Deletes the file that was actually loaded — never a path rebuilt from
    /// the file's contents.
    pub fn remove(&self, run_id: &str) -> std::io::Result<()> {
        let (_, path) = self.load_with_path(run_id)?;
        std::fs::remove_file(path)
    }
}

/// Parse a checkpoint and check its embedded run id against its file name.
fn read_checked(path: &Path) -> std::io::Result<WorkflowRunState> {
    let bytes = std::fs::read(path)?;
    let state: WorkflowRunState = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !is_safe_run_id(&state.run_id) || state.run_id != stem {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "checkpoint {} declares run id '{}', which does not match its file",
                path.display(),
                state.run_id
            ),
        ));
    }
    Ok(state)
}

/// A run id that is safe to use as a file stem.
pub fn is_safe_run_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && run_id.len() <= 128
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: &str) -> WorkflowRunState {
        WorkflowRunState::new(id, "flow", "do it", "sess", "/tmp/wd", "fp")
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mut s = state("run-1");
        s.add_output(PhaseOutput {
            phase_name: "plan".into(),
            role: "planner".into(),
            full_text: "the plan".into(),
            approved: Some(true),
            ..Default::default()
        });
        s.next_phase = Some("execute".into());
        store.save(&mut s).unwrap();
        let loaded = store.load("run-1").unwrap();
        assert_eq!(loaded, s);
        assert_eq!(loaded.conversation_id, "sess::workflow::run-1");
    }

    #[test]
    fn a_unique_prefix_loads_and_an_ambiguous_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        store.save(&mut state("abc111")).unwrap();
        store.save(&mut state("abc222")).unwrap();
        assert_eq!(store.load("abc1").unwrap().run_id, "abc111");
        assert_eq!(
            store.load("abc").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            store.load("zzz").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn list_is_newest_first_and_latest_resumable_skips_finished_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        let mut old = state("old");
        old.status = RunStatus::Failed;
        store.save(&mut old).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut done = state("done");
        done.status = RunStatus::Complete;
        store.save(&mut done).unwrap();
        let ids: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|s| s.run_id)
            .collect();
        assert_eq!(ids, ["done", "old"]);
        assert_eq!(store.latest_resumable(None).unwrap().unwrap().run_id, "old");
        assert!(store.latest_resumable(Some("other")).unwrap().is_none());
    }

    #[test]
    fn unsafe_run_ids_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        assert!(store.save(&mut state("../escape")).is_err());
        assert!(!is_safe_run_id("a/b"));
        assert!(is_safe_run_id("2f9c-run_1"));
    }

    #[test]
    fn a_crafted_checkpoint_cannot_steer_paths_outside_the_store() {
        let outer = tempfile::tempdir().unwrap();
        let store_dir = outer.path().join("runs");
        std::fs::create_dir_all(&store_dir).unwrap();
        let victim = outer.path().join("victim.json");
        std::fs::write(&victim, "precious").unwrap();
        // A file named like a safe run whose contents claim `../victim`.
        let mut evil = state("evil");
        evil.run_id = "../victim".into();
        std::fs::write(
            store_dir.join("evil.json"),
            serde_json::to_vec(&evil).unwrap(),
        )
        .unwrap();
        let store = CheckpointStore::new(&store_dir);
        assert!(store.load("evil").is_err(), "the mismatched id is rejected");
        assert!(store.list().unwrap().is_empty());
        assert!(store.remove("evil").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    }

    #[test]
    fn context_block_matches_agenthicc_shape() {
        let mut s = state("r");
        assert_eq!(
            s.context_block(),
            "[WORKFLOW CONTEXT]\nOriginal intent: do it"
        );
        s.add_output(PhaseOutput {
            phase_name: "plan".into(),
            role: "planner".into(),
            full_text: "x".repeat(250),
            ..Default::default()
        });
        let block = s.context_block();
        assert!(block.contains("Completed phases:"));
        assert!(block.contains(&format!("- plan (planner): {}...", "x".repeat(200))));
    }

    #[test]
    fn re_adding_an_output_moves_it_to_the_end() {
        let mut s = state("r");
        for name in ["a", "b", "a"] {
            s.add_output(PhaseOutput {
                phase_name: name.into(),
                ..Default::default()
            });
        }
        assert_eq!(s.output_order, ["b", "a"]);
        assert_eq!(s.last_output().unwrap().phase_name, "a");
    }

    #[test]
    fn remove_deletes_the_checkpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path());
        store.save(&mut state("gone")).unwrap();
        store.remove("gone").unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
