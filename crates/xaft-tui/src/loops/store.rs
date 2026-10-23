//! Atomic, owner-only persistence for one session's loop record.
//!
//! Layout: `<root>/<session_id>/loop.json`, where `<root>` is
//! `core.data_dir/loops`. agenthicc keeps the record inside the session's own
//! directory (`~/.agenthicc/sessions/<id>/loop.json`); xaft keeps sessions in
//! SQLite, so loops get a sibling directory of their own under the same data
//! dir. `/loops` is a projection over every session directory there, not a
//! second registry.
//!
//! # Writes
//!
//! Each save writes a temporary file in the session directory and renames it
//! over `loop.json`, so a crash mid-write leaves either the old record or the
//! new one, never a torn file. The directory is `0700` and the file `0600` on
//! Unix — a loop payload is a prompt the user typed and may name internal
//! systems. agenthicc additionally takes an `fcntl` lock around each write;
//! that guards concurrent *writers*, and the one-owner rule below already makes
//! a second writer for the same session a refused operation rather than a race
//! to arbitrate.
//!
//! # The owner marker
//!
//! agenthicc refuses to run or delete another session's loop while that
//! session is live, using its session-lease inspection. xaft has no session
//! lease, so the live owner writes `owner.json` (its PID) next to the record
//! while it holds the session, and removes it on shutdown. A record whose
//! marker names a running process belongs to a live session. A stale marker
//! (the process is gone) does not. PID reuse can only make a dead owner look
//! alive, which errs toward refusing — the safe direction.

use std::path::{Path, PathBuf};

use super::model::{LOOP_SCHEMA_VERSION, LoopRecord};

/// Raised when a loop record cannot be read or written safely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LoopStorageError(pub String);

/// Whether `id` is a plain path component (no separators, no `..`).
///
/// Session ids reach the store from records on disk and from `/loops`
/// selections, so they are validated before they touch a path.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// One session's loop store.
#[derive(Debug, Clone)]
pub struct LoopStore {
    root: PathBuf,
    session_id: String,
}

