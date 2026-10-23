//! Atomic durable storage for parallel orchestration manifests.
//!
//! Port of `agenthicc.worktrees.store` (226 lines). One JSON file per
//! orchestration, written through a temporary file and a rename so a crash
//! mid-write cannot truncate the previous manifest.
//!
//! # Storage layout
//!
//! `$XAFT_HOME/orchestrations/{orchestration_id}.json`, defaulting to
//! `~/.xaft/orchestrations/`. `XAFT_HOME` is the same override `jobs` and
//! `runs` already use, so all three stores can be redirected together in a
//! test.
//!
//! # Locking
//!
//! agenthicc takes an `fcntl` exclusive lock around every write. This store
//! does not: it is single-process, like `JobStore` and `RunStore`, and adding
//! an advisory-lock dependency to guard a one-shot CLI store would be a
//! dependency added for a property nothing observes. The write itself is still
//! atomic, so a concurrent reader sees either the old file or the new one.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::error::WorktreeError;
use super::model::ParallelManifest;

/// Persist one JSON manifest per orchestration.
#[derive(Debug, Clone)]
pub struct ManifestStore {
    root: PathBuf,
}

impl ManifestStore {
    /// A store rooted at `root`. The directory is created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$XAFT_HOME/orchestrations`, else `~/.xaft/orchestrations`.
    pub fn default_root() -> Result<PathBuf, WorktreeError> {
        if let Some(dir) = std::env::var_os("XAFT_HOME") {
            return Ok(PathBuf::from(dir).join("orchestrations"));
        }
        let base = dirs::home_dir().ok_or_else(|| {
            WorktreeError::Io(std::io::Error::other(
                "cannot determine home directory; set XAFT_HOME",
            ))
        })?;
        Ok(base.join(".xaft").join("orchestrations"))
    }

    /// Open the store at its default location.
    pub fn open_default() -> Result<Self, WorktreeError> {
        Ok(Self::new(Self::default_root()?))
    }

    /// The directory holding the manifests.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an orchestration id to a path inside the root.
    ///
    /// The id reaches this from a shell argument, so it is validated before it
    /// ever touches the filesystem: anything that is not a plain file name is
    /// refused, which closes `../`, absolute paths and nested paths.
    ///
    /// agenthicc's check is `Path(id).name != id`, which on POSIX rejects
    /// `/` but not `\`. This rejects `\` too. The set of legitimate ids is
    /// generated hex, so nothing real is lost, and the stricter rule is the
    /// one worth having on a path built from untrusted input.
    fn path_for(&self, orchestration_id: &str) -> Result<PathBuf, WorktreeError> {
        let safe = !orchestration_id.is_empty()
            && orchestration_id != "."
            && orchestration_id != ".."
            && orchestration_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !safe {
            return Err(WorktreeError::Invalid(
                "invalid orchestration id".to_string(),
            ));
        }
        Ok(self.root.join(format!("{orchestration_id}.json")))
    }

    /// Read one manifest.
    pub fn get(&self, orchestration_id: &str) -> Result<ParallelManifest, WorktreeError> {
        let path = self.path_for(orchestration_id)?;
        let raw = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WorktreeError::NotFound(orchestration_id.to_string()),
            _ => WorktreeError::Io(e),
        })?;
        serde_json::from_str(&raw).map_err(|e| {
            WorktreeError::Invalid(format!(
                "cannot read orchestration manifest {orchestration_id}: {e}"
            ))
        })
    }

    /// Write a new manifest, refusing to clobber an existing one.
    pub fn create(&self, manifest: ParallelManifest) -> Result<ParallelManifest, WorktreeError> {
        let path = self.path_for(&manifest.orchestration_id)?;
        if path.exists() {
            return Err(WorktreeError::Invalid(format!(
                "orchestration already exists: {}",
                manifest.orchestration_id
            )));
        }
        self.write(&path, &manifest)?;
        Ok(manifest)
    }

    /// Write a manifest, replacing any existing one.
    pub fn save(&self, manifest: ParallelManifest) -> Result<ParallelManifest, WorktreeError> {
        let path = self.path_for(&manifest.orchestration_id)?;
        self.write(&path, &manifest)?;
        Ok(manifest)
    }

    fn write(&self, path: &Path, manifest: &ParallelManifest) -> Result<(), WorktreeError> {
        std::fs::create_dir_all(&self.root)?;
        let value = to_payload(manifest)?;
        let json = serde_json::to_string_pretty(&value)?;
        // Write-then-rename, matching JobStore: a crash mid-write leaves the
        // previous manifest readable.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Every manifest, most recently updated first.
    ///
    /// Manifests that cannot be read are skipped rather than aborting the
    /// listing: one corrupt file must not hide every other orchestration.
    /// Ties break on `orchestration_id` so the order is total and stable.
    pub fn list(&self) -> Vec<ParallelManifest> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut results: Vec<ParallelManifest> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| {
                let stem = entry.path().file_stem()?.to_str()?.to_string();
                self.get(&stem).ok()
            })
            .collect();
        results.sort_by(|a, b| {
            b.updated_at
                .partial_cmp(&a.updated_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.orchestration_id.cmp(&b.orchestration_id))
        });
        results
    }

    /// Read, mutate and re-save one manifest.
    ///
    /// The mutation runs on a value the caller owns, so a failure inside it
    /// cannot leave a half-applied manifest on disk.
    pub fn update<F>(&self, orchestration_id: &str, f: F) -> Result<ParallelManifest, WorktreeError>
    where
        F: FnOnce(&mut ParallelManifest),
    {
        let mut manifest = self.get(orchestration_id)?;
        f(&mut manifest);
        manifest.touch();
        self.save(manifest)
    }
}