impl LoopStore {
    /// A store for `session_id` under `root`.
    pub fn new(root: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            session_id: session_id.into(),
        }
    }

    /// The directory holding every session's loop.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The session this store belongs to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// `<root>/<session_id>`.
    pub fn session_dir(&self) -> PathBuf {
        self.root.join(&self.session_id)
    }

    /// `<root>/<session_id>/loop.json`.
    pub fn path(&self) -> PathBuf {
        self.session_dir().join("loop.json")
    }

    fn owner_path(&self) -> PathBuf {
        self.session_dir().join("owner.json")
    }

    fn check_id(&self) -> Result<(), LoopStorageError> {
        if is_safe_session_id(&self.session_id) {
            Ok(())
        } else {
            Err(LoopStorageError(format!(
                "invalid loop session id {:?}",
                self.session_id
            )))
        }
    }

    /// Read this session's record, `None` when there is none.
    pub fn load(&self) -> Result<Option<LoopRecord>, LoopStorageError> {
        self.check_id()?;
        let path = self.path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(LoopStorageError(format!(
                    "cannot read loop record {}: {e}",
                    path.display()
                )));
            }
        };
        let record: LoopRecord = serde_json::from_str(&raw).map_err(|e| {
            LoopStorageError(format!("invalid loop record {}: {e}", path.display()))
        })?;
        record.validate().map_err(|e| {
            LoopStorageError(format!("invalid loop record {}: {e}", path.display()))
        })?;
        if record.session_id != self.session_id || record.conversation_id != self.session_id {
            return Err(LoopStorageError(
                "loop record identity does not match its session".into(),
            ));
        }
        Ok(Some(record))
    }

    /// Atomically replace this session's record.
    pub fn save(&self, record: &LoopRecord) -> Result<(), LoopStorageError> {
        self.check_id()?;
        if record.session_id != self.session_id || record.conversation_id != self.session_id {
            return Err(LoopStorageError(
                "cannot save a loop for a different session".into(),
            ));
        }
        if record.schema_version != LOOP_SCHEMA_VERSION {
            return Err(LoopStorageError(
                "cannot save an unsupported loop schema".into(),
            ));
        }
        let dir = self.session_dir();
        let fail = |e: std::io::Error| {
            LoopStorageError(format!(
                "cannot persist loop record {}: {e}",
                self.path().display()
            ))
        };
        std::fs::create_dir_all(&dir).map_err(fail)?;
        restrict(&dir, 0o700);
        // Sorted keys and a trailing newline — agenthicc's
        // `json.dumps(..., sort_keys=True, indent=2) + "\n"`.
        let value = serde_json::to_value(record).map_err(|e| LoopStorageError(e.to_string()))?;
        let mut encoded =
            serde_json::to_string_pretty(&value).map_err(|e| LoopStorageError(e.to_string()))?;
        encoded.push('\n');
        let tmp = dir.join(format!(
            ".loop-{}-{}.tmp",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::write(&tmp, encoded).map_err(fail)?;
        restrict(&tmp, 0o600);
        if let Err(e) = std::fs::rename(&tmp, self.path()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(fail(e));
        }
        Ok(())
    }

    /// Delete this session's record, optionally only if it still has
    /// `expected_loop_id`. Returns the deleted record.
    pub fn delete(
        &self,
        expected_loop_id: Option<&str>,
    ) -> Result<Option<LoopRecord>, LoopStorageError> {
        let Some(current) = self.load()? else {
            return Ok(None);
        };
        if let Some(expected) = expected_loop_id {
            if current.loop_id != expected {
                return Err(LoopStorageError(
                    "loop changed before it could be deleted".into(),
                ));
            }
        }
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(Some(current)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LoopStorageError(format!(
                "cannot delete loop record {}: {e}",
                self.path().display()
            ))),
        }
    }

    /// Every valid loop record below `root`, most recently updated first.
    ///
    /// A missing, corrupt or unreadable record is skipped rather than failing
    /// the whole listing — one bad session must not hide every healthy job
    /// (agenthicc's `LoopStore.list_all`).
    pub fn list_all(root: &Path) -> Vec<LoopRecord> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type()
                    .map(|t| t.is_dir() && !t.is_symlink())
                    .unwrap_or(false)
            })
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        let mut records: Vec<LoopRecord> = names
            .into_iter()
            .filter_map(|name| LoopStore::new(root, name).load().ok().flatten())
            .collect();
        records.sort_by(|a, b| {
            b.updated_at
                .total_cmp(&a.updated_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
                .then_with(|| a.loop_id.cmp(&b.loop_id))
        });
        records
    }

    /// Record this process as the session's live owner.
    pub fn claim_owner(&self) -> Result<(), LoopStorageError> {
        self.check_id()?;
        let dir = self.session_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| LoopStorageError(format!("cannot claim loop session: {e}")))?;
        restrict(&dir, 0o700);
        let body = serde_json::json!({
            "pid": std::process::id(),
            "session_id": self.session_id,
        });
        std::fs::write(self.owner_path(), format!("{body}\n"))
            .map_err(|e| LoopStorageError(format!("cannot claim loop session: {e}")))?;
        restrict(&self.owner_path(), 0o600);
        Ok(())
    }

    /// Drop this process's owner marker, if it is still ours.
    pub fn release_owner(&self) {
        if self.owner_pid() == Some(std::process::id()) {
            let _ = std::fs::remove_file(self.owner_path());
        }
    }

    fn owner_pid(&self) -> Option<u32> {
        if !is_safe_session_id(&self.session_id) {
            return None;
        }
        let raw = std::fs::read_to_string(self.owner_path()).ok()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        value
            .get("pid")?
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
    }

    /// Whether a live process (other than this one) owns the session.
    pub fn owned_by_another_live_process(&self) -> bool {
        match self.owner_pid() {
            Some(pid) if pid != std::process::id() => process_is_alive(pid),
            _ => false,
        }
    }
}