/// Render a manifest the way agenthicc's `--json` output does.
///
/// `worktrees list --json` and `worktrees show` both use
/// `json.dumps(payload, indent=2, sort_keys=True)`. Routing through
/// [`serde_json::Value`] gets both halves: `to_string_pretty` is a two-space
/// indent, and `serde_json::Map` is a `BTreeMap` because serde_json is built
/// without `preserve_order`, so keys come out sorted.
pub(crate) fn to_payload<T: serde::Serialize>(value: &T) -> Result<Value, WorktreeError> {
    Ok(serde_json::to_value(value)?)
}

/// Render a payload exactly as the `worktrees` commands print it.
pub(crate) fn render<T: serde::Serialize>(value: &T) -> Result<String, WorktreeError> {
    Ok(serde_json::to_string_pretty(&to_payload(value)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrees::model::{OrchestrationStatus, ParallelTask, TaskStatus};

    fn manifest(id: &str, tasks: Vec<ParallelTask>) -> ParallelManifest {
        ParallelManifest {
            orchestration_id: id.to_string(),
            parent_session_id: "session-1".into(),
            run_id: String::new(),
            repository: "/tmp/repo".into(),
            parent_branch: "main".into(),
            base_commit: "deadbeef".into(),
            status: OrchestrationStatus::Planning,
            tasks,
            worktrees: vec![],
            created_at: 10.0,
            updated_at: 10.0,
            last_error: String::new(),
            verification: vec![],
        }
    }

    fn task(id: &str) -> ParallelTask {
        ParallelTask {
            task_id: id.into(),
            description: format!("do {id}"),
            dependencies: vec![],
            status: TaskStatus::Pending,
            worker_session_id: String::new(),
            worktree_id: String::new(),
            result: None,
            error: String::new(),
            created_at: 10.0,
            updated_at: 10.0,
        }
    }

    #[test]
    fn an_empty_store_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path().join("orchestrations"));
        assert!(store.list().is_empty());
        assert!(!store.root().exists());
    }

    #[test]
    fn create_get_and_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        let created = store.create(manifest("o1", vec![task("t1")])).unwrap();
        assert_eq!(created.orchestration_id, "o1");

        let loaded = store.get("o1").unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.parent_session_id, "session-1");
        assert_eq!(loaded.status, OrchestrationStatus::Planning);

        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].orchestration_id, "o1");
    }

    #[test]
    fn creating_the_same_orchestration_twice_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        store.create(manifest("o1", vec![])).unwrap();
        let err = store.create(manifest("o1", vec![])).unwrap_err();
        assert!(matches!(err, WorktreeError::Invalid(_)));
        assert!(err.to_string().contains("orchestration already exists: o1"));
    }

    #[test]
    fn a_missing_orchestration_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        assert!(matches!(
            store.get("nope").unwrap_err(),
            WorktreeError::NotFound(id) if id == "nope"
        ));
    }

    #[test]
    fn orchestration_ids_that_escape_the_root_are_refused() {
        // The root is nested inside a directory this test owns, so the escape
        // target below is private to it: asserting on the shared temp dir's
        // parent would fail whenever any other process left an
        // `escape.json` there.
        let outer = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir_in(outer.path()).unwrap();
        let store = ManifestStore::new(dir.path());
        for bad in ["../escape", "a/b", "..", ".", "", "a\\b", "/abs"] {
            assert!(
                matches!(store.get(bad), Err(WorktreeError::Invalid(_))),
                "id {bad:?} should have been refused"
            );
        }
        assert!(!dir.path().parent().unwrap().join("escape.json").exists());
    }

    #[test]
    fn list_is_ordered_by_update_time_then_id_and_skips_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        let mut older = manifest("aaa", vec![]);
        older.updated_at = 1.0;
        let mut newer = manifest("bbb", vec![]);
        newer.updated_at = 9.0;
        let mut same_time_first = manifest("zzz", vec![]);
        same_time_first.updated_at = 9.0;
        store.create(older).unwrap();
        store.create(newer).unwrap();
        store.create(same_time_first).unwrap();

        std::fs::write(dir.path().join("corrupt.json"), "{not json").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "nope").unwrap();

        let ids: Vec<_> = store
            .list()
            .into_iter()
            .map(|m| m.orchestration_id)
            .collect();
        assert_eq!(ids, vec!["bbb", "zzz", "aaa"]);
    }

    #[test]
    fn update_applies_and_stamps_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        store.create(manifest("o1", vec![task("t1")])).unwrap();
        let saved = store
            .update("o1", |m| {
                m.status = OrchestrationStatus::Running;
                m.tasks[0].status = TaskStatus::Running;
            })
            .unwrap();
        assert_eq!(saved.status, OrchestrationStatus::Running);
        assert!(saved.updated_at > 10.0);
        assert_eq!(
            store.get("o1").unwrap().status,
            OrchestrationStatus::Running
        );
    }

    #[test]
    fn update_on_a_missing_orchestration_is_not_found_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        assert!(matches!(
            store.update("nope", |_| {}).unwrap_err(),
            WorktreeError::NotFound(_)
        ));
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ManifestStore::new(dir.path());
        store.create(manifest("o1", vec![])).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left {leftovers:?} behind");
    }

    #[test]
    fn render_matches_agenthicc_indent_and_sorted_keys() {
        let rendered = render(&manifest("o1", vec![])).unwrap();
        assert!(rendered.starts_with("{\n  \"base_commit\": \"deadbeef\","));
        assert!(rendered.ends_with("\n}"));
        let keys: Vec<&str> = rendered
            .lines()
            .filter(|l| l.starts_with("  \""))
            .map(|l| l.trim().split('"').nth(1).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "top-level keys must be sorted");
    }
}