/// Whether `pid` names a running process.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission check; it
    // delivers nothing. EPERM still means "exists, but not ours to signal".
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a portable existence check, assume a marker is live — refusing a
/// foreign session's loop is the safe direction.
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::model::{LoopLifecycle, LoopPayloadKind};

    fn record(session: &str, loop_id: &str, updated_at: f64) -> LoopRecord {
        LoopRecord {
            schema_version: 1,
            loop_id: loop_id.into(),
            session_id: session.into(),
            conversation_id: session.into(),
            payload_kind: LoopPayloadKind::Prompt,
            payload: format!("inspect {loop_id}"),
            interval_s: 60,
            created_at: 1.0,
            updated_at,
            next_due_at: 100.0,
            expires_at: 1_000.0,
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
        }
    }

    #[test]
    fn a_record_round_trips_and_its_identity_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let store = LoopStore::new(dir.path(), "session-1");
        let r = record("session-1", "loop-1", 2.0);
        store.save(&r).unwrap();
        assert_eq!(store.load().unwrap(), Some(r.clone()));
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains("\"conversation_id\""));

        std::fs::write(store.path(), r#"{"session_id": "other"}"#).unwrap();
        assert!(store.load().is_err(), "a malformed record is refused");

        let foreign = record("session-2", "loop-x", 1.0);
        assert!(
            store.save(&foreign).is_err(),
            "a record for another session"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_session_directory_and_record_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = LoopStore::new(dir.path(), "session-1");
        store.save(&record("session-1", "loop-1", 1.0)).unwrap();
        let dir_mode = std::fs::metadata(store.session_dir())
            .unwrap()
            .permissions()
            .mode();
        let file_mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert_eq!(file_mode & 0o777, 0o600);
    }

    #[test]
    fn list_all_projects_every_valid_session_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        LoopStore::new(dir.path(), "session-1")
            .save(&record("session-1", "loop-1", 2.0))
            .unwrap();
        LoopStore::new(dir.path(), "session-2")
            .save(&record("session-2", "loop-2", 3.0))
            .unwrap();
        // A corrupt session must not hide the healthy ones.
        std::fs::create_dir_all(dir.path().join("broken")).unwrap();
        std::fs::write(dir.path().join("broken").join("loop.json"), "{not json").unwrap();

        let ids: Vec<String> = LoopStore::list_all(dir.path())
            .into_iter()
            .map(|r| r.loop_id)
            .collect();
        assert_eq!(ids, vec!["loop-2".to_string(), "loop-1".to_string()]);
        assert!(LoopStore::list_all(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn delete_honours_its_precondition() {
        let dir = tempfile::tempdir().unwrap();
        let store = LoopStore::new(dir.path(), "session-1");
        store.save(&record("session-1", "loop-1", 1.0)).unwrap();
        assert!(store.delete(Some("loop-other")).is_err());
        assert!(
            store.load().unwrap().is_some(),
            "a failed precondition deletes nothing"
        );
        let deleted = store.delete(Some("loop-1")).unwrap();
        assert_eq!(deleted.map(|r| r.loop_id), Some("loop-1".to_string()));
        assert_eq!(store.load().unwrap(), None);
        assert_eq!(store.delete(None).unwrap(), None);
    }

    #[test]
    fn unsafe_session_ids_never_reach_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "..", "a/b", "../escape"] {
            let store = LoopStore::new(dir.path(), bad);
            assert!(store.load().is_err(), "{bad:?}");
            assert!(store.claim_owner().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_owner_marker_distinguishes_this_process_from_a_live_other() {
        let dir = tempfile::tempdir().unwrap();
        let store = LoopStore::new(dir.path(), "session-1");
        assert!(
            !store.owned_by_another_live_process(),
            "no marker, no owner"
        );
        store.claim_owner().unwrap();
        assert!(
            !store.owned_by_another_live_process(),
            "this process owning the session is not 'another' owner"
        );
        store.release_owner();
        assert!(!store.owner_path().exists());

        // A marker naming a process that is certainly not running is stale.
        std::fs::write(store.owner_path(), r#"{"pid": 4294967294}"#).unwrap();
        assert!(!store.owned_by_another_live_process());
    }

    #[cfg(unix)]
    #[test]
    fn a_marker_naming_a_live_process_belongs_to_a_live_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = LoopStore::new(dir.path(), "session-1");
        std::fs::create_dir_all(store.session_dir()).unwrap();
        // PID 1 is always alive on Unix.
        std::fs::write(store.owner_path(), r#"{"pid": 1}"#).unwrap();
        assert!(store.owned_by_another_live_process());
    }
}
